use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use omsh_ai::fake::FakeProvider;
use omsh_ai::{ChatRequest, ErrorKind, ProviderError};
use omsh_config::Provider as Kind;
use omsh_history::{Filter, Query, Store};
use omsh_ui::Paint;

use super::*;

/// Limits whose time budget cannot run out, so a busy machine cannot change a count.
const TEST_LIMITS: BlastLimits = BlastLimits {
    max_entries: 50_000,
    max_ms: 600_000,
};

// -- test doubles ---------------------------------------------------------------------------

#[derive(Default)]
struct State {
    keys: VecDeque<Result<Key, ReadError>>,
    lines: VecDeque<Result<String, ReadError>>,
    err: String,
    key_prompts: Vec<String>,
    line_prompts: Vec<String>,
}

/// Terminals that answer from a script and keep what was printed.
#[derive(Default)]
struct Script(RefCell<State>);

struct Term<'a>(&'a RefCell<State>);

impl TerminalSource for Script {
    fn open<'a>(&'a mut self, _shell: &'a Shell, _files: &'a Io) -> Box<dyn Terminal + 'a> {
        Box::new(Term(&self.0))
    }
}

impl Terminal for Term<'_> {
    fn input_is_terminal(&self) -> bool {
        true
    }
    fn out(&mut self, _text: &str) {}
    fn err(&mut self, text: &str) {
        self.0.borrow_mut().err.push_str(text);
    }
    fn read_line(
        &mut self,
        prompt: &str,
        _hidden: bool,
        line: &mut Vec<u8>,
    ) -> Result<(), ReadError> {
        let mut state = self.0.borrow_mut();
        state.line_prompts.push(prompt.to_owned());
        line.clear();
        match state.lines.pop_front() {
            Some(Ok(text)) => {
                line.extend_from_slice(text.as_bytes());
                Ok(())
            }
            Some(Err(error)) => Err(error),
            None => Err(ReadError::EndOfInput),
        }
    }
    fn read_key(&mut self, prompt: &str) -> Result<Key, ReadError> {
        let mut state = self.0.borrow_mut();
        state.key_prompts.push(prompt.to_owned());
        state.keys.pop_front().unwrap_or(Err(ReadError::EndOfInput))
    }
    fn interrupted(&self) -> bool {
        false
    }
}

/// A directory for the files that test steps create, removed at the end.
struct Dir(PathBuf);

impl Dir {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("omsh-plan-{}-{n}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn file(&self, name: &str) -> String {
        self.0.join(name).to_string_lossy().into_owned()
    }
    fn has(&self, name: &str) -> bool {
        self.0.join(name).exists()
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn escape(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

/// A plan as the model would send it: (title, command, risk) for each step.
fn json(steps: &[(&str, &str, &str)]) -> String {
    let steps: Vec<String> = steps
        .iter()
        .map(|(title, command, risk)| {
            format!(
                r#"{{"title":"{}","command":"{}","explanation":"Because.","risk":"{risk}","needs_root":false}}"#,
                escape(title),
                escape(command)
            )
        })
        .collect();
    format!(
        r#"{{"summary":"Do the thing","steps":[{}]}}"#,
        steps.join(",")
    )
}

fn enter() -> Result<Key, ReadError> {
    Ok(Key::Enter)
}
fn ch(c: char) -> Result<Key, ReadError> {
    Ok(Key::Char(c))
}

struct Ran {
    result: LineResult,
    err: String,
    shell: Shell,
    requests: Vec<ChatRequest>,
    key_prompts: Vec<String>,
    line_prompts: Vec<String>,
    /// What the shell remembered as recent commands when the flow ended.
    recent: Vec<omsh_ai::RecentCommand>,
    /// How many providers were made (one for each request), and how many were still alive
    /// when the flow ended (none may be).
    connects: u32,
    alive: usize,
}

impl Ran {
    fn status(&self) -> u8 {
        match &self.result {
            LineResult::Ran(outcome) => outcome.status().0,
            other => panic!("not run: {other:?}"),
        }
    }
}

#[derive(Default)]
struct Setup {
    keys: Vec<Result<Key, ReadError>>,
    lines: Vec<Result<String, ReadError>>,
    edits: Vec<Edited>,
    aliases: Vec<&'static str>,
    history: Option<History>,
    context: Option<AiContext>,
    /// Commands the shell already ran before the request: (line, status).
    recent: Vec<(&'static str, i32)>,
    /// `[safety] confirm`; Dangerous when not set.
    confirm: Option<ConfirmLevel>,
    /// The limits of the blast-radius count; the defaults when not set.
    blast: Option<BlastLimits>,
    /// The trash and the journal; none when not set (as under `--no-config`).
    undo: Option<Arc<Undo>>,
    /// What the shell's "did you mean" offers for any name; no suggester when empty.
    suggest: Vec<&'static str>,
    /// Desktop entries of a made-up machine: (file name, text). `{bin}` in the text is the folder
    /// of `programs`. Without any, the shell has no entry folder that exists.
    apps: Vec<(&'static str, String)>,
    /// Programs the made-up machine has, and the whole of its `PATH` (links to `/bin/true`). Without
    /// any, the shell keeps its default `PATH`.
    programs: Vec<&'static str>,
    /// Programs of the made-up machine that are **not** in `PATH` (links to `/bin/true`); `{opt}`
    /// in an entry's text is their folder.
    outside: Vec<&'static str>,
}

/// A suggester that offers the same names for every name.
struct FakeSuggest(Vec<String>);

impl omsh_exec::CommandSuggest for FakeSuggest {
    fn suggest(&self, _name: &str, _path: &[u8], _shell_names: &[String]) -> Vec<String> {
        self.0.clone()
    }
}

fn go(fake: FakeProvider, request: &str, setup: Setup) -> Ran {
    let fake = Arc::new(fake);
    let mut shell = Shell::new();
    let mut files = Io::process();
    if !setup.suggest.is_empty() {
        let names = setup
            .suggest
            .iter()
            .map(|name| (*name).to_owned())
            .collect();
        shell.set_suggest(Arc::new(FakeSuggest(names)));
    }
    // The shell never looks at the real desktop entries of the machine that runs the tests.
    let machine = Dir::new();
    let (share, bin) = (machine.file("share/applications"), machine.file("bin"));
    let opt = machine.file("opt");
    fs::create_dir_all(&share).unwrap();
    fs::create_dir_all(&bin).unwrap();
    for (name, text) in &setup.apps {
        let text = text.replace("{bin}", &bin).replace("{opt}", &opt);
        fs::write(format!("{share}/{name}"), text).unwrap();
    }
    fs::create_dir_all(&opt).unwrap();
    for name in &setup.outside {
        std::os::unix::fs::symlink("/bin/true", format!("{opt}/{name}")).unwrap();
    }
    for name in &setup.programs {
        std::os::unix::fs::symlink("/bin/true", format!("{bin}/{name}")).unwrap();
    }
    let mut vars = format!(
        "XDG_DATA_HOME='{}' XDG_DATA_DIRS='{}'",
        machine.file("none"),
        machine.file("share")
    );
    if !setup.programs.is_empty() {
        // Only the made-up programs: the machine that runs the tests may have the real ones.
        vars.push_str(&format!(" PATH='{bin}'"));
    }
    shell.run_source(&vars, true, &mut files);
    if let Some(undo) = &setup.undo {
        shell.set_journal(Arc::clone(undo) as Arc<dyn omsh_exec::JournalCommand>);
    }
    for alias in &setup.aliases {
        shell.run_source(alias, true, &mut files);
    }
    let mut tty = Script(RefCell::new(State {
        keys: setup.keys.into(),
        lines: setup.lines.into(),
        ..State::default()
    }));
    let mut recent = Recent::new(Filter::new(&[" *".to_owned()]));
    for (line, status) in &setup.recent {
        recent.record(line, *status);
    }
    let mut edits: VecDeque<Edited> = setup.edits.into();
    let mut edit = |_: &str| edits.pop_front().unwrap_or(Edited::Cancelled);
    let connects = Arc::new(AtomicU32::new(0));
    let plan_setup = PlanSetup {
        connect: Box::new({
            let fake = Arc::clone(&fake);
            let connects = Arc::clone(&connects);
            move || {
                connects.fetch_add(1, Ordering::Relaxed);
                Ok(Box::new(Arc::clone(&fake)) as Box<dyn Provider>)
            }
        }),
        settings: Settings {
            kind: Kind::Ollama,
            model: "test-model".to_owned(),
            base_url: None,
        },
        context: setup.context.unwrap_or_default(),
        calls: Arc::new(crate::ai::usage::Calls::standard()),
        menu: None,
    };
    let result = run(
        plan_setup,
        request,
        PlanEnv {
            shell: &mut shell,
            files: &mut files,
            tty: &mut tty,
            history: setup.history.as_ref(),
            recent: &mut recent,
            confirm: setup.confirm.unwrap_or(ConfirmLevel::Dangerous),
            blast: setup.blast.unwrap_or(TEST_LIMITS),
            paint: &Paint::plain(),
            undo: setup.undo.as_deref(),
            edit: &mut edit,
            menu: None,
        },
    );
    let state = tty.0.into_inner();
    Ran {
        result,
        err: state.err,
        shell,
        requests: fake.requests(),
        key_prompts: state.key_prompts,
        line_prompts: state.line_prompts,
        recent: recent.items(),
        connects: connects.load(Ordering::Relaxed),
        // The test's own handle is one; every provider that was made and not dropped adds one.
        alive: Arc::strong_count(&fake) - 1,
    }
}

fn says(text: &str) -> FakeProvider {
    FakeProvider::new(Kind::Ollama).says(&[text])
}

// -- the one rule: nothing runs without a key ------------------------------------------------

#[test]
fn a_plan_of_one_harmless_step_still_waits_for_enter() {
    let dir = Dir::new();
    let command = format!("echo hi > {}", dir.file("marker"));
    let answer = json(&[("Say hi", &command, "low")]);
    // Esc: nothing ran, although the step is as safe as can be.
    let ran = go(
        says(&answer),
        "say hi",
        Setup {
            keys: vec![Ok(Key::Escape)],
            ..Setup::default()
        },
    );
    assert!(!dir.has("marker"));
    assert_eq!(ran.status(), 1);
    assert!(ran.err.contains("cancelled; nothing ran"), "{}", ran.err);
    assert_eq!(
        ran.key_prompts,
        ["[Enter] run  [e] edit  [c] chat  [v] view context  [Esc] cancel "]
    );
    // Enter: it ran.
    let ran = go(
        says(&answer),
        "say hi",
        Setup {
            keys: vec![enter()],
            ..Setup::default()
        },
    );
    assert!(dir.has("marker"));
    assert_eq!(ran.status(), 0);
    assert!(ran.err.contains("plan done (1 step)"), "{}", ran.err);
}

#[test]
fn every_way_of_not_pressing_enter_runs_nothing() {
    let dir = Dir::new();
    let answer = json(&[("Touch", &format!("echo x > {}", dir.file("m")), "low")]);
    let cases: Vec<(Vec<Result<Key, ReadError>>, u8)> = vec![
        (vec![Ok(Key::Escape)], 1),
        (vec![ch('n')], 1),
        (vec![ch('N')], 1),
        (vec![ch('q')], 1),
        (vec![Err(ReadError::Interrupted)], 130),
        (vec![Err(ReadError::EndOfInput)], 1),
        (vec![Err(ReadError::Failed)], 1),
        (vec![], 1),
    ];
    for (keys, status) in cases {
        let ran = go(
            says(&answer),
            "x",
            Setup {
                keys,
                ..Setup::default()
            },
        );
        assert!(!dir.has("m"));
        assert_eq!(ran.status(), status);
        assert_eq!(ran.shell.last_status().0, status);
    }
}

#[test]
fn other_keys_are_ignored_and_the_question_stays() {
    let dir = Dir::new();
    let answer = json(&[("Touch", &format!("echo x > {}", dir.file("m")), "low")]);
    let ran = go(
        says(&answer),
        "x",
        Setup {
            keys: vec![ch('x'), ch(' '), ch('z'), enter()],
            ..Setup::default()
        },
    );
    assert!(dir.has("m"));
    assert_eq!(ran.status(), 0);
    assert_eq!(ran.key_prompts.len(), 4);
}

// -- levels ---------------------------------------------------------------------------------

#[test]
fn the_analyzers_level_beats_the_models_low() {
    let answer = json(&[("Wipe", "shred /nonexistent-omsh-plan-test", "low")]);
    // Enter on the panel is not enough for a critical step; "no" cancels.
    let ran = go(
        says(&answer),
        "x",
        Setup {
            keys: vec![enter()],
            lines: vec![Ok("no".to_owned())],
            ..Setup::default()
        },
    );
    assert!(ran.err.contains("[critical]"), "{}", ran.err);
    assert!(!ran.err.contains("the model's estimate"), "{}", ran.err);
    assert!(ran.err.contains("critical step"), "{}", ran.err);
    assert_eq!(ran.status(), 1);
    assert!(!ran.err.contains("[1/1]"), "nothing may start: {}", ran.err);
    assert_eq!(
        ran.line_prompts,
        ["Type \"yes\" to run it: "]
            .iter()
            .map(|p| p.replace("\"yes\"", "\"yes\" or \"/nonexistent-omsh-plan-test\""))
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_critical_step_runs_after_yes() {
    let answer = json(&[("Wipe", "shred /nonexistent-omsh-plan-test", "low")]);
    let ran = go(
        says(&answer),
        "x",
        Setup {
            keys: vec![enter()],
            lines: vec![Ok("yes".to_owned())],
            ..Setup::default()
        },
    );
    // It ran (and failed, the file does not exist).
    assert!(ran.err.contains("[1/1]"), "{}", ran.err);
    assert_ne!(ran.status(), 0);
}

#[test]
fn the_models_estimate_shows_when_it_is_higher_than_the_analyzers() {
    let answer = json(&[("Hello", "echo hi", "high")]);
    let ran = go(says(&answer), "x", Setup::default());
    assert!(
        ran.err.contains("[dangerous (the model's estimate)]"),
        "{}",
        ran.err
    );
}

#[test]
fn a_step_that_became_riskier_is_asked_about_before_it_runs() {
    // The panel showed no findings; the fresh analysis finds a critical command.
    let dir = Dir::new();
    let mut shell = Shell::new();
    let mut files = Io::process();
    let step = Reviewed {
        title: "Wipe".to_owned(),
        explanation: String::new(),
        needs_root: false,
        model: ModelRisk::Low,
        command: "shred /nonexistent-omsh-plan-test".to_owned(),
        analysis: Analysis::default(),
        missing: Vec::new(),
        missing_paths: Vec::new(),
        fixed: Vec::new(),
        radii: Vec::new(),
        edited: false,
    };
    let marker = Reviewed {
        command: format!("echo later > {}", dir.file("later")),
        ..step.clone()
    };
    let mut tty = Script(RefCell::new(State {
        lines: vec![Ok("no".to_owned())].into(),
        ..State::default()
    }));
    let mut edit = |_: &str| Edited::Cancelled;
    let mut recent = Recent::new(Filter::default());
    let mut env = PlanEnv {
        shell: &mut shell,
        files: &mut files,
        tty: &mut tty,
        history: None,
        recent: &mut recent,
        confirm: ConfirmLevel::Dangerous,
        blast: TEST_LIMITS,
        paint: &Paint::plain(),
        undo: None,
        edit: &mut edit,
        menu: None,
    };
    let result = execute(
        &mut env,
        &[step, marker],
        Mode {
            menu: None,
            stepwise: false,
        },
    );
    let LineResult::Ran(outcome) = result else {
        panic!("{result:?}")
    };
    assert_eq!(outcome.status().0, 1);
    let state = tty.0.borrow();
    assert!(
        state.err.contains("riskier than the plan showed"),
        "{}",
        state.err
    );
    assert!(
        state.err.contains("cancelled; nothing ran"),
        "{}",
        state.err
    );
    assert!(
        !dir.has("later"),
        "the plan stops at the step that was refused"
    );
}

// -- running --------------------------------------------------------------------------------

#[test]
fn a_failing_step_stops_the_plan() {
    let dir = Dir::new();
    let answer = json(&[
        ("One", &format!("echo 1 > {}", dir.file("one")), "low"),
        ("Two", "false", "low"),
        ("Three", &format!("echo 3 > {}", dir.file("three")), "low"),
    ]);
    let ran = go(
        says(&answer),
        "x",
        Setup {
            keys: vec![enter()],
            ..Setup::default()
        },
    );
    assert!(dir.has("one"));
    assert!(!dir.has("three"));
    assert_eq!(ran.status(), 1);
    assert_eq!(ran.shell.last_status().0, 1);
    assert!(
        ran.err
            .contains("step 2 did not succeed (status 1); the remaining 1 step did not run"),
        "{}",
        ran.err
    );
    assert!(ran.err.contains("[2/3] Two"), "{}", ran.err);
    assert!(!ran.err.contains("[3/3]"), "{}", ran.err);
}

#[test]
fn steps_share_the_session_and_report_each_status() {
    let dir = Dir::new();
    let answer = json(&[
        ("Go", &format!("cd {}", dir.0.display()), "low"),
        // `pwd` prints the shell's own idea of where it is. A relative file would depend on the
        // working directory of the whole test process, which other tests change too.
        ("Where", &format!("pwd > {}", dir.file("here")), "low"),
    ]);
    let ran = go(
        says(&answer),
        "x",
        Setup {
            keys: vec![enter()],
            ..Setup::default()
        },
    );
    let printed = fs::read_to_string(dir.0.join("here")).unwrap_or_default();
    assert_eq!(
        printed.trim_end(),
        dir.0.display().to_string(),
        "the cd of step 1 holds for step 2"
    );
    assert_eq!(ran.status(), 0);
    assert!(ran.err.contains("[1/2] Go") && ran.err.contains("[2/2] Where"));
    assert!(ran.err.contains("plan done (2 steps)"));
}

#[test]
fn a_background_job_that_fails_at_once_fails_its_step() {
    let dir = Dir::new();
    let answer = json(&[
        // Other tests leave the process in a folder that is gone by now; programs cannot start
        // from there, and the job would fail for that reason instead of the one under test.
        ("Go", "cd /", "low"),
        ("Start", "omsh-no-such-command-xyz &>/dev/null &", "low"),
        ("Next", &format!("echo x > {}", dir.file("next")), "low"),
    ]);
    let ran = go(
        says(&answer),
        "x",
        Setup {
            keys: vec![enter()],
            ..Setup::default()
        },
    );
    assert_eq!(ran.status(), 127, "{}", ran.err);
    assert!(
        ran.err.contains("background job [1]: Exit 127"),
        "{}",
        ran.err
    );
    assert!(
        ran.err.contains("step 2 did not succeed (status 127)"),
        "{}",
        ran.err
    );
    assert!(!ran.err.contains("plan done"), "{}", ran.err);
    assert!(!dir.has("next"));
}

#[test]
fn a_background_job_that_is_still_running_keeps_its_step_done() {
    // An absolute path (the shell of these tests has no PATH), from a folder that exists.
    let answer = json(&[("Go", "cd /", "low"), ("Wait", "/bin/sleep 1 &", "low")]);
    let ran = go(
        says(&answer),
        "x",
        Setup {
            keys: vec![enter()],
            ..Setup::default()
        },
    );
    assert_eq!(ran.status(), 0, "{}", ran.err);
    assert!(ran.err.contains("plan done (2 steps)"), "{}", ran.err);
    assert!(!ran.err.contains("background job"), "{}", ran.err);
}

#[test]
fn exit_in_a_step_ends_the_shell_with_its_status() {
    let answer = json(&[("Bye", "exit 3", "low")]);
    let ran = go(
        says(&answer),
        "x",
        Setup {
            keys: vec![enter()],
            ..Setup::default()
        },
    );
    assert!(matches!(
        ran.result,
        LineResult::Ran(Outcome::Exit(ExitStatus(3)))
    ));
}

#[test]
fn a_step_runs_as_the_model_wrote_it_and_aliases_do_not_change_it() {
    let dir = Dir::new();
    let command = format!("hi > {}", dir.file("out"));
    let answer = json(&[("Greet", &command, "low")]);
    let ran = go(
        says(&answer),
        "x",
        Setup {
            keys: vec![enter()],
            // `hi` is also a function, which does apply; the alias of the same name does not.
            aliases: vec!["hi() { echo from-function; }", "alias hi='echo aliased'"],
            ..Setup::default()
        },
    );
    assert!(!ran.err.contains("runs as:"), "{}", ran.err);
    assert_eq!(
        fs::read_to_string(dir.file("out")).unwrap(),
        "from-function\n"
    );
}

#[test]
fn an_alias_is_not_applied_to_a_step_that_names_a_real_program() {
    let dir = Dir::new();
    // `echo` is aliased to something that would change the output; the step must get the real one.
    let command = format!("echo plain > {}", dir.file("out"));
    let answer = json(&[("Say", &command, "low")]);
    let ran = go(
        says(&answer),
        "x",
        Setup {
            keys: vec![enter()],
            aliases: vec!["alias echo='echo aliased'"],
            ..Setup::default()
        },
    );
    assert!(!ran.err.contains("runs as:"), "{}", ran.err);
    assert_eq!(fs::read_to_string(dir.file("out")).unwrap(), "plain\n");
}

#[test]
fn every_step_that_ran_is_in_the_history() {
    let dir = Dir::new();
    let answer = json(&[
        ("One", &format!("echo 1 > {}", dir.file("one")), "low"),
        ("Two", "false", "low"),
    ]);
    let history = History {
        store: Store::in_memory(),
        filter: Filter::default(),
    };
    let ran = go(
        says(&answer),
        "x",
        Setup {
            keys: vec![enter()],
            history: Some(history.clone()),
            ..Setup::default()
        },
    );
    assert_eq!(ran.status(), 1);
    history.store.settle(Duration::from_secs(2));
    let entries = history.store.query(&Query::default());
    let mut seen: Vec<(String, Option<i32>)> = entries
        .iter()
        .map(|entry| (entry.command.clone(), entry.exit_status))
        .collect();
    seen.sort();
    assert_eq!(
        seen,
        [
            (format!("echo 1 > {}", dir.file("one")), Some(0)),
            ("false".to_owned(), Some(1)),
        ]
    );
}

mod chat;

// -- asking ---------------------------------------------------------------------------------

#[test]
fn the_tokens_the_server_reported_are_shown_once_after_the_wait_and_never_made_up() {
    let dir = Dir::new();
    let plan = json(&[("Touch", &format!("echo x > {}", dir.file("m")), "low")]);
    let fake = FakeProvider::new(Kind::Ollama)
        .says(&[&plan])
        .reporting(omsh_ai::Usage {
            input: Some(2048),
            output: Some(96),
            ..Default::default()
        });
    let ran = go(
        fake,
        "make a file",
        Setup {
            keys: vec![enter()],
            ..Setup::default()
        },
    );
    assert_eq!(
        ran.err.matches("2,048 tokens in, 96 out").count(),
        1,
        "{}",
        ran.err
    );
    // Without a report there is no figure at all.
    let dir = Dir::new();
    let plan = json(&[("Touch", &format!("echo x > {}", dir.file("m")), "low")]);
    let ran = go(
        says(&plan),
        "make a file",
        Setup {
            keys: vec![enter()],
            ..Setup::default()
        },
    );
    assert!(!ran.err.contains("tokens"), "{}", ran.err);
}

#[test]
fn a_bad_answer_is_asked_again_with_the_reason() {
    let dir = Dir::new();
    let good = json(&[("Touch", &format!("echo x > {}", dir.file("m")), "low")]);
    let fake = FakeProvider::new(Kind::Ollama)
        .says(&["I would love to help!"])
        .says(&[&good]);
    let ran = go(
        fake,
        "make a file",
        Setup {
            keys: vec![enter()],
            ..Setup::default()
        },
    );
    assert!(dir.has("m"));
    assert_eq!(ran.requests.len(), 2);
    let second = &ran.requests[1];
    assert_eq!(second.messages.len(), 3);
    assert!(
        second.messages[2].text.contains("no JSON object"),
        "{}",
        second.messages[2].text
    );
    assert_eq!(second.messages[0], ran.requests[0].messages[0]);
}

#[test]
fn two_bad_answers_end_with_a_clean_message_and_nothing_runs() {
    let fake = FakeProvider::new(Kind::Ollama)
        .says(&["nope"])
        .says(&["{\"summary\":\"s\",\"steps\":[]}"]);
    let ran = go(fake, "x", Setup::default());
    assert_eq!(ran.requests.len(), 2, "one retry, no more");
    assert_eq!(ran.status(), 1);
    assert!(
        ran.err
            .contains("oh-my-shell: ?: couldn't make a plan: the plan has no steps"),
        "{}",
        ran.err
    );
    assert!(ran.key_prompts.is_empty(), "no panel for a bad plan");
}

#[test]
fn commands_our_shell_cannot_run_are_sent_back() {
    let bad = json(&[("Expand", "echo {a,b}", "low")]);
    let good = json(&[("Echo", "echo ab", "low")]);
    let fake = FakeProvider::new(Kind::Ollama).says(&[&bad]).says(&[&good]);
    let ran = go(fake, "x", Setup::default());
    assert_eq!(ran.requests.len(), 2);
    let reason = &ran.requests[1].messages[2].text;
    assert!(reason.contains("step 1"), "{reason}");
    assert!(reason.contains("POSIX sh"), "{reason}");
    assert!(
        ran.err.contains("echo ab"),
        "the second plan is shown: {}",
        ran.err
    );
}

#[test]
fn placeholders_are_sent_back_too() {
    let bad = json(&[("Copy", "cp /path/to/file .", "low")]);
    let good = json(&[("List", "ls", "low")]);
    let fake = FakeProvider::new(Kind::Ollama).says(&[&bad]).says(&[&good]);
    let ran = go(fake, "x", Setup::default());
    assert!(
        ran.requests[1].messages[2].text.contains("placeholder"),
        "{}",
        ran.requests[1].messages[2].text
    );
}

#[test]
fn a_provider_error_is_one_line_and_nothing_else_happens() {
    let fake =
        FakeProvider::new(Kind::Ollama).fails(ProviderError::from_status(401, "Unauthorized"));
    let ran = go(fake, "x", Setup::default());
    assert_eq!(ran.status(), 1);
    assert!(
        ran.err
            .starts_with("Asking Ollama (local) (test-model)...\n"),
        "{}",
        ran.err
    );
    assert!(ran.err.contains("oh-my-shell: ?: "), "{}", ran.err);
    assert_eq!(ran.err.lines().count(), 2, "{}", ran.err);
    assert!(ran.key_prompts.is_empty());
}

#[test]
fn an_offline_provider_points_to_a_local_model() {
    let fake = FakeProvider::new(Kind::Ollama).fails(ProviderError::new(ErrorKind::Offline, "x"));
    let ran = go(fake, "x", Setup::default());
    assert!(ran.err.contains("ollama serve"), "{}", ran.err);
}

#[test]
fn an_empty_request_prints_the_usage_and_asks_nobody() {
    let ran = go(FakeProvider::new(Kind::Ollama), "   ", Setup::default());
    assert_eq!(ran.status(), 2);
    assert!(ran.err.contains("usage: ?"), "{}", ran.err);
    assert!(ran.requests.is_empty());
}

// -- what is sent ---------------------------------------------------------------------------

#[test]
fn secrets_in_the_request_are_masked_before_sending() {
    let token = format!("ghp_{}", "a1B2".repeat(9));
    let request = format!("push using {token} to my repo");
    let ran = go(
        says(&json(&[("List", "ls", "low")])),
        &request,
        Setup::default(),
    );
    let sent = &ran.requests[0].messages[0].text;
    assert!(!sent.contains(&token), "{sent}");
    assert!(sent.contains("[MASKED]"), "{sent}");
    assert!(sent.starts_with("push using "), "{sent}");
    assert!(ran.err.contains("hidden before sending"), "{}", ran.err);
    // Nothing to hide: no note, text unchanged.
    let ran = go(
        says(&json(&[("List", "ls", "low")])),
        "list files",
        Setup::default(),
    );
    assert_eq!(ran.requests[0].messages[0].text, "list files");
    assert!(!ran.err.contains("hidden"));
}

#[test]
fn the_context_toggles_decide_what_is_sent() {
    let ran = go(
        says(&json(&[("List", "ls", "low")])),
        "x",
        Setup {
            context: Some(AiContext {
                os: false,
                cwd: false,
                git: false,
                recent_commands: false,
            }),
            ..Setup::default()
        },
    );
    let system = ran.requests[0].system.clone().unwrap();
    assert!(
        !system.contains("OS:") && !system.contains("Working directory"),
        "{system}"
    );
    let ran = go(
        says(&json(&[("List", "ls", "low")])),
        "x",
        Setup {
            context: Some(AiContext {
                os: false,
                cwd: true,
                git: false,
                recent_commands: false,
            }),
            ..Setup::default()
        },
    );
    let system = ran.requests[0].system.clone().unwrap();
    assert!(system.contains("- Working directory: /"), "{system}");
    assert!(!system.contains("OS:"));
    // Only the request and the system prompt go out.
    assert_eq!(ran.requests[0].messages.len(), 1);
    assert!(ran.requests[0].json);
}

#[test]
fn the_os_name_is_read_from_os_release_text() {
    let text = "NAME=\"Ubuntu\"\nPRETTY_NAME=\"Ubuntu 26.04 LTS\"\nID=ubuntu\n";
    assert_eq!(parse_os_release(text).as_deref(), Some("Ubuntu 26.04 LTS"));
    assert_eq!(
        parse_os_release("PRETTY_NAME=Arch Linux").as_deref(),
        Some("Arch Linux")
    );
    assert_eq!(parse_os_release("NAME=x"), None);
    assert_eq!(parse_os_release("PRETTY_NAME=\"\""), None);
    let escaped = parse_os_release("PRETTY_NAME=\"A\u{1b}[31mB\"").unwrap();
    assert!(!escaped.contains('\u{1b}'), "{escaped:?}");
}

// -- editing --------------------------------------------------------------------------------

#[test]
fn a_command_can_be_edited_and_the_new_one_runs() {
    let dir = Dir::new();
    let answer = json(&[("Touch", &format!("echo old > {}", dir.file("old")), "low")]);
    let new = format!("echo new > {}", dir.file("new"));
    let ran = go(
        says(&answer),
        "x",
        Setup {
            keys: vec![ch('e'), enter()],
            edits: vec![Edited::Text(new)],
            ..Setup::default()
        },
    );
    assert!(dir.has("new") && !dir.has("old"));
    assert_eq!(ran.status(), 0);
    // The panel was shown again with the edit marked.
    assert!(ran.err.contains(", edited]"), "{}", ran.err);
}

#[test]
fn an_edited_command_goes_through_the_analyzer_again() {
    let answer = json(&[("Hello", "echo hi", "low")]);
    let ran = go(
        says(&answer),
        "x",
        Setup {
            keys: vec![ch('e'), enter()],
            edits: vec![Edited::Text("shred /nonexistent-omsh-plan-test".to_owned())],
            lines: vec![Ok("no".to_owned())],
            ..Setup::default()
        },
    );
    assert!(ran.err.contains("[critical, edited]"), "{}", ran.err);
    assert_eq!(ran.status(), 1);
    assert!(!ran.err.contains("[1/1]"), "{}", ran.err);
}

#[test]
fn an_edit_our_shell_cannot_run_is_refused_and_the_old_command_stays() {
    let dir = Dir::new();
    let command = format!("echo old > {}", dir.file("old"));
    let answer = json(&[("Touch", &command, "low")]);
    let ran = go(
        says(&answer),
        "x",
        Setup {
            keys: vec![ch('e'), enter()],
            edits: vec![Edited::Text("echo {a,b}".to_owned())],
            ..Setup::default()
        },
    );
    assert!(
        ran.err.contains("the command was not changed"),
        "{}",
        ran.err
    );
    assert!(dir.has("old"));
}

#[test]
fn giving_up_an_edit_changes_nothing() {
    let dir = Dir::new();
    let answer = json(&[("Touch", &format!("echo old > {}", dir.file("old")), "low")]);
    let ran = go(
        says(&answer),
        "x",
        Setup {
            keys: vec![ch('e'), enter()],
            edits: vec![Edited::Cancelled],
            ..Setup::default()
        },
    );
    assert!(dir.has("old"));
    assert_eq!(ran.status(), 0);
    assert!(!ran.err.contains("edited"));
}

#[test]
fn with_several_steps_the_person_says_which_one_to_edit() {
    let dir = Dir::new();
    let answer = json(&[
        ("One", &format!("echo 1 > {}", dir.file("one")), "low"),
        ("Two", &format!("echo 2 > {}", dir.file("two")), "low"),
    ]);
    let ran = go(
        says(&answer),
        "x",
        Setup {
            keys: vec![ch('e'), ch('2'), enter()],
            edits: vec![Edited::Text(format!("echo 22 > {}", dir.file("two22")))],
            ..Setup::default()
        },
    );
    assert!(dir.has("one") && dir.has("two22") && !dir.has("two"));
    assert!(
        ran.key_prompts
            .contains(&"Edit which step? [1-2] ".to_owned()),
        "{:?}",
        ran.key_prompts
    );
    // A step number that does not exist goes back to the question.
    let ran = go(
        says(&answer),
        "x",
        Setup {
            keys: vec![ch('e'), ch('9'), Ok(Key::Escape)],
            ..Setup::default()
        },
    );
    assert_eq!(ran.status(), 1);
}

#[test]
fn without_a_line_editor_the_new_command_is_typed_as_a_line() {
    let dir = Dir::new();
    let answer = json(&[("Touch", &format!("echo old > {}", dir.file("old")), "low")]);
    let new = format!("echo plain > {}", dir.file("plain"));
    let ran = go(
        says(&answer),
        "x",
        Setup {
            keys: vec![ch('e'), enter()],
            edits: vec![Edited::Unavailable],
            lines: vec![Ok(new)],
            ..Setup::default()
        },
    );
    assert!(dir.has("plain") && !dir.has("old"));
    assert_eq!(ran.status(), 0);
    assert_eq!(ran.line_prompts, ["new command (Enter keeps it)> "]);
    // An empty line keeps the command.
    let ran = go(
        says(&answer),
        "x",
        Setup {
            keys: vec![ch('e'), enter()],
            edits: vec![Edited::Unavailable],
            lines: vec![Ok(String::new())],
            ..Setup::default()
        },
    );
    assert!(dir.has("old"));
    assert_eq!(ran.status(), 0);
}

// -- the panel ------------------------------------------------------------------------------

#[test]
fn the_panel_shows_summary_steps_commands_reasons_and_root() {
    let answer = r#"{"summary":"Clean up","steps":[
        {"title":"Remove build","command":"rm -rf build","explanation":"Deletes the folder.","risk":"high","needs_root":true},
        {"title":"Look","command":"ls","explanation":"","risk":"low"}]}"#;
    let ran = go(says(answer), "x", Setup::default());
    let err = &ran.err;
    assert!(
        err.contains("plan from Ollama (local) (test-model):\n  Clean up\n"),
        "{err}"
    );
    assert!(err.contains("  1. Remove build   [dangerous]"), "{err}");
    assert!(err.contains("     $ rm -rf build\n"), "{err}");
    assert!(err.contains("     Deletes the folder.\n"), "{err}");
    assert!(err.contains("needs root:"), "{err}");
    assert!(err.contains("     - Deletes files without asking"), "{err}");
    assert!(err.contains("       on: build\n"), "{err}");
    assert!(err.contains("  2. Look   [safe]"), "{err}");
}

// -- commands that are not found, before anything runs ---------------------------------------

/// How many `not found:` notes the panels printed.
fn notes(err: &str) -> usize {
    err.matches("     not found: ").count()
}

#[test]
fn a_command_that_is_not_found_is_noted_under_its_step_and_nothing_is_blocked() {
    let answer = json(&[
        ("Open", "omsh-no-such-program --x", "low"),
        ("Look", "ls", "low"),
    ]);
    let ran = go(says(&answer), "x", Setup::default());
    let err = &ran.err;
    let first = err.find("1. Open").expect("step 1");
    let second = err.find("2. Look").expect("step 2");
    assert!(first < second);
    assert!(
        err[first..second].contains("     not found: omsh-no-such-program\n"),
        "{err}"
    );
    assert!(!err[second..].contains("not found:"), "{err}");
    assert_eq!(notes(err), 1, "{err}");
    // A note, not a refusal: the level is the same and the plan is still asked about.
    assert!(err.contains("1. Open   [safe]"), "{err}");
}

#[test]
fn the_note_says_what_the_shell_would_offer() {
    let answer = json(&[("Open", "gti status", "low")]);
    let ran = go(
        says(&answer),
        "x",
        Setup {
            suggest: vec!["git", "gist"],
            ..Setup::default()
        },
    );
    assert!(
        ran.err
            .contains("     not found: gti; did you mean git or gist?\n"),
        "{}",
        ran.err
    );
    // With nothing close, only the name.
    let ran = go(says(&answer), "x", Setup::default());
    assert!(ran.err.contains("     not found: gti\n"), "{}", ran.err);
}

#[test]
fn a_builtin_and_a_function_are_found_and_an_alias_alone_is_not() {
    let answer = json(&[("Go", "cd /", "low"), ("Greet", "greet", "low")]);
    let ran = go(
        says(&answer),
        "x",
        Setup {
            aliases: vec!["greet() { :; }"],
            ..Setup::default()
        },
    );
    assert_eq!(notes(&ran.err), 0, "{}", ran.err);
    // A plan step runs without aliases, so a step named after one would fail.
    let answer = json(&[("Long", "zz", "low")]);
    let ran = go(
        says(&answer),
        "x",
        Setup {
            aliases: vec!["alias zz=ls"],
            ..Setup::default()
        },
    );
    assert!(ran.err.contains("     not found: zz\n"), "{}", ran.err);
}

#[test]
fn editing_a_step_updates_its_note() {
    let answer = json(&[("Look", "ls", "low")]);
    let ran = go(
        says(&answer),
        "x",
        Setup {
            keys: vec![ch('e'), enter(), ch('e'), enter()],
            edits: vec![
                Edited::Text("omsh-no-such-program".to_owned()),
                Edited::Text("ls".to_owned()),
            ],
            ..Setup::default()
        },
    );
    // Only the panel shown after the first edit has the note: the first panel has none and the
    // one after the second edit has none again.
    assert_eq!(notes(&ran.err), 1, "{}", ran.err);
    assert!(
        ran.err.contains("     not found: omsh-no-such-program\n"),
        "{}",
        ran.err
    );
}

#[test]
fn the_panel_with_buttons_colors_the_label_like_needs_root() {
    use omsh_ui::text::Tone;
    let lines = view::body_lines("     not found: gti; did you mean git?\n", 80);
    assert_eq!(lines.len(), 1);
    let spans = lines[0].spans();
    assert_eq!(spans[0].text(), "not found:");
    assert_eq!(spans[0].style.tone, Tone::Warn);
    assert_eq!(spans[1].text(), " gti; did you mean git?");
    assert_eq!(spans[1].style.tone, Tone::Plain);
}

/// The commands of `line` that are not found in a shell that has nothing special.
fn missing_in(line: &str) -> Vec<String> {
    missing_commands(&Shell::new(), line)
        .into_iter()
        .map(|(name, _)| name)
        .collect()
}

#[test]
fn compound_lines_whose_commands_all_exist_get_no_note() {
    for line in [
        "VAR=1 ls",
        "ls | wc -l",
        "cd / && ls",
        "for x in a; do ls; done",
        "if true; then ls; fi",
        "echo $(ls)",
    ] {
        assert_eq!(missing_in(line), Vec::<String>::new(), "{line}");
    }
    // The same lines as the steps of a plan: no note in any panel.
    let answer = json(&[
        ("a", "VAR=1 ls", "low"),
        ("b", "ls | wc -l", "low"),
        ("c", "cd / && ls", "low"),
        ("d", "for x in a; do ls; done", "low"),
        ("e", "if true; then ls; fi", "low"),
        ("f", "echo $(ls)", "low"),
    ]);
    let ran = go(says(&answer), "x", Setup::default());
    assert_eq!(notes(&ran.err), 0, "{}", ran.err);
}

#[test]
fn other_shapes_of_a_line_get_no_note_either() {
    for line in [
        "f() { :; }; f",
        "case x in a) ls;; esac",
        "while read l; do ls; done",
        "until false; do ls; done",
        "! ls",
        "(ls)",
        "{ ls; }",
        "ls > /dev/null 2>&1",
        "x=$(ls)",
        "ls &",
        "ls; ls",
        "[ -d / ] && ls",
        "FOO=1 BAR=2 ls",
        "echo \"$(ls)\"",
        "echo `ls`",
        "command ls",
        "cat <<EOF\nx\nEOF",
        "ls \\\n  -l",
    ] {
        assert_eq!(missing_in(line), Vec::<String>::new(), "{line:?}");
    }
}

#[test]
fn a_missing_command_is_found_inside_every_shape_of_line() {
    let none = "omsh-no-such-program";
    for line in [
        "omsh-no-such-program",
        "ls | omsh-no-such-program",
        "cd / && omsh-no-such-program",
        "VAR=1 omsh-no-such-program",
        "for x in a; do omsh-no-such-program; done",
        "if true; then omsh-no-such-program; fi",
        "echo $(omsh-no-such-program)",
        "'omsh-no-such-program' --x",
    ] {
        assert_eq!(missing_in(line), [none], "{line}");
    }
    // A function defined in the line hides only its own name.
    assert_eq!(missing_in("f() { :; }; f; omsh-no-such-program"), [none]);
    // Said once, in the order of the line.
    assert_eq!(
        missing_in("omsh-no-such-program && ls && omsh-no-such-two; omsh-no-such-program"),
        [none, "omsh-no-such-two"]
    );
}

#[test]
fn a_command_that_may_depend_on_the_start_or_an_earlier_step_is_left_alone() {
    for line in [
        "./omsh-no-such-program",
        "/opt/omsh-no-such/bin/tool --x",
        "$cmd --x",
        "omsh-no-such-*",
    ] {
        assert_eq!(missing_in(line), Vec::<String>::new(), "{line}");
    }
}

#[test]
fn a_long_command_and_a_long_explanation_are_cut() {
    let long = format!("echo {}", "a".repeat(400));
    let many = (0..9)
        .map(|n| format!("echo {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    let answer = json(&[("Long", &long, "low"), ("Many", &many, "low")]);
    let ran = go(says(&answer), "x", Setup::default());
    assert!(ran.err.contains("..."), "{}", ran.err);
    assert!(!ran.err.contains(&"a".repeat(200)));
    assert!(ran.err.contains("       ...\n"), "{}", ran.err);
}

#[test]
fn text_from_the_model_cannot_carry_escape_codes_into_the_panel() {
    let bad = r#"{"summary":"Hi\u001b[2J","steps":[{"title":"t","command":"ls","risk":"low"}]}"#;
    let good = json(&[("List", "ls", "low")]);
    let fake = FakeProvider::new(Kind::Ollama).says(&[bad]).says(&[&good]);
    let ran = go(fake, "x", Setup::default());
    assert!(!ran.err.contains('\u{1b}'), "{:?}", ran.err);
    assert!(
        ran.requests[1].messages[2]
            .text
            .contains("control character")
    );
}

#[test]
fn choose_reads_keys_until_one_means_something() {
    let mut tty = Script(RefCell::new(State {
        keys: vec![ch('?'), ch('e')].into(),
        ..State::default()
    }));
    let shell = Shell::new();
    let files = Io::process();
    let mut terminal = tty.open(&shell, &files);
    assert_eq!(
        choose(&mut *terminal, 1, "", &Paint::plain()),
        Choice::Edit(0)
    );
}

// -- context: git, recent commands, `[v]` -----------------------------------------------------

/// A directory that looks like a git repository on branch `branch`, removed at the end.
fn repo(dir: &Dir, branch: &str) {
    fs::create_dir_all(dir.0.join(".git")).unwrap();
    fs::write(
        dir.0.join(".git/HEAD"),
        format!("ref: refs/heads/{branch}\n"),
    )
    .unwrap();
}

fn system_of(ran: &Ran) -> String {
    ran.requests[0].system.clone().unwrap()
}

fn in_dir(dir: &Dir) -> Vec<&'static str> {
    // `Setup::aliases` are lines the shell runs before the request; `cd` sets PWD.
    vec![Box::leak(format!("cd {}", dir.file("")).into_boxed_str())]
}

#[test]
fn the_git_branch_and_recent_commands_are_sent_by_default() {
    let dir = Dir::new();
    repo(&dir, "feature/x");
    let ran = go(
        says(&json(&[("List", "ls", "low")])),
        "x",
        Setup {
            aliases: in_dir(&dir),
            recent: vec![("ls", 0), ("false", 1)],
            ..Setup::default()
        },
    );
    let system = system_of(&ran);
    assert!(
        system.contains("- Git: inside a repository, on branch feature/x\n"),
        "{system}"
    );
    assert!(
        system.contains("  - ls  (exit 0)\n  - false  (exit 1)\n"),
        "{system}"
    );
}

#[test]
fn a_folder_that_is_no_repository_says_so() {
    let dir = Dir::new();
    let ran = go(
        says(&json(&[("List", "ls", "low")])),
        "x",
        Setup {
            aliases: in_dir(&dir),
            ..Setup::default()
        },
    );
    // The temp folder may itself sit inside a repository on some machines; only the shape of
    // the answer is fixed.
    assert!(system_of(&ran).contains("- Git: "), "{}", system_of(&ran));
}

#[test]
fn recent_commands_are_masked_and_the_note_is_shown() {
    let token = format!("ghp_{}", "a1B2".repeat(9));
    let line: &'static str = Box::leak(format!("export GITHUB_TOKEN={token}").into_boxed_str());
    let ran = go(
        says(&json(&[("List", "ls", "low")])),
        "list files",
        Setup {
            recent: vec![(line, 0)],
            ..Setup::default()
        },
    );
    let system = system_of(&ran);
    assert!(!system.contains(&token), "{system}");
    assert!(
        system.contains("export GITHUB_TOKEN=[MASKED]  (exit 0)"),
        "{system}"
    );
    assert!(ran.err.contains("hidden before sending"), "{}", ran.err);
}

#[test]
fn the_switches_keep_git_and_recent_commands_out() {
    let dir = Dir::new();
    repo(&dir, "main");
    let ran = go(
        says(&json(&[("List", "ls", "low")])),
        "x",
        Setup {
            aliases: in_dir(&dir),
            recent: vec![("ls", 0)],
            context: Some(AiContext {
                git: false,
                recent_commands: false,
                ..AiContext::default()
            }),
            ..Setup::default()
        },
    );
    let system = system_of(&ran);
    assert!(
        !system.contains("Git:") && !system.contains("Recent commands"),
        "{system}"
    );
    assert!(system.contains("Working directory"), "{system}");
}

#[test]
fn lines_the_history_ignores_are_not_sent_as_recent_commands() {
    let ran = go(
        says(&json(&[("List", "ls", "low")])),
        "x",
        Setup {
            recent: vec![(" hidden thing", 0), ("visible", 0)],
            ..Setup::default()
        },
    );
    let system = system_of(&ran);
    assert!(!system.contains("hidden thing"), "{system}");
    assert!(system.contains("  - visible  (exit 0)"), "{system}");
}

#[test]
fn steps_that_run_become_recent_commands() {
    let dir = Dir::new();
    let first = format!("echo a > {}", dir.file("a"));
    let answer = json(&[("One", &first, "low"), ("Two", "false", "low")]);
    let ran = go(
        says(&answer),
        "x",
        Setup {
            keys: vec![enter()],
            recent: vec![("old1", 0), ("old2", 0)],
            ..Setup::default()
        },
    );
    let kept: Vec<(&str, i32)> = ran
        .recent
        .iter()
        .map(|r| (r.command.as_str(), r.status))
        .collect();
    // Three are kept: the older one first, then the two steps with their status.
    assert_eq!(kept, [("old2", 0), (first.as_str(), 0), ("false", 1)]);
}

#[test]
fn v_shows_what_was_sent_and_then_keeps_waiting() {
    let dir = Dir::new();
    let marker = dir.file("marker");
    let command = format!("echo hi > {marker}");
    let answer = json(&[("Say hi", &command, "low")]);
    let ran = go(
        says(&answer),
        "say hi please",
        Setup {
            keys: vec![ch('v'), ch('V'), Ok(Key::Escape)],
            recent: vec![("ls", 0)],
            ..Setup::default()
        },
    );
    // Shown twice, and neither `v` ran anything or ended the question.
    assert_eq!(
        ran.err.matches("this was sent to").count(),
        2,
        "{}",
        ran.err
    );
    assert!(!dir.has("marker"));
    assert_eq!(ran.key_prompts.len(), 3);
    assert!(
        ran.err.contains("  | You are the planning engine"),
        "{}",
        ran.err
    );
    assert!(
        ran.err.contains("  | - Recent commands in this shell"),
        "{}",
        ran.err
    );
    assert!(
        ran.err.contains("request:\n  | say hi please\n"),
        "{}",
        ran.err
    );
    assert!(
        ran.err.contains("[MASKED] before anything is sent"),
        "{}",
        ran.err
    );
    assert!(ran.err.contains("cancelled; nothing ran"), "{}", ran.err);
    // Enter after `v` still runs.
    let ran = go(
        says(&answer),
        "x",
        Setup {
            keys: vec![ch('v'), enter()],
            ..Setup::default()
        },
    );
    assert!(dir.has("marker"));
    assert_eq!(ran.status(), 0);
}

#[test]
fn v_shows_the_masked_request_and_never_the_secret() {
    let token = format!("ghp_{}", "a1B2".repeat(9));
    let ran = go(
        says(&json(&[("List", "ls", "low")])),
        &format!("use {token} please"),
        Setup {
            keys: vec![ch('v'), Ok(Key::Escape)],
            ..Setup::default()
        },
    );
    assert!(!ran.err.contains(&token), "{}", ran.err);
    assert!(ran.err.contains("  | use [MASKED] please\n"), "{}", ran.err);
}

#[test]
fn v_after_a_second_try_shows_the_retry_message() {
    let good = json(&[("List", "ls", "low")]);
    let fake = FakeProvider::new(Kind::Ollama)
        .says(&["{\"summary\":\"FIRST-ANSWER-MARKER\",\"steps\":[]}"])
        .says(&[&good]);
    let ran = go(
        fake,
        "x",
        Setup {
            keys: vec![ch('v'), Ok(Key::Escape)],
            ..Setup::default()
        },
    );
    assert!(
        ran.err.contains("second try: the model's first answer ("),
        "{}",
        ran.err
    );
    assert!(
        ran.err.contains("That answer was not usable"),
        "{}",
        ran.err
    );
    // The refused answer's own text is not printed again.
    assert!(!ran.err.contains("FIRST-ANSWER-MARKER"), "{}", ran.err);
}

#[test]
fn the_view_drops_control_characters() {
    let sent = Sent {
        request: ChatRequest {
            system: Some("line\x1b[2J one\nline two".to_owned()),
            messages: vec![omsh_ai::Message::user("ask\x07 me")],
            max_tokens: 1,
            json: true,
        },
        first_answer_chars: None,
    };
    let settings = Settings {
        kind: Kind::Ollama,
        model: "m".to_owned(),
        base_url: None,
    };
    let view = sent_view(&sent, &settings);
    assert!(
        !view.chars().any(|c| c.is_control() && c != '\n'),
        "{view:?}"
    );
    assert!(view.contains("  | line[2J one\n  | line two\n"), "{view:?}");
    assert!(view.contains("  | ask me\n"), "{view:?}");
}

// -- the blast radius and the question after Enter ---------------------------------------------

/// `a/` with two files and `b/` with four, in a fresh directory.
fn two_folders() -> Dir {
    let dir = Dir::new();
    for (folder, files) in [("a", 2), ("b", 4)] {
        fs::create_dir_all(dir.0.join(folder)).unwrap();
        for i in 0..files {
            fs::write(dir.0.join(format!("{folder}/f{i}")), "12345").unwrap();
        }
    }
    dir
}

#[test]
fn the_panel_counts_what_a_destructive_step_would_touch() {
    let dir = two_folders();
    let command = format!("rm -rf {}", dir.file("a"));
    let ran = go(
        says(&json(&[("Remove a", &command, "high")])),
        "x",
        Setup {
            keys: vec![Ok(Key::Escape)],
            ..Setup::default()
        },
    );
    assert!(
        ran.err
            .contains("       touches 2 files and 1 folder (10 B of file data)\n"),
        "{}",
        ran.err
    );
    assert!(ran.err.contains("owned by someone else"), "{}", ran.err);
    // Counting changed nothing.
    assert!(dir.has("a/f0") && dir.has("a/f1"));
    assert_eq!(ran.status(), 1);
}

#[test]
fn a_dangerous_plan_asks_once_more_after_enter() {
    let dir = two_folders();
    let command = format!("rm -rf {}", dir.file("a"));
    let answer = json(&[("Remove a", &command, "low")]);
    // "n": nothing is deleted.
    let ran = go(
        says(&answer),
        "x",
        Setup {
            keys: vec![enter()],
            lines: vec![Ok("n".to_owned())],
            ..Setup::default()
        },
    );
    assert_eq!(ran.status(), 1);
    assert_eq!(ran.line_prompts, ["Run it? [y/N] "]);
    assert!(
        ran.err.contains("this plan has a dangerous step"),
        "{}",
        ran.err
    );
    assert!(dir.has("a/f0"), "nothing ran");
    // Enter alone at that question is "no" too.
    let ran = go(
        says(&answer),
        "x",
        Setup {
            keys: vec![enter()],
            lines: vec![Ok(String::new())],
            ..Setup::default()
        },
    );
    assert_eq!(ran.status(), 1);
    assert!(dir.has("a/f0"));
    // "y": it runs.
    let ran = go(
        says(&answer),
        "x",
        Setup {
            keys: vec![enter()],
            lines: vec![Ok("y".to_owned())],
            ..Setup::default()
        },
    );
    assert_eq!(ran.status(), 0);
    assert!(!dir.has("a"), "the folder was removed after the answer");
    assert!(dir.has("b/f0"));
}

#[test]
fn the_models_own_dangerous_label_asks_too() {
    // The analyzer sees nothing wrong with `echo`; the model said "high". The person is asked.
    let dir = Dir::new();
    let command = format!("echo hi > {}", dir.file("m"));
    let answer = json(&[("Say hi", &command, "high")]);
    let ran = go(
        says(&answer),
        "x",
        Setup {
            keys: vec![enter()],
            lines: vec![Ok("n".to_owned())],
            ..Setup::default()
        },
    );
    assert_eq!(ran.status(), 1);
    assert_eq!(ran.line_prompts, ["Run it? [y/N] "]);
    assert!(!dir.has("m"));
}

#[test]
fn a_harmless_plan_does_not_ask_again() {
    let dir = Dir::new();
    let command = format!("echo hi > {}", dir.file("m"));
    let ran = go(
        says(&json(&[("Say hi", &command, "low")])),
        "x",
        Setup {
            keys: vec![enter()],
            ..Setup::default()
        },
    );
    assert_eq!(ran.status(), 0);
    assert!(ran.line_prompts.is_empty());
    assert!(dir.has("m"));
}

#[test]
fn with_confirm_set_to_critical_a_dangerous_plan_does_not_ask_again() {
    let dir = two_folders();
    let command = format!("rm -rf {}", dir.file("a"));
    let ran = go(
        says(&json(&[("Remove a", &command, "high")])),
        "x",
        Setup {
            keys: vec![enter()],
            confirm: Some(ConfirmLevel::Critical),
            ..Setup::default()
        },
    );
    assert_eq!(ran.status(), 0);
    assert!(ran.line_prompts.is_empty(), "{:?}", ran.line_prompts);
    assert!(!dir.has("a"));
    // The panel still showed the count.
    assert!(ran.err.contains("touches 2 files"), "{}", ran.err);
}

#[test]
fn with_confirm_set_to_caution_a_plan_with_a_caution_step_asks() {
    let dir = Dir::new();
    // `git clean -f` is a caution; the plan asks only because the setting says so.
    let command = format!("git clean -f 2>/dev/null; echo > {}", dir.file("m"));
    let answer = json(&[("Clean", &command, "low")]);
    let ran = go(
        says(&answer),
        "x",
        Setup {
            keys: vec![enter()],
            lines: vec![Ok("n".to_owned())],
            confirm: Some(ConfirmLevel::Caution),
            ..Setup::default()
        },
    );
    assert_eq!(ran.line_prompts, ["Run it? [y/N] "]);
    assert!(!dir.has("m"));
}

#[test]
fn a_cd_in_one_step_decides_what_the_next_one_means() {
    // `rm -rf *` is only critical because the step before it moved to `/etc`.
    let answer = json(&[("Go", "cd /etc", "low"), ("Clear", "rm -rf *", "low")]);
    let ran = go(
        says(&answer),
        "x",
        Setup {
            keys: vec![Ok(Key::Escape)],
            blast: Some(BlastLimits {
                max_entries: 100,
                max_ms: 600_000,
            }),
            ..Setup::default()
        },
    );
    assert!(ran.err.contains("2. Clear   [critical]"), "{}", ran.err);
    assert!(
        ran.err
            .contains("(counted now, before the steps above have run)"),
        "{}",
        ran.err
    );
    assert_eq!(ran.status(), 1);
}

#[test]
fn editing_a_step_counts_the_new_command() {
    let dir = two_folders();
    let first = format!("rm -rf {}", dir.file("a"));
    let second = format!("rm -rf {}", dir.file("b"));
    let ran = go(
        says(&json(&[("Remove", &first, "high")])),
        "x",
        Setup {
            keys: vec![ch('e'), Ok(Key::Escape)],
            edits: vec![Edited::Text(second)],
            ..Setup::default()
        },
    );
    let first_count = ran.err.find("touches 2 files").expect(&ran.err);
    let second_count = ran.err.find("touches 4 files").expect(&ran.err);
    assert!(first_count < second_count, "{}", ran.err);
    assert!(dir.has("a/f0") && dir.has("b/f0"), "nothing was deleted");
}

#[test]
fn a_find_delete_step_says_what_it_searches() {
    let dir = two_folders();
    let command = format!("find {} -name 'f0' -delete", dir.file(""));
    let ran = go(
        says(&json(&[("Find", &command, "low")])),
        "x",
        Setup {
            keys: vec![enter()],
            lines: vec![Ok("n".to_owned())],
            ..Setup::default()
        },
    );
    assert!(
        ran.err.contains("searches 6 files and 3 folders"),
        "{}",
        ran.err
    );
    assert!(ran.err.contains("it deletes only the ones its tests match"));
    assert_eq!(ran.status(), 1);
    assert!(dir.has("a/f0"), "the answer was no");
}

#[test]
fn editing_an_earlier_step_changes_how_the_later_ones_are_judged() {
    // `rm -rf *` is dangerous in a temporary folder and critical once the step before it has
    // moved to `/etc`; editing that step must show the change.
    let dir = Dir::new();
    let answer = json(&[
        ("Go", &format!("cd {}", dir.0.display()), "low"),
        ("Clear", "rm -rf *", "low"),
    ]);
    let ran = go(
        says(&answer),
        "x",
        Setup {
            keys: vec![ch('e'), ch('1'), Ok(Key::Escape)],
            edits: vec![Edited::Text("cd /etc".to_owned())],
            blast: Some(BlastLimits {
                max_entries: 100,
                max_ms: 600_000,
            }),
            ..Setup::default()
        },
    );
    let before = ran.err.find("2. Clear   [dangerous]").expect(&ran.err);
    let after = ran.err.find("2. Clear   [critical]").expect(&ran.err);
    assert!(before < after, "{}", ran.err);
}

#[test]
fn the_panel_of_a_step_that_became_riskier_counts_too() {
    let dir = two_folders();
    let mut shell = Shell::new();
    let mut files = Io::process();
    // The plan showed nothing about this step; the fresh analysis finds a dangerous one.
    let step = Reviewed {
        title: "Remove a".to_owned(),
        explanation: String::new(),
        needs_root: false,
        model: ModelRisk::Low,
        command: format!("rm -rf {}", dir.file("a")),
        analysis: Analysis::default(),
        missing: Vec::new(),
        missing_paths: Vec::new(),
        fixed: Vec::new(),
        radii: Vec::new(),
        edited: false,
    };
    let mut tty = Script(RefCell::new(State {
        lines: vec![Ok("no".to_owned())].into(),
        ..State::default()
    }));
    let mut edit = |_: &str| Edited::Cancelled;
    let mut recent = Recent::new(Filter::default());
    let mut env = PlanEnv {
        shell: &mut shell,
        files: &mut files,
        tty: &mut tty,
        history: None,
        recent: &mut recent,
        confirm: ConfirmLevel::Dangerous,
        blast: TEST_LIMITS,
        paint: &Paint::plain(),
        undo: None,
        edit: &mut edit,
        menu: None,
    };
    let result = execute(
        &mut env,
        &[step],
        Mode {
            menu: None,
            stepwise: false,
        },
    );
    let LineResult::Ran(outcome) = result else {
        panic!("{result:?}")
    };
    assert_eq!(outcome.status().0, 1);
    let state = tty.0.borrow();
    assert!(
        state.err.contains("riskier than the plan showed"),
        "{}",
        state.err
    );
    assert!(
        state.err.contains("touches 2 files and 1 folder"),
        "{}",
        state.err
    );
    assert!(dir.has("a/f0"), "the answer was no");
}

// -- the trash and the undo journal (S18) ---------------------------------------------------

mod trash {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn mode(path: &str) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o7777
    }

    fn with_trash() -> (Arc<Undo>, Dir) {
        let data = Dir::new();
        (Undo::new(data.0.join("data")), data)
    }

    #[test]
    fn an_ai_delete_goes_to_the_trash_is_journaled_and_can_be_undone() {
        let work = Dir::new();
        let (undo, _data) = with_trash();
        let victim = work.file("victim");
        fs::create_dir_all(format!("{victim}/inner")).unwrap();
        fs::write(format!("{victim}/inner/f"), "precious").unwrap();
        fs::set_permissions(&victim, fs::Permissions::from_mode(0o750)).unwrap();
        fs::set_permissions(
            format!("{victim}/inner/f"),
            fs::Permissions::from_mode(0o640),
        )
        .unwrap();
        let answer = json(&[("Remove it", &format!("rm -rf {victim}"), "high")]);
        let ran = go(
            says(&answer),
            "remove the victim",
            Setup {
                keys: vec![enter()],
                lines: vec![Ok("y".to_owned())],
                undo: Some(Arc::clone(&undo)),
                ..Setup::default()
            },
        );
        assert_eq!(ran.status(), 0, "{}", ran.err);
        assert!(!work.has("victim"), "the folder is gone from its place");
        assert!(
            ran.err
                .contains("rm goes to the trash; undo can bring it back"),
            "{}",
            ran.err
        );
        assert!(
            ran.err
                .contains("moved 1 item to the trash; \"undo\" brings it back (#1)"),
            "{}",
            ran.err
        );
        // It is in the trash, whole, and the journal knows.
        let journal = undo.journal().unwrap();
        let action = journal.latest_undoable().unwrap();
        assert_eq!(action.command, format!("rm -rf {victim}"));
        assert_eq!(action.origin, omsh_safety::journal::Origin::Plan);
        assert_eq!(action.items, 1);
        let report = journal.undo(action.id, &|| false).unwrap();
        assert_eq!(report.failed, 0, "{:?}", report.lines);
        assert_eq!(mode(&victim), 0o750);
        assert_eq!(mode(&format!("{victim}/inner/f")), 0o640);
        assert_eq!(
            fs::read_to_string(format!("{victim}/inner/f")).unwrap(),
            "precious"
        );
    }

    #[test]
    fn a_delete_that_cannot_go_to_the_trash_is_said_to_be_for_good() {
        let work = Dir::new();
        let (undo, _data) = with_trash();
        fs::write(work.file("keep"), "x").unwrap();
        let answer = json(&[(
            "Remove it",
            &format!("command rm -f {}", work.file("keep")),
            "medium",
        )]);
        let ran = go(
            says(&answer),
            "remove",
            Setup {
                keys: vec![Ok(Key::Escape)],
                undo: Some(undo),
                ..Setup::default()
            },
        );
        assert!(
            ran.err.contains(
                "deletes for good, not through the trash: it runs through sudo or another program"
            ),
            "{}",
            ran.err
        );
        assert!(work.has("keep"), "cancelled: nothing ran");
    }

    #[test]
    fn without_a_trash_the_panel_says_the_delete_is_for_good() {
        let work = Dir::new();
        fs::write(work.file("gone"), "x").unwrap();
        let answer = json(&[("Remove", &format!("rm {}", work.file("gone")), "low")]);
        let ran = go(
            says(&answer),
            "remove",
            Setup {
                keys: vec![enter()],
                ..Setup::default()
            },
        );
        assert!(
            ran.err
                .contains("deletes for good: the trash is not available in this shell"),
            "{}",
            ran.err
        );
        assert!(!work.has("gone"), "a plain rm ran");
    }

    #[test]
    fn mkdir_mv_and_writes_of_a_step_are_followed_and_undone() {
        let work = Dir::new();
        let (undo, _data) = with_trash();
        fs::write(work.file("a.txt"), "a").unwrap();
        fs::write(work.file("cfg"), "old").unwrap();
        fs::set_permissions(work.file("cfg"), fs::Permissions::from_mode(0o640)).unwrap();
        let command = format!(
            "mkdir {out} && mv {a} {out}/ && echo new > {cfg}",
            out = work.file("out"),
            a = work.file("a.txt"),
            cfg = work.file("cfg")
        );
        let answer = json(&[("Reorganize", &command, "low")]);
        let ran = go(
            says(&answer),
            "reorganize",
            Setup {
                keys: vec![enter()],
                undo: Some(Arc::clone(&undo)),
                ..Setup::default()
            },
        );
        assert_eq!(ran.status(), 0, "{}", ran.err);
        assert!(
            ran.err
                .contains("undo can reverse: a redirection, mkdir, mv"),
            "{}",
            ran.err
        );
        assert!(work.has("out/a.txt") && !work.has("a.txt"));
        assert_eq!(fs::read_to_string(work.file("cfg")).unwrap(), "new\n");
        assert!(
            ran.err
                .contains("recorded 3 changes (0 moved to the trash)"),
            "{}",
            ran.err
        );

        let journal = undo.journal().unwrap();
        let action = journal.latest_undoable().unwrap();
        assert_eq!(action.items, 3);
        let report = journal.undo(action.id, &|| false).unwrap();
        assert_eq!(report.failed, 0, "{:?}", report.lines);
        assert!(work.has("a.txt") && !work.has("out"));
        assert_eq!(fs::read_to_string(work.file("cfg")).unwrap(), "old");
        assert_eq!(mode(&work.file("cfg")), 0o640);
    }

    #[test]
    fn a_step_that_changes_nothing_leaves_no_entry() {
        let work = Dir::new();
        let (undo, _data) = with_trash();
        fs::write(work.file("a.txt"), "a").unwrap();
        // `mv` of a file that does not exist fails and changes nothing; the `||` goes on.
        let command = format!("mv {} {} || true", work.file("nothing"), work.file("else"));
        let answer = json(&[("Try", &command, "low")]);
        let ran = go(
            says(&answer),
            "try",
            Setup {
                keys: vec![enter()],
                undo: Some(Arc::clone(&undo)),
                ..Setup::default()
            },
        );
        assert_eq!(ran.status(), 0, "{}", ran.err);
        let recorded = undo.journal().map(|j| j.list(10).len()).unwrap_or(0);
        assert_eq!(recorded, 0);
        assert!(!ran.err.contains("recorded"), "{}", ran.err);
    }

    #[test]
    fn a_change_that_cannot_be_followed_is_said_before_the_key() {
        let work = Dir::new();
        let (undo, _data) = with_trash();
        let answer = json(&[(
            "Make",
            &format!("for d in a b; do mkdir {}/$d; done", work.file("x")),
            "low",
        )]);
        let ran = go(
            says(&answer),
            "make",
            Setup {
                keys: vec![Ok(Key::Escape)],
                undo: Some(undo),
                ..Setup::default()
            },
        );
        // `$d` is only known when the loop runs: the panel says `undo` cannot follow it.
        assert!(
            ran.err.contains("not undoable: mkdir (")
                && ran.err.contains("/x/$d is only known when the step runs)"),
            "{}",
            ran.err
        );
        assert!(!work.has("x"));
    }
}

// -- the offer to explain a failure (S24) ----------------------------------------------------

#[test]
fn a_step_that_ran_and_failed_offers_the_explanation_and_is_the_last_command() {
    let answer = json(&[
        ("Fail", "sh -c 'exit 3'", "low"),
        ("Never", "echo unreachable", "low"),
    ]);
    let ran = go(
        says(&answer),
        "do it",
        Setup {
            keys: vec![enter()],
            ..Setup::default()
        },
    );
    assert_eq!(ran.status(), 3);
    assert!(
        ran.err
            .contains("type ?? (or press Alt+?) to ask why it failed"),
        "{}",
        ran.err
    );
    // `??` would explain exactly this step.
    assert_eq!(
        ran.recent.last().map(|c| (c.command.as_str(), c.status)),
        Some(("sh -c 'exit 3'", 3))
    );
}

#[test]
fn no_offer_after_ctrl_c_a_stop_a_cancel_or_success() {
    for status in [130, 148] {
        let answer = json(&[("Stop", &format!("sh -c 'exit {status}'"), "low")]);
        let ran = go(
            says(&answer),
            "do it",
            Setup {
                keys: vec![enter()],
                ..Setup::default()
            },
        );
        assert_eq!(ran.status(), status);
        assert!(!ran.err.contains("Alt+?"), "{status}: {}", ran.err);
    }
    let answer = json(&[("Fine", "true", "low")]);
    let ran = go(
        says(&answer),
        "do it",
        Setup {
            keys: vec![enter()],
            ..Setup::default()
        },
    );
    assert!(!ran.err.contains("Alt+?"), "{}", ran.err);
    let ran = go(
        says(&answer),
        "do it",
        Setup {
            keys: vec![Ok(Key::Escape)],
            ..Setup::default()
        },
    );
    assert!(!ran.err.contains("Alt+?"), "{}", ran.err);
}

// -- Ctrl+Z on a step (phase 6) --------------------------------------------------------------
//
// `pause` is the part of the flow that needs no terminal of its own: the question is a function
// here, and the jobs are real (`set -m`), so a stopped step is a real stopped process.

/// A shell with job control and the pieces `pause` needs, and the steps of a plan of two:
/// the one that gets stopped, and one after it.
fn paused_plan<R>(body: impl FnOnce(&mut PlanEnv<'_>, &[Reviewed], &mut Run) -> R) -> R {
    let mut shell = Shell::new();
    let mut files = Io::process();
    shell.run_source("set -m", true, &mut files);
    let mut tty = Script::default();
    let mut recent = Recent::new(Filter::new(&[" *".to_owned()]));
    let mut edit = |_: &str| Edited::Cancelled;
    let mut env = PlanEnv {
        shell: &mut shell,
        files: &mut files,
        tty: &mut tty,
        history: None,
        recent: &mut recent,
        confirm: ConfirmLevel::Dangerous,
        blast: TEST_LIMITS,
        paint: &Paint::plain(),
        undo: None,
        edit: &mut edit,
        menu: None,
    };
    let step = |title: &str| {
        let analysis = review(env.shell, "true", &StartDir::Shell).unwrap();
        Reviewed {
            title: title.to_owned(),
            explanation: String::new(),
            needs_root: false,
            model: ModelRisk::Low,
            command: "true".to_owned(),
            analysis,
            missing: Vec::new(),
            missing_paths: Vec::new(),
            fixed: Vec::new(),
            radii: Vec::new(),
            edited: false,
        }
    };
    let steps = [step("Wait"), step("After")];
    let mut run = Run {
        began: Instant::now(),
        states: vec![RowState::Pending; steps.len()],
    };
    // Nothing may outlive the test, also when it fails: a stopped child that is left behind
    // holds the output of the test run open, and the run then never ends.
    let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        body(&mut env, &steps, &mut run)
    }));
    for row in env.shell.job_rows() {
        internal(&mut env, &format!("command kill -KILL %{}", row.number));
    }
    match out {
        Ok(out) => out,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

/// Runs `command` as the step: it stops itself at once, which is what Ctrl+Z does to it. Returns
/// the job number the plan would see.
fn stopped_step(env: &mut PlanEnv<'_>, command: &str) -> u32 {
    let known: Vec<u32> = env.shell.job_rows().iter().map(|row| row.number).collect();
    let prepared = env.shell.prepare_unaliased(command, true).unwrap();
    let result = env.shell.run_prepared(prepared, env.files);
    assert!(
        matches!(&result, LineResult::Ran(outcome) if is_stop_status(outcome.status().0)),
        "{result:?}"
    );
    new_stopped_job(env, &known).expect("the step is a new stopped job")
}

fn row_of(env: &PlanEnv<'_>, job: u32) -> Option<omsh_exec::JobRow> {
    env.shell
        .job_rows()
        .into_iter()
        .find(|row| row.number == job)
}

fn answers(list: Vec<view::PauseAnswer>) -> impl FnMut(Progress) -> view::PauseAnswer {
    let mut list: VecDeque<view::PauseAnswer> = list.into();
    move |_| {
        list.pop_front()
            .expect("the question was asked more often than expected")
    }
}

#[test]
fn only_the_statuses_of_a_stopped_program_start_the_question() {
    for status in [147, 148, 149, 150] {
        assert!(is_stop_status(status), "{status}");
    }
    for status in [0, 1, 2, 130, 137, 143, 146, 151, 255] {
        assert!(!is_stop_status(status), "{status}");
    }
}

#[test]
fn a_new_stopped_job_is_the_step_and_an_old_one_is_not() {
    paused_plan(|env, _, _| {
        let old = stopped_step(env, "sh -c 'kill -STOP $$'");
        let known: Vec<u32> = vec![old];
        assert_eq!(new_stopped_job(env, &known), None);
        let new = stopped_step(env, "sh -c 'kill -STOP $$'");
        assert_ne!(new, old);
        assert_eq!(new_stopped_job(env, &known), Some(new));
        // A job that runs is not "stopped".
        internal(env, "sleep 30 &");
        assert_eq!(new_stopped_job(env, &[old, new]), None);
    });
}

#[test]
fn stop_plan_leaves_the_step_stopped_in_the_job_table() {
    paused_plan(|env, steps, run| {
        let job = stopped_step(env, "sh -c 'kill -STOP $$; exit 7'");
        let mut ask = answers(vec![view::PauseAnswer::Stop]);
        assert!(matches!(
            pause(env, steps, run, 0, job, &mut ask),
            Paused::Stay
        ));
        assert!(matches!(
            row_of(env, job).unwrap().state,
            JobState::Stopped(_)
        ));
        assert_eq!(run.states[0], RowState::Paused);
    });
}

/// A stopped process holds SIGTERM until it is continued, so Skip must continue it too. The step
/// stops before it can reach `exit 7`: if the signal were sent without a continue, the job would
/// stay stopped for ever and Skip would give up after a second.
#[test]
fn skip_ends_a_stopped_step_even_though_a_stopped_process_holds_sigterm() {
    paused_plan(|env, steps, run| {
        let job = stopped_step(env, "sh -c 'kill -STOP $$; exit 7'");
        let mut ask = answers(vec![view::PauseAnswer::Skip]);
        let started = Instant::now();
        assert!(matches!(
            pause(env, steps, run, 0, job, &mut ask),
            Paused::Skipped
        ));
        assert!(started.elapsed() < SKIP_WAIT, "it took the whole wait");
        let row = row_of(env, job).unwrap();
        assert_eq!(row.state, JobState::Signaled("Terminated".to_owned()));
    });
}

#[test]
fn skip_gives_up_after_a_second_on_a_step_that_ignores_sigterm() {
    paused_plan(|env, steps, run| {
        let job = stopped_step(env, "sh -c 'trap \"\" TERM; kill -STOP $$; sleep 30'");
        let mut ask = answers(vec![view::PauseAnswer::Skip]);
        let started = Instant::now();
        assert!(matches!(
            pause(env, steps, run, 0, job, &mut ask),
            Paused::Stuck(n) if n == job
        ));
        assert!(started.elapsed() >= SKIP_WAIT);
        assert!(!row_of(env, job).unwrap().state.is_finished());
    });
}

#[test]
fn continue_runs_the_step_on_and_gives_its_status() {
    paused_plan(|env, steps, run| {
        let job = stopped_step(env, "sh -c 'kill -STOP $$; exit 5'");
        let mut ask = answers(vec![view::PauseAnswer::Continue]);
        assert!(matches!(
            pause(env, steps, run, 0, job, &mut ask),
            Paused::Ended(5)
        ));
        let job = stopped_step(env, "sh -c 'kill -STOP $$; exit 0'");
        let mut ask = answers(vec![view::PauseAnswer::Continue]);
        assert!(matches!(
            pause(env, steps, run, 0, job, &mut ask),
            Paused::Ended(0)
        ));
    });
}

#[test]
fn a_step_that_stops_again_after_continue_gets_the_question_again() {
    paused_plan(|env, steps, run| {
        let job = stopped_step(env, "sh -c 'kill -STOP $$; kill -STOP $$; exit 4'");
        let mut asked = 0;
        let mut ask = |_: Progress| {
            asked += 1;
            view::PauseAnswer::Continue
        };
        assert!(matches!(
            pause(env, steps, run, 0, job, &mut ask),
            Paused::Ended(4)
        ));
        assert_eq!(asked, 2);
    });
}

#[test]
fn to_background_lets_the_step_run_on_and_does_not_end_it() {
    paused_plan(|env, steps, run| {
        let job = stopped_step(env, "sh -c 'kill -STOP $$; sleep 30'");
        let mut ask = answers(vec![view::PauseAnswer::Background]);
        assert!(matches!(
            pause(env, steps, run, 0, job, &mut ask),
            Paused::Background(n) if n == job
        ));
        let end = Instant::now() + Duration::from_secs(5);
        while row_of(env, job).unwrap().state != JobState::Running {
            assert!(Instant::now() < end, "the job never ran again");
            std::thread::sleep(Duration::from_millis(10));
        }
    });
}

#[test]
fn the_question_sees_the_plan_with_the_step_paused_and_the_time_so_far() {
    paused_plan(|env, steps, run| {
        let job = stopped_step(env, "sh -c 'kill -STOP $$'");
        run.states[0] = RowState::Pending;
        let mut seen = Vec::new();
        let mut ask = |progress: Progress| {
            seen.push(progress);
            view::PauseAnswer::Stop
        };
        pause(env, steps, run, 0, job, &mut ask);
        let lines: Vec<String> = seen[0]
            .lines(&omsh_ui::caps::Caps::plain())
            .iter()
            .map(omsh_ui::text::Line::plain_text)
            .collect();
        assert!(
            lines.iter().any(|line| line.contains("2. After")),
            "{lines:?}"
        );
        assert!(
            lines.iter().any(|line| line.contains("1. Wait  (paused)")),
            "{lines:?}"
        );
    });
}

#[test]
fn the_lines_after_a_stop_in_the_background_name_the_job_and_the_steps_that_did_not_run() {
    paused_plan(|env, steps, _| {
        let result = left_in_the_background(env, steps, 1, 3);
        assert!(matches!(result, LineResult::Ran(_)));
        assert_eq!(env.shell.last_status().0, 0);
    });
}

// -- a command that is not found, fixed from the desktop entries ----------------------------------

/// The entries of a machine that has Google Chrome (under two entries, one program) and Telegram
/// (with an action that quits it).
fn chrome_and_telegram() -> Setup {
    Setup {
        apps: vec![
            (
                "google-chrome.desktop",
                "[Desktop Entry]\nType=Application\nName=Google Chrome\nExec={bin}/google-chrome-stable %U\n"
                    .to_owned(),
            ),
            (
                "com.google.Chrome.desktop",
                "[Desktop Entry]\nType=Application\nName=Google Chrome\nExec=google-chrome-stable %U\n"
                    .to_owned(),
            ),
            (
                "org.telegram.desktop.desktop",
                "[Desktop Entry]\nType=Application\nName=Telegram\nTryExec=Telegram\n\
                 Exec=Telegram -- %U\nActions=Quit;\n\n\
                 [Desktop Action Quit]\nName=Quit Telegram\nExec=Telegram -quit\n"
                    .to_owned(),
            ),
        ],
        programs: vec!["google-chrome-stable", "Telegram"],
        ..Setup::default()
    }
}

#[test]
fn a_missing_command_is_replaced_by_the_program_of_its_desktop_entry() {
    let answer = json(&[("Open", "google-chrome https://example.org &", "low")]);
    let ran = go(says(&answer), "x", chrome_and_telegram());
    let err = &ran.err;
    assert!(
        err.contains("     fixed: google-chrome -> google-chrome-stable (Google Chrome)\n"),
        "{err}"
    );
    // Everything but the command word is as the model wrote it.
    assert!(
        err.contains("     $ google-chrome-stable https://example.org &\n"),
        "{err}"
    );
    assert!(!err.contains("not found:"), "{err}");
    assert!(!err.contains("$ google-chrome https"), "{err}");
}

#[test]
fn the_telegram_entry_fixes_its_name_and_keeps_the_parentheses_and_the_ampersand() {
    let answer = json(&[("Open", "(telegram-desktop &)", "low")]);
    let ran = go(says(&answer), "x", chrome_and_telegram());
    let err = &ran.err;
    assert!(
        err.contains("     fixed: telegram-desktop -> Telegram (Telegram)\n"),
        "{err}"
    );
    assert!(err.contains("     $ (Telegram &)\n"), "{err}");
    // The action of the entry ("Quit Telegram", `Telegram -quit`) is nowhere.
    assert!(!err.contains("quit"), "{err}");
    assert!(!err.contains("not found:"), "{err}");
}

#[test]
fn the_fixed_command_is_the_one_that_runs() {
    let answer = json(&[("Open", "google-chrome", "low")]);
    let mut setup = chrome_and_telegram();
    setup.keys = vec![enter()];
    let ran = go(says(&answer), "x", setup);
    // `google-chrome-stable` is a link to `true` here: 0, where an unfixed step would be 127.
    assert_eq!(ran.status(), 0, "{}", ran.err);
    assert!(ran.err.contains("plan done"), "{}", ran.err);
}

#[test]
fn only_the_command_words_that_are_not_found_are_changed() {
    let answer = json(&[(
        "Open",
        "google-chrome a; echo google-chrome; google-chrome b | omsh-no-such-program",
        "low",
    )]);
    let ran = go(says(&answer), "x", chrome_and_telegram());
    let err = &ran.err;
    assert!(
        err.contains(
            "     $ google-chrome-stable a; echo google-chrome; google-chrome-stable b | \
             omsh-no-such-program\n"
        ),
        "{err}"
    );
    // One line for the name, even though it was replaced twice, and the other missing name keeps
    // its note.
    assert_eq!(err.matches("     fixed: ").count(), 1, "{err}");
    assert!(
        err.contains("     not found: omsh-no-such-program\n"),
        "{err}"
    );
}

#[test]
fn a_program_that_is_found_is_left_alone_even_when_an_entry_names_it() {
    let answer = json(&[("Open", "google-chrome-stable --x", "low")]);
    let ran = go(says(&answer), "x", chrome_and_telegram());
    assert!(!ran.err.contains("fixed:"), "{}", ran.err);
    assert!(
        ran.err.contains("     $ google-chrome-stable --x\n"),
        "{}",
        ran.err
    );
}

#[test]
fn two_different_programs_leave_the_note_as_it_was() {
    let mut setup = chrome_and_telegram();
    // A second program that an entry also calls "Google Chrome" (and `-beta` is not an ending
    // that is ignored, so only the name makes it a candidate).
    setup.programs.push("google-chrome-beta");
    setup.apps.push((
        "beta.desktop",
        "[Desktop Entry]\nType=Application\nName=Google Chrome\nExec={bin}/google-chrome-beta %U\n"
            .to_owned(),
    ));
    let answer = json(&[("Open", "google-chrome", "low")]);
    let ran = go(says(&answer), "x", setup);
    assert!(!ran.err.contains("fixed:"), "{}", ran.err);
    assert!(
        ran.err.contains("     not found: google-chrome\n"),
        "{}",
        ran.err
    );
    assert!(ran.err.contains("     $ google-chrome\n"), "{}", ran.err);
}

#[test]
fn an_entry_whose_program_is_not_installed_is_not_a_fix() {
    let mut setup = chrome_and_telegram();
    setup
        .programs
        .retain(|name| *name != "google-chrome-stable");
    let answer = json(&[("Open", "google-chrome", "low")]);
    let ran = go(says(&answer), "x", setup);
    assert!(!ran.err.contains("fixed:"), "{}", ran.err);
    assert!(
        ran.err.contains("     not found: google-chrome\n"),
        "{}",
        ran.err
    );
}

#[test]
fn with_no_entry_folders_nothing_changes_and_nothing_is_said() {
    // The made-up machine has no entries at all: no files, no programs.
    let answer = json(&[("Open", "google-chrome", "low")]);
    let ran = go(
        says(&answer),
        "x",
        Setup {
            programs: vec!["ls"],
            ..Setup::default()
        },
    );
    assert!(!ran.err.contains("fixed:"), "{}", ran.err);
    assert!(
        ran.err.contains("     not found: google-chrome\n"),
        "{}",
        ran.err
    );
    assert!(ran.err.contains("     $ google-chrome\n"), "{}", ran.err);
}

#[test]
fn a_fixed_step_is_still_rated_by_the_model_and_by_the_analyzer() {
    // The analyzer judges the command as it now reads: a fixed `rm` is not a thing here, so the
    // level and the model's estimate decide as for any other step.
    let answer = json(&[("Open", "google-chrome", "high")]);
    let ran = go(says(&answer), "x", chrome_and_telegram());
    assert!(ran.err.contains("1. Open   [dangerous"), "{}", ran.err);
    assert!(
        ran.err
            .contains("fixed: google-chrome -> google-chrome-stable"),
        "{}",
        ran.err
    );
}

#[test]
fn editing_a_step_to_a_missing_name_fixes_it_too() {
    let answer = json(&[("Look", "ls", "low")]);
    let mut setup = chrome_and_telegram();
    setup.programs.push("ls");
    setup.keys = vec![ch('e'), enter()];
    setup.edits = vec![Edited::Text("google-chrome --new-window".to_owned())];
    let ran = go(says(&answer), "x", setup);
    let err = &ran.err;
    let after = &err[err.find("(edited)").map_or(0, |at| at.saturating_sub(200))..];
    assert!(
        after.contains("     $ google-chrome-stable --new-window\n"),
        "{err}"
    );
    assert_eq!(err.matches("     fixed: ").count(), 1, "{err}");
    assert!(!err.contains("not found:"), "{err}");
}

#[test]
fn the_name_of_the_application_cannot_carry_control_characters_to_the_screen() {
    let mut setup = chrome_and_telegram();
    setup.apps.push((
        "evil.desktop",
        "[Desktop Entry]\nType=Application\nName=Evil\x1b[31m Tool\nExec=evil-tool\n".to_owned(),
    ));
    setup.programs.push("evil-tool");
    let answer = json(&[("Open", "evil", "low")]);
    let ran = go(says(&answer), "x", setup);
    assert!(
        ran.err.contains("fixed: evil -> evil-tool (Evil[31m Tool)"),
        "{:?}",
        ran.err
    );
    assert!(!ran.err.contains('\x1b'), "{:?}", ran.err);
}

#[test]
fn the_entries_are_read_only_when_something_is_missing_and_only_once() {
    let machine = Dir::new();
    let share = machine.file("share/applications");
    fs::create_dir_all(&share).unwrap();
    fs::write(
        format!("{share}/a.desktop"),
        "[Desktop Entry]\nType=Application\nName=A\nExec=a-tool\n",
    )
    .unwrap();
    let mut shell = Shell::new();
    shell.run_source(
        &format!(
            "XDG_DATA_HOME='{}' XDG_DATA_DIRS='{}'",
            machine.file("none"),
            machine.file("share")
        ),
        true,
        &mut Io::process(),
    );
    let mut entries = None;
    // Everything is found: no read.
    let (command, fixes) = fix_missing(&shell, "cd / && true", &mut entries);
    assert_eq!(command, "cd / && true");
    assert!(fixes.is_empty());
    assert!(entries.is_none(), "nothing was missing, nothing is read");
    // Something is missing: read once, and kept for the next step.
    fix_missing(&shell, "omsh-no-such-program", &mut entries);
    assert_eq!(entries.as_ref().map(Vec::len), Some(1));
    fs::remove_file(format!("{share}/a.desktop")).unwrap();
    fix_missing(&shell, "omsh-other-missing", &mut entries);
    assert_eq!(entries.as_ref().map(Vec::len), Some(1), "not read again");
}

#[test]
fn the_panel_with_buttons_colors_the_fixed_label_like_the_not_found_one() {
    use omsh_ui::text::Tone;
    let lines = view::body_lines("     fixed: gti -> git (Git)\n", 80);
    assert_eq!(lines.len(), 1);
    let spans = lines[0].spans();
    assert_eq!(spans[0].text(), "fixed:");
    assert_eq!(spans[0].style.tone, Tone::Warn);
    assert_eq!(spans[1].text(), " gti -> git (Git)");
    assert_eq!(spans[1].style.tone, Tone::Plain);
}

#[test]
fn a_web_app_shortcut_of_the_user_does_not_stop_the_browser_from_being_fixed() {
    // The shape of a real machine: two entries of Chrome (one is `NoDisplay`) and a shortcut of
    // the user's own that runs the browser's real binary, which is not in `PATH`.
    let mut setup = chrome_and_telegram();
    setup.outside = vec!["google-chrome"];
    setup.apps.push((
        "a-chrome-hnpfjngllnobngcgfapefoaidbinmjnm-Default.desktop",
        "[Desktop Entry]\nType=Application\nName=WhatsApp Web\n\
         Exec={opt}/google-chrome --profile-directory=Default --app-id=hnpf\n"
            .to_owned(),
    ));
    setup.apps.push((
        "com.google.Chrome.desktop",
        "[Desktop Entry]\nType=Application\nName=Google Chrome\nNoDisplay=true\n\
         Exec={bin}/google-chrome-stable %U\n"
            .to_owned(),
    ));
    let answer = json(&[("Open", "google-chrome https://example.org", "low")]);
    let ran = go(says(&answer), "x", setup);
    let err = &ran.err;
    // As on a real machine, the entries write the program as an absolute path.
    let line = err
        .lines()
        .find(|line| line.starts_with("     fixed: "))
        .unwrap_or_else(|| panic!("{err}"));
    assert!(
        line.starts_with("     fixed: google-chrome -> /")
            && line.ends_with("/bin/google-chrome-stable (Google Chrome)"),
        "{line}"
    );
    assert!(
        err.contains("/bin/google-chrome-stable https://example.org\n"),
        "{err}"
    );
    assert!(!err.contains("not found:"), "{err}");
    assert!(!err.contains("WhatsApp"), "{err}");
}

#[test]
fn a_fix_found_only_by_its_program_shows_no_application_name() {
    // Only the shortcut runs a program called `google-chrome`: it fixes the name, but its own name
    // ("WhatsApp Web") is another application and is not shown.
    let setup = Setup {
        apps: vec![(
            "shortcut.desktop",
            "[Desktop Entry]\nType=Application\nName=WhatsApp Web\n\
             Exec={opt}/google-chrome --app-id=x\n"
                .to_owned(),
        )],
        programs: vec!["ls"],
        outside: vec!["google-chrome"],
        ..Setup::default()
    };
    let answer = json(&[("Open", "google-chrome", "low")]);
    let ran = go(says(&answer), "x", setup);
    let line = ran
        .err
        .lines()
        .find(|line| line.starts_with("     fixed: "))
        .unwrap_or_else(|| panic!("{}", ran.err));
    assert!(
        line.starts_with("     fixed: google-chrome -> ") && line.ends_with("/opt/google-chrome"),
        "{line}"
    );
    assert!(!ran.err.contains("WhatsApp"), "{}", ran.err);
}

// -- paths that do not exist, before anything runs --------------------------------------------

/// A made-up place: `base/work` (where the shell starts) and `base/night-before`. Returns the
/// base folder with links resolved, and the setup line that moves the shell into `work`.
fn place(dir: &Dir) -> (String, &'static str) {
    fs::create_dir_all(dir.file("work")).unwrap();
    fs::create_dir_all(dir.file("night-before")).unwrap();
    let base = fs::canonicalize(dir.file("")).unwrap();
    let line: &'static str = Box::leak(format!("cd '{}/work'", base.display()).into_boxed_str());
    (base.to_string_lossy().into_owned(), line)
}

/// How many `no such path:` notes the panels printed.
fn path_notes(err: &str) -> usize {
    err.matches("     no such path: ").count()
}

#[test]
fn a_path_that_does_not_exist_is_noted_with_the_name_that_was_meant() {
    let dir = Dir::new();
    let (_, cd) = place(&dir);
    let answer = json(&[
        ("Open", "code ../night-before-codebase", "low"),
        ("Look", "ls", "low"),
    ]);
    let ran = go(
        says(&answer),
        "x",
        Setup {
            aliases: vec![cd],
            keys: vec![Ok(Key::Escape)],
            ..Setup::default()
        },
    );
    let err = &ran.err;
    let first = err.find("1. Open").expect("step 1");
    let second = err.find("2. Look").expect("step 2");
    assert!(
        err[first..second].contains(
            "     no such path: ../night-before-codebase; did you mean ../night-before?\n"
        ),
        "{err}"
    );
    assert_eq!(path_notes(err), 1, "{err}");
    // A note, not a refusal: the level is the same and the plan is still asked about.
    assert!(err.contains("1. Open   [safe]"), "{err}");
    assert_eq!(ran.key_prompts.len(), 1, "{:?}", ran.key_prompts);
    assert!(err.contains("cancelled; nothing ran"), "{err}");
}

#[test]
fn the_command_is_not_rewritten_for_a_missing_path() {
    let dir = Dir::new();
    let (_, cd) = place(&dir);
    let answer = json(&[("Open", "cat ../night-before-codebase", "low")]);
    let ran = go(
        says(&answer),
        "x",
        Setup {
            aliases: vec![cd],
            keys: vec![Ok(Key::Escape)],
            ..Setup::default()
        },
    );
    assert!(
        ran.err.contains("     $ cat ../night-before-codebase\n"),
        "{}",
        ran.err
    );
    assert!(!ran.err.contains("     fixed:"), "{}", ran.err);
}

#[test]
fn an_absolute_path_and_a_home_path_are_noted_too() {
    let dir = Dir::new();
    let (base, _) = place(&dir);
    let command = format!("ls {base}/night-befor");
    let answer = json(&[("Look", &command, "low")]);
    let ran = go(says(&answer), "x", Setup::default());
    assert!(
        ran.err.contains(&format!(
            "     no such path: {base}/night-befor; did you mean {base}/night-before?\n"
        )),
        "{}",
        ran.err
    );
    // With nothing near, only the path.
    let command = format!("ls {base}/zzzzzz");
    let answer = json(&[("Look", &command, "low")]);
    let ran = go(says(&answer), "x", Setup::default());
    assert!(
        ran.err
            .contains(&format!("     no such path: {base}/zzzzzz\n")),
        "{}",
        ran.err
    );
}

#[test]
fn a_name_that_an_earlier_step_creates_or_names_is_not_noted() {
    let dir = Dir::new();
    let (_, cd) = place(&dir);
    for steps in [
        vec![("Make", "mkdir fresh", "low"), ("Go", "cd fresh", "low")],
        vec![
            ("Clone", "git clone https://example.org/fresh.git", "low"),
            ("Go", "cd fresh", "low"),
        ],
        vec![("Both", "mkdir -p fresh && cd fresh", "low")],
    ] {
        let answer = json(&steps);
        let ran = go(
            says(&answer),
            "x",
            Setup {
                aliases: vec![cd],
                keys: vec![Ok(Key::Escape)],
                ..Setup::default()
            },
        );
        assert_eq!(path_notes(&ran.err), 0, "{steps:?}\n{}", ran.err);
    }
}

#[test]
fn the_steps_after_a_missing_folder_do_not_note_it_again() {
    let dir = Dir::new();
    let (_, cd) = place(&dir);
    let answer = json(&[
        ("Go", "cd ../night-before-codebase", "low"),
        ("Look", "ls src", "low"),
        ("Open", "code .", "low"),
    ]);
    let ran = go(
        says(&answer),
        "x",
        Setup {
            aliases: vec![cd],
            keys: vec![Ok(Key::Escape)],
            ..Setup::default()
        },
    );
    assert_eq!(path_notes(&ran.err), 1, "{}", ran.err);
}

#[test]
fn a_folder_that_is_not_known_gives_no_note_for_a_relative_path() {
    let dir = Dir::new();
    let (_, cd) = place(&dir);
    let answer = json(&[
        ("Go", "cd \"$somewhere\"", "low"),
        ("Look", "ls zzzzzz", "low"),
    ]);
    let ran = go(
        says(&answer),
        "x",
        Setup {
            aliases: vec![cd],
            keys: vec![Ok(Key::Escape)],
            ..Setup::default()
        },
    );
    assert_eq!(path_notes(&ran.err), 0, "{}", ran.err);
}

#[test]
fn cdpath_keeps_a_bare_cd_name_quiet_but_not_other_commands() {
    let dir = Dir::new();
    let (_, cd) = place(&dir);
    let answer = json(&[("Look", "ls yyyyyy", "low"), ("Go", "cd zzzzzz", "low")]);
    let ran = go(
        says(&answer),
        "x",
        Setup {
            aliases: vec![cd, "CDPATH=/"],
            keys: vec![Ok(Key::Escape)],
            ..Setup::default()
        },
    );
    assert!(!ran.err.contains("no such path: zzzzzz"), "{}", ran.err);
    assert!(
        ran.err.contains("     no such path: yyyyyy\n"),
        "{}",
        ran.err
    );
}

#[test]
fn commands_where_a_missing_path_is_normal_get_no_note() {
    let dir = Dir::new();
    let (_, cd) = place(&dir);
    let answer = json(&[
        ("a", "mkdir zzzzzz", "low"),
        ("b", "touch yyyyyy", "low"),
        ("c", "cp a xxxxxx", "low"),
        ("d", "mv a wwwwww", "low"),
        ("e", "echo hi > vvvvvv/out", "low"),
        ("f", "cat < uuuuuu", "low"),
    ]);
    let ran = go(
        says(&answer),
        "x",
        Setup {
            aliases: vec![cd],
            keys: vec![Ok(Key::Escape)],
            ..Setup::default()
        },
    );
    assert_eq!(path_notes(&ran.err), 0, "{}", ran.err);
}

#[test]
fn editing_a_step_updates_its_path_note() {
    let dir = Dir::new();
    let (_, cd) = place(&dir);
    let answer = json(&[("Look", "ls", "low")]);
    let ran = go(
        says(&answer),
        "x",
        Setup {
            aliases: vec![cd],
            keys: vec![ch('e'), enter(), ch('e'), enter()],
            edits: vec![
                Edited::Text("ls ../night-before-codebase".to_owned()),
                Edited::Text("ls ../night-before".to_owned()),
            ],
            ..Setup::default()
        },
    );
    // Only the panel after the first edit has the note.
    assert_eq!(path_notes(&ran.err), 1, "{}", ran.err);
    assert!(
        ran.err.contains("did you mean ../night-before?\n"),
        "{}",
        ran.err
    );
}

#[test]
fn an_edit_that_adds_a_folder_in_an_earlier_step_silences_the_later_note() {
    let dir = Dir::new();
    let (_, cd) = place(&dir);
    let answer = json(&[("Make", "true", "low"), ("Go", "cd fresh", "low")]);
    let ran = go(
        says(&answer),
        "x",
        Setup {
            aliases: vec![cd],
            keys: vec![ch('e'), ch('1'), enter(), Ok(Key::Escape)],
            edits: vec![Edited::Text("mkdir fresh".to_owned())],
            ..Setup::default()
        },
    );
    // The first panel notes `fresh`; once step 1 mentions it, the note is gone.
    assert_eq!(path_notes(&ran.err), 1, "{}", ran.err);
    assert!(ran.err.contains("     $ mkdir fresh\n"), "{}", ran.err);
}

#[test]
fn the_panel_with_buttons_colors_the_path_label_like_the_command_label() {
    use omsh_ui::text::Tone;
    let lines = view::body_lines("     no such path: ../a; did you mean ../b?\n", 80);
    assert_eq!(lines.len(), 1);
    let spans = lines[0].spans();
    assert_eq!(spans[0].text(), "no such path:");
    assert_eq!(spans[0].style.tone, Tone::Warn);
    assert_eq!(spans[1].text(), " ../a; did you mean ../b?");
    assert_eq!(spans[1].style.tone, Tone::Plain);
}
