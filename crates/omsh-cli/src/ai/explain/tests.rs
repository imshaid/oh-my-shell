use std::collections::VecDeque;
use std::fs;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use omsh_ai::fake::FakeProvider;
use omsh_ai::{ErrorKind, LEFT_OUT, ProviderError};
use omsh_config::Provider as Kind;
use omsh_exec::ReadError;
use omsh_history::Filter;
use omsh_ui::Paint;

use super::*;
use crate::ai::plan::Connect;

/// Limits whose time budget cannot run out, so a busy machine cannot change a count.
const LIMITS: BlastLimits = BlastLimits {
    max_entries: 50_000,
    max_ms: 600_000,
};

// -- test doubles ---------------------------------------------------------------------------

#[derive(Default)]
struct State {
    lines: VecDeque<Result<String, ReadError>>,
    out: String,
    err: String,
}

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
    fn out(&mut self, text: &str) {
        self.0.borrow_mut().out.push_str(text);
    }
    fn err(&mut self, text: &str) {
        self.0.borrow_mut().err.push_str(text);
    }
    fn read_line(&mut self, _p: &str, _h: bool, line: &mut Vec<u8>) -> Result<(), ReadError> {
        line.clear();
        match self.0.borrow_mut().lines.pop_front() {
            Some(Ok(text)) => {
                line.extend_from_slice(text.as_bytes());
                Ok(())
            }
            Some(Err(error)) => Err(error),
            None => Err(ReadError::EndOfInput),
        }
    }
    fn interrupted(&self) -> bool {
        false
    }
}

struct Dir(PathBuf);

impl Dir {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("omsh-explain-{}-{n}", std::process::id()));
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

struct Ran {
    result: LineResult,
    out: String,
    err: String,
    requests: Vec<omsh_ai::ChatRequest>,
    connects: u32,
    alive: usize,
    shell: Shell,
    recent: Recent,
}

impl Ran {
    fn status(&self) -> u8 {
        match &self.result {
            LineResult::Ran(outcome) => outcome.status().0,
            other => panic!("not run: {other:?}"),
        }
    }
    /// The text of the one message that was sent.
    fn sent(&self) -> &str {
        &self.requests.last().expect("no request was sent").messages[0].text
    }
    fn system(&self) -> &str {
        self.requests
            .last()
            .unwrap()
            .system
            .as_deref()
            .unwrap_or_default()
    }
}

struct Setup {
    /// Answers to the question before a risky command.
    answers: Vec<Result<String, ReadError>>,
    /// Lines the shell already ran: (line, status).
    recent: Vec<(&'static str, i32)>,
    ignore: Vec<String>,
    /// The AI is not set up.
    unavailable: bool,
    status_before: u8,
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            answers: Vec::new(),
            recent: Vec::new(),
            ignore: vec![" *".to_owned()],
            unavailable: false,
            status_before: 0,
        }
    }
}

enum Flow<'a> {
    Command(&'a str),
    Last { typed: bool },
    Pipe { command: &'a str, question: &'a str },
}

fn go(fake: FakeProvider, flow: Flow<'_>, setup: Setup) -> Ran {
    let fake = Arc::new(fake);
    let mut shell = Shell::new();
    shell.set_last_status(ExitStatus(setup.status_before));
    let mut files = Io::process();
    // The question before a risky command is read by the shell from its own standard input
    // (`run_line` opens its own terminal on `files`), so the answers are put there. This also
    // keeps the tests from ever reading the real standard input.
    let (answers, writer) = std::io::pipe().unwrap();
    {
        use std::io::Write as _;
        let mut writer = writer;
        // An answer that is an error stands for "the input ended": nothing is written.
        for text in setup.answers.iter().flatten() {
            writeln!(writer, "{text}").unwrap();
        }
    }
    files.set(0, Arc::new(OwnedFd::from(answers)));
    let mut tty = Script(RefCell::new(State::default()));
    let mut recent = Recent::new(Filter::new(&setup.ignore));
    for (line, status) in &setup.recent {
        recent.record(line, *status);
    }
    let connects = Arc::new(AtomicU32::new(0));
    let make = |connects: &Arc<AtomicU32>, fake: &Arc<FakeProvider>| -> PlanSetup {
        let connect: Connect = Box::new({
            let fake = Arc::clone(fake);
            let connects = Arc::clone(connects);
            move || {
                connects.fetch_add(1, Ordering::Relaxed);
                Ok(Box::new(Arc::clone(&fake)) as Box<dyn Provider>)
            }
        });
        PlanSetup {
            connect,
            settings: Settings {
                kind: Kind::Ollama,
                model: "test-model".to_owned(),
                base_url: None,
            },
            context: omsh_config::AiContext::default(),
        }
    };
    let available = |connects: &Arc<AtomicU32>, fake: &Arc<FakeProvider>| {
        if setup.unavailable {
            Err("AI is not set up; run `omsh ai setup`".to_owned())
        } else {
            Ok(make(connects, fake))
        }
    };
    let result = {
        let env = ExplainEnv {
            shell: &mut shell,
            files: &mut files,
            tty: &mut tty,
            recent: &mut recent,
            paint: &Paint::plain(),
        };
        match flow {
            Flow::Command(text) => match available(&connects, &fake) {
                Ok(setup) => command(setup, text, env),
                Err(why) => {
                    let mut env = env;
                    unavailable(&mut env, "??", &why)
                }
            },
            Flow::Last { typed } => last(|| available(&connects, &fake), typed, env),
            Flow::Pipe { command, question } => pipe(
                available(&connects, &fake),
                command,
                question,
                PipeEnv {
                    confirm: ConfirmLevel::Dangerous,
                    blast: LIMITS,
                    undo: None,
                },
                env,
            ),
        }
    };
    let state = tty.0.into_inner();
    Ran {
        result,
        out: state.out,
        err: state.err,
        requests: fake.requests(),
        connects: connects.load(Ordering::Relaxed),
        alive: Arc::strong_count(&fake) - 1,
        shell,
        recent,
    }
}

fn says(pieces: &[&str]) -> FakeProvider {
    FakeProvider::new(Kind::Ollama).says(pieces)
}

fn ghp() -> String {
    format!("ghp_{}", "a1B2".repeat(9))
}

// -- the tokens line (S28) -----------------------------------------------------------------

#[test]
fn the_tokens_the_server_reported_are_shown_after_the_answer_with_the_real_time() {
    let ran = go(
        FakeProvider::new(Kind::Ollama)
            .says(&["It lists files."])
            .reporting(omsh_ai::Usage {
                input: Some(1204),
                output: Some(310),
            }),
        Flow::Command("ls"),
        Setup::default(),
    );
    assert_eq!(ran.status(), 0);
    // The answer is on standard output, the figures on standard error.
    assert_eq!(ran.out, "It lists files.\n");
    let line = ran.err.lines().last().expect("a line on standard error");
    assert!(
        line.starts_with("  ") && line.ends_with(" s, 1,204 tokens in, 310 out"),
        "{:?}",
        ran.err
    );
    assert_eq!(ran.alive, 0);
}

#[test]
fn a_server_that_reports_no_tokens_gets_no_tokens_line() {
    let ran = go(
        says(&["It lists files."]),
        Flow::Command("ls"),
        Setup::default(),
    );
    assert_eq!(ran.status(), 0);
    assert!(!ran.err.contains("tokens"), "{:?}", ran.err);
    assert!(!ran.err.contains(" s,"), "{:?}", ran.err);
}

#[test]
fn a_failed_request_shows_no_tokens() {
    let ran = go(
        FakeProvider::new(Kind::Ollama).fails(ProviderError::new(ErrorKind::Auth, "401")),
        Flow::Command("ls"),
        Setup::default(),
    );
    assert!(!ran.err.contains("tokens"), "{:?}", ran.err);
}

// -- ?? <command or topic> -------------------------------------------------------------------

#[test]
fn an_explanation_is_streamed_to_standard_output_and_nothing_runs() {
    let dir = Dir::new();
    let text = format!("touch {}", dir.file("made"));
    let ran = go(
        says(&["It ", "creates a file", ".\n", "Done"]),
        Flow::Command(&text),
        Setup::default(),
    );
    assert_eq!(ran.status(), 0);
    assert_eq!(
        ran.out, "It creates a file.\nDone\n",
        "ends with a line feed"
    );
    assert!(!dir.has("made"), "the command must not run");
    assert_eq!(ran.sent(), format!("Explain this:\n{text}"));
    assert!(!ran.requests[0].json);
    assert_eq!(
        (ran.connects, ran.alive),
        (1, 0),
        "no provider outlives the request"
    );
    assert!(ran.err.contains("(test-model)"), "{}", ran.err);
}

#[test]
fn the_shells_own_check_is_shown_locally_and_the_command_is_still_only_explained() {
    let ran = go(
        says(&["Deletes everything.\n"]),
        Flow::Command("rm -rf /"),
        Setup::default(),
    );
    assert!(
        ran.err
            .contains("oh-my-shell: the shell's own check: critical:"),
        "{}",
        ran.err
    );
    // The check is for the person; the model got only the text.
    assert!(!ran.sent().contains("shell's own check"));
    assert_eq!(ran.status(), 0);
    // A harmless command says nothing locally.
    let ran = go(says(&["ok\n"]), Flow::Command("ls -l"), Setup::default());
    assert!(!ran.err.contains("own check"), "{}", ran.err);
    // A topic that is not a command says nothing either.
    let ran = go(
        says(&["ok\n"]),
        Flow::Command("how do I unpack a tarball (it's big)"),
        Setup::default(),
    );
    assert!(!ran.err.contains("own check"), "{}", ran.err);
    assert_eq!(ran.status(), 0);
}

#[test]
fn a_secret_in_the_text_is_masked_and_said_so() {
    let key = ghp();
    let ran = go(
        says(&["ok\n"]),
        Flow::Command(&format!("curl -H 'Authorization: Bearer {key}' x")),
        Setup::default(),
    );
    assert!(!ran.sent().contains(&key));
    assert!(ran.sent().contains("[MASKED]"));
    assert!(ran.err.contains("a secret"), "{}", ran.err);
}

#[test]
fn control_characters_in_the_answer_do_not_reach_the_terminal() {
    let ran = go(
        says(&["safe \x1b]0;title\x07text", " \x1b[31mred\x1b[0m\n"]),
        Flow::Command("ls"),
        Setup::default(),
    );
    assert!(
        !ran.out.contains('\x1b') && !ran.out.contains('\x07'),
        "{:?}",
        ran.out
    );
    assert!(ran.out.contains("safe "));
}

#[test]
fn errors_are_one_line_with_status_1_and_nothing_on_standard_output() {
    let ran = go(
        FakeProvider::new(Kind::Ollama).fails(ProviderError::new(ErrorKind::Auth, "401")),
        Flow::Command("ls"),
        Setup::default(),
    );
    assert_eq!(ran.status(), 1);
    assert_eq!(ran.out, "");
    assert!(ran.err.contains("oh-my-shell: ??:"), "{}", ran.err);
    assert!(ran.err.contains("did not accept the key"), "{}", ran.err);
    assert_eq!(ran.alive, 0);
}

#[test]
fn an_answer_that_breaks_off_keeps_what_was_shown_and_ends_the_line() {
    let ran = go(
        FakeProvider::new(Kind::Ollama)
            .partial_then_fail(
                &["half an ans"],
                ProviderError::new(ErrorKind::Server, "500"),
            )
            .says(&["whole"]),
        Flow::Command("ls"),
        Setup::default(),
    );
    assert_eq!(ran.status(), 1);
    assert_eq!(ran.out, "half an ans\n");
    assert_eq!(ran.requests.len(), 1, "no second try after text was shown");
    assert!(ran.err.contains("error on its side"), "{}", ran.err);
}

#[test]
fn an_empty_answer_is_an_error_not_silence() {
    let ran = go(says(&[""]), Flow::Command("ls"), Setup::default());
    assert_eq!(ran.status(), 1);
    assert!(ran.err.contains("no answer"), "{}", ran.err);
}

#[test]
fn without_ai_the_explanation_says_why_and_sends_nothing() {
    let ran = go(
        says(&["x"]),
        Flow::Command("ls"),
        Setup {
            unavailable: true,
            ..Setup::default()
        },
    );
    assert_eq!(ran.status(), 1);
    assert!(ran.err.contains("AI is not set up"), "{}", ran.err);
    assert_eq!(ran.connects, 0);
}

// -- ?? alone and Alt+? ----------------------------------------------------------------------

#[test]
fn the_last_failed_command_and_its_status_are_sent_and_the_error_text_is_not() {
    let ran = go(
        says(&["Probably a missing target.\n"]),
        Flow::Last { typed: true },
        Setup {
            recent: vec![("ls", 0), ("make build", 2)],
            ..Setup::default()
        },
    );
    assert_eq!(ran.status(), 0);
    assert_eq!(ran.out, "Probably a missing target.\n");
    assert!(ran.sent().contains("make build"));
    assert!(ran.sent().ends_with("Exit status: 2"));
    assert!(!ran.sent().contains("ls"));
    assert!(ran.system().contains("NOT captured"));
    assert!(ran.err.contains("error text is not sent"), "{}", ran.err);
    assert!(
        ran.err.contains("2>&1 | ? why did this fail"),
        "{}",
        ran.err
    );
}

#[test]
fn a_signal_status_is_named() {
    let ran = go(
        says(&["x\n"]),
        Flow::Last { typed: true },
        Setup {
            recent: vec![("./crash", 139)],
            ..Setup::default()
        },
    );
    assert!(ran.sent().contains("SIGSEGV"), "{}", ran.sent());
}

#[test]
fn nothing_is_sent_when_there_is_nothing_to_explain() {
    let cases: [(Vec<(&'static str, i32)>, &str); 5] = [
        (vec![], "succeeded"),
        (vec![("true", 0)], "succeeded"),
        (vec![("sleep 9", 130)], "Ctrl+C"),
        (vec![("vim", 148)], "stopped"),
        (vec![("false", 1), ("true", 0)], "succeeded"),
    ];
    for (recent, word) in cases {
        let ran = go(
            says(&["never"]),
            Flow::Last { typed: true },
            Setup {
                recent: recent.clone(),
                ..Setup::default()
            },
        );
        assert_eq!(ran.status(), 2, "{recent:?}");
        assert!(ran.err.contains(word), "{recent:?}: {}", ran.err);
        assert_eq!((ran.connects, ran.out.as_str()), (0, ""), "{recent:?}");
    }
}

#[test]
fn a_last_line_that_the_history_ignores_is_never_sent_and_hides_the_one_before() {
    let ran = go(
        says(&["never"]),
        Flow::Last { typed: true },
        Setup {
            // `make` failed, then a line that starts with a space (ignored) ran.
            recent: vec![("make", 2), (" echo private", 1)],
            ..Setup::default()
        },
    );
    assert_eq!(ran.status(), 2);
    assert!(ran.err.contains("[history] ignore"), "{}", ran.err);
    assert_eq!(ran.connects, 0);
}

#[test]
fn alt_question_mark_leaves_the_status_alone() {
    let ran = go(
        says(&["x\n"]),
        Flow::Last { typed: false },
        Setup {
            recent: vec![("false", 1)],
            status_before: 1,
            ..Setup::default()
        },
    );
    assert_eq!(ran.status(), 1, "the failure stays visible in the prompt");
    assert_eq!(ran.shell.last_status().0, 1);
    // Also when nothing was explained.
    let ran = go(
        says(&["x"]),
        Flow::Last { typed: false },
        Setup {
            recent: vec![("true", 0)],
            status_before: 0,
            ..Setup::default()
        },
    );
    assert_eq!(ran.shell.last_status().0, 0);
    // And when the AI is not set up.
    let ran = go(
        says(&["x"]),
        Flow::Last { typed: false },
        Setup {
            recent: vec![("false", 1)],
            status_before: 1,
            unavailable: true,
            ..Setup::default()
        },
    );
    assert_eq!(ran.shell.last_status().0, 1);
    assert!(ran.err.contains("AI is not set up"), "{}", ran.err);
}

#[test]
fn the_target_table() {
    use super::Last;
    let ran = |command: &str, status| {
        target(Some(&Last::Ran {
            command: command.into(),
            status,
        }))
    };
    assert_eq!(
        ran("x", 1),
        Target::Explain {
            command: "x".into(),
            status: 1
        }
    );
    assert!(matches!(ran("x", 0), Target::Nothing(_)));
    assert!(matches!(ran("x", 130), Target::Nothing(_)));
    assert!(matches!(ran("x", 148), Target::Nothing(_)));
    assert!(matches!(target(None), Target::Nothing(_)));
    assert!(matches!(target(Some(&Last::Ignored)), Target::Nothing(_)));
    // 137 (killed) and 143 (terminated) are failures worth explaining.
    assert!(matches!(ran("x", 137), Target::Explain { .. }));
}

// -- cmd | ? question ------------------------------------------------------------------------

#[test]
fn the_output_of_the_command_goes_to_the_model_with_the_question() {
    let ran = go(
        says(&["Two lines.\n"]),
        Flow::Pipe {
            command: "echo one; echo two",
            question: "how many lines",
        },
        Setup::default(),
    );
    assert_eq!(ran.status(), 0);
    assert_eq!(
        ran.out, "Two lines.\n",
        "the command's own output is not shown"
    );
    let sent = ran.sent();
    assert!(sent.contains("Command: echo one; echo two\n"), "{sent}");
    assert!(sent.contains("Question: how many lines\n"), "{sent}");
    assert!(sent.contains("\none\ntwo\n"), "{sent}");
    assert!(sent.contains("Exit status: 0"), "{sent}");
    assert!(
        ran.err.contains("sending 8 characters of output"),
        "{}",
        ran.err
    );
    assert!(ran.system().contains("DATA"));
    assert_eq!(
        ran.recent.last(),
        Some(&Last::Ran {
            command: "echo one; echo two".into(),
            status: 0
        })
    );
    assert_eq!((ran.connects, ran.alive), (1, 0));
}

#[test]
fn only_standard_output_is_sent() {
    let ran = go(
        says(&["ok\n"]),
        Flow::Pipe {
            command: "echo out-$((1+1)); echo err-$((1+1)) >&2",
            question: "q",
        },
        Setup::default(),
    );
    assert!(ran.sent().contains("out-2"));
    assert!(!ran.sent().contains("err-2"));
    // With 2>&1 written by the person, both go.
    let ran = go(
        says(&["ok\n"]),
        Flow::Pipe {
            command: "{ echo out-$((1+1)); echo err-$((1+1)) >&2; } 2>&1",
            question: "q",
        },
        Setup::default(),
    );
    assert!(ran.sent().contains("out-2") && ran.sent().contains("err-2"));
}

#[test]
fn a_secret_in_the_output_is_masked_and_said_so() {
    let key = ghp();
    // The key is built by the command: only the output holds it, not the text of the command.
    let ran = go(
        says(&["ok\n"]),
        Flow::Pipe {
            command: "k=a1B2a1B2a1B2a1B2a1B2a1B2a1B2a1B2a1B2; echo token=ghp_$k",
            question: "what is this",
        },
        Setup::default(),
    );
    assert!(!ran.sent().contains(&key));
    assert!(ran.sent().contains("[MASKED]"));
    assert!(ran.err.contains("a secret was hidden"), "{}", ran.err);
    // Nothing is said when there was nothing to hide.
    let ran = go(
        says(&["ok\n"]),
        Flow::Pipe {
            command: "echo token=nothing-secret",
            question: "q",
        },
        Setup::default(),
    );
    assert!(!ran.err.contains("a secret"), "{}", ran.err);
}

#[test]
fn a_failing_command_is_still_asked_about_and_the_status_is_kept() {
    let ran = go(
        says(&["ok\n"]),
        Flow::Pipe {
            command: "echo partial; false",
            question: "why",
        },
        Setup::default(),
    );
    assert_eq!(ran.status(), 1, "the command's status, as a pipeline would");
    assert!(ran.sent().contains("Exit status: 1"));
    assert!(ran.sent().contains("partial"));
}

#[test]
fn if_the_question_cannot_be_answered_the_status_says_so() {
    let ran = go(
        FakeProvider::new(Kind::Ollama).fails(ProviderError::new(ErrorKind::Auth, "401")),
        Flow::Pipe {
            command: "echo hi",
            question: "q",
        },
        Setup::default(),
    );
    assert_eq!(ran.status(), 1);
    assert!(ran.err.contains("oh-my-shell: ?:"), "{}", ran.err);
}

#[test]
fn an_empty_question_asks_for_an_explanation_of_the_output() {
    let ran = go(
        says(&["ok\n"]),
        Flow::Pipe {
            command: "echo hi",
            question: "",
        },
        Setup::default(),
    );
    assert!(ran.sent().contains("Explain what this output shows."));
}

#[test]
fn without_ai_the_command_does_not_run() {
    let dir = Dir::new();
    let command = format!("echo x > {}", dir.file("m"));
    let ran = go(
        says(&["x"]),
        Flow::Pipe {
            command: &command,
            question: "q",
        },
        Setup {
            unavailable: true,
            ..Setup::default()
        },
    );
    assert!(!dir.has("m"));
    assert_eq!(ran.status(), 1);
    assert!(ran.err.contains("AI is not set up"), "{}", ran.err);
}

#[test]
fn a_command_cancelled_at_the_question_sends_nothing_and_deletes_nothing() {
    let dir = Dir::new();
    let victim = dir.file("victim");
    fs::create_dir_all(&victim).unwrap();
    fs::write(format!("{victim}/f"), "data").unwrap();
    for answer in [
        Ok(String::new()),
        Ok("n".to_owned()),
        Ok("no thanks".to_owned()),
        Err(ReadError::EndOfInput),
    ] {
        let ran = go(
            says(&["x"]),
            Flow::Pipe {
                command: &format!("rm -rf {victim}"),
                question: "q",
            },
            Setup {
                answers: vec![answer],
                ..Setup::default()
            },
        );
        assert!(Path::new(&victim).exists(), "the cancelled rm must not run");
        assert_eq!(ran.connects, 0, "nothing was sent");
        assert!(ran.status() == 1 || ran.status() == 130);
        assert_eq!(
            ran.recent.last(),
            None,
            "a command that did not run is not recent"
        );
    }
}

#[test]
fn a_dangerous_command_that_is_confirmed_runs_and_is_asked_about() {
    let dir = Dir::new();
    let victim = dir.file("victim");
    fs::create_dir_all(&victim).unwrap();
    let ran = go(
        says(&["ok\n"]),
        Flow::Pipe {
            command: &format!("rm -rf {victim}"),
            question: "q",
        },
        Setup {
            answers: vec![Ok("y".to_owned())],
            ..Setup::default()
        },
    );
    assert!(!Path::new(&victim).exists());
    assert_eq!(ran.connects, 1);
    // It ran through the normal line path, so the model was asked about its (empty) output.
    assert!(ran.sent().contains("wrote nothing"), "{}", ran.sent());
}

#[test]
fn a_command_that_does_not_parse_is_reported_by_the_shell_and_not_asked_about() {
    let ran = go(
        says(&["x"]),
        Flow::Pipe {
            command: "echo 'unterminated",
            question: "q",
        },
        Setup::default(),
    );
    assert!(matches!(
        ran.result,
        LineResult::Rejected(_) | LineResult::Incomplete
    ));
    assert_eq!(ran.connects, 0);
}

#[test]
fn exit_in_the_command_ends_the_shell_and_sends_nothing() {
    let ran = go(
        says(&["x"]),
        Flow::Pipe {
            command: "echo bye; exit 3",
            question: "q",
        },
        Setup::default(),
    );
    assert!(matches!(
        ran.result,
        LineResult::Ran(Outcome::Exit(ExitStatus(3)))
    ));
    assert_eq!(ran.connects, 0);
}

#[test]
fn an_interrupted_command_sends_nothing() {
    let ran = go(
        says(&["x"]),
        Flow::Pipe {
            command: "echo partial; sh -c 'exit 130'",
            question: "q",
        },
        Setup::default(),
    );
    assert_eq!(ran.status(), 130);
    assert_eq!(ran.connects, 0);
    assert!(ran.err.contains("nothing was sent"), "{}", ran.err);
}

#[test]
fn binary_output_is_not_sent() {
    let ran = go(
        says(&["x\n"]),
        Flow::Pipe {
            command: "head -c 5 /dev/zero",
            question: "q",
        },
        Setup::default(),
    );
    assert!(
        ran.sent().contains("5 bytes of binary data"),
        "{}",
        ran.sent()
    );
    assert!(ran.err.contains("binary"), "{}", ran.err);
}

#[test]
fn a_long_output_is_cut_to_its_start_and_its_end() {
    let ran = go(
        says(&["ok\n"]),
        Flow::Pipe {
            command: "i=0; while [ $i -lt 20000 ]; do echo \"line $i\"; i=$((i+1)); done",
            question: "q",
        },
        Setup::default(),
    );
    let sent = ran.sent();
    assert!(sent.contains("\nline 0\n"), "the start");
    assert!(sent.contains("\nline 19999\n"), "the end");
    assert!(sent.contains(LEFT_OUT));
    assert!(!sent.contains("\nline 10000\n"), "the middle is left out");
    assert!(sent.len() < 9_000, "{}", sent.len());
    assert!(ran.err.contains("the start and the end of"), "{}", ran.err);
}

#[test]
fn a_background_process_holding_the_pipe_cannot_hang_the_shell() {
    let started = Instant::now();
    let ran = go(
        says(&["ok\n"]),
        Flow::Pipe {
            command: "echo early; sh -c 'sleep 4 &'",
            question: "q",
        },
        Setup::default(),
    );
    assert!(
        started.elapsed() < Duration::from_millis(3500),
        "{:?}",
        started.elapsed()
    );
    assert!(ran.sent().contains("early"));
    assert!(ran.err.contains("was not read"), "{}", ran.err);
}

#[test]
fn drain_keeps_the_start_and_the_end_in_order() {
    let mut captured = Captured::default();
    let data: Vec<u8> = (0..(KEEP_BYTES * 3)).map(|n| (n % 251) as u8).collect();
    for chunk in data.chunks(7000) {
        captured.add(chunk);
    }
    assert_eq!(captured.total, data.len() as u64);
    assert_eq!(captured.head, data[..KEEP_BYTES]);
    assert_eq!(captured.tail, data[data.len() - KEEP_BYTES..]);
    // Small output stays in the head.
    let mut small = Captured::default();
    small.add(b"abc");
    assert_eq!((small.head.as_slice(), small.tail.len()), (&b"abc"[..], 0));
}

#[test]
fn one_line_cuts_and_cleans() {
    assert_eq!(one_line("ls -l"), "ls -l");
    assert_eq!(one_line("a\nb"), "a...");
    assert!(one_line(&"x".repeat(100)).ends_with("..."));
    assert_eq!(one_line("a\x1b[0mb"), "a[0mb");
}
