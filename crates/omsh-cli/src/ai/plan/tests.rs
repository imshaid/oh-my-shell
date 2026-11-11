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

use super::*;

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
        r#"{{"summary":"Do the thing","steps":[{}],"language":"en"}}"#,
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
}

fn go(fake: FakeProvider, request: &str, setup: Setup) -> Ran {
    let fake = Arc::new(fake);
    let mut shell = Shell::new();
    let mut files = Io::process();
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
    let plan_setup = PlanSetup {
        provider: Box::new(Arc::clone(&fake)),
        settings: Settings {
            kind: Kind::Ollama,
            model: "test-model".to_owned(),
            base_url: None,
        },
        language: AiLanguage::Auto,
        context: setup.context.unwrap_or_default(),
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
            confirm: ConfirmLevel::Dangerous,
            unicode: false,
            edit: &mut edit,
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
        ["[Enter] run  [e] edit  [v] view context  [Esc] cancel "]
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
        text: String::new(),
        analysis: Analysis::default(),
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
        unicode: false,
        edit: &mut edit,
    };
    let result = execute(&mut env, &[step, marker]);
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
        ("Make", "echo made > here", "low"),
    ]);
    let ran = go(
        says(&answer),
        "x",
        Setup {
            keys: vec![enter()],
            ..Setup::default()
        },
    );
    assert!(dir.has("here"), "the cd of step 1 holds for step 2");
    assert_eq!(ran.status(), 0);
    assert!(ran.err.contains("[1/2] Go") && ran.err.contains("[2/2] Make"));
    assert!(ran.err.contains("plan done (2 steps)"));
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
fn aliases_apply_and_the_panel_says_what_will_run() {
    let dir = Dir::new();
    let command = format!("hi > {}", dir.file("out"));
    let answer = json(&[("Greet", &command, "low")]);
    let ran = go(
        says(&answer),
        "x",
        Setup {
            keys: vec![enter()],
            aliases: vec!["alias hi='echo aliased'"],
            ..Setup::default()
        },
    );
    assert!(ran.err.contains("runs as: echo aliased > "), "{}", ran.err);
    assert_eq!(fs::read_to_string(dir.file("out")).unwrap(), "aliased\n");
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

// -- asking ---------------------------------------------------------------------------------

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
    assert_eq!(choose(&mut *terminal, 1, ""), Choice::Edit(0));
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
