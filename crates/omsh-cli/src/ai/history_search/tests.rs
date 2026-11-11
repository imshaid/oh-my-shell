use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use chrono::TimeZone;
use omsh_ai::fake::FakeProvider;
use omsh_ai::{ErrorKind, Provider, ProviderError, Settings};
use omsh_config::{AiLanguage, Provider as Kind};
use omsh_exec::{ReadError, Terminal};
use omsh_history::{Filter, Store};

use super::*;
use crate::ai::plan::Connect;

const PATIENT: Duration = Duration::from_secs(10);

// -- test doubles ---------------------------------------------------------------------------

#[derive(Default)]
struct State {
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
    fn read_line(&mut self, _p: &str, _h: bool, _line: &mut Vec<u8>) -> Result<(), ReadError> {
        Err(ReadError::EndOfInput)
    }
    fn interrupted(&self) -> bool {
        false
    }
}

struct Ran {
    result: LineResult,
    out: String,
    err: String,
    requests: Vec<omsh_ai::ChatRequest>,
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
    fn sent_text(&self) -> String {
        let request = self.requests.last().expect("no request was sent");
        let mut all = request.system.clone().unwrap_or_default();
        for message in &request.messages {
            all.push('\n');
            all.push_str(&message.text);
        }
        all
    }
}

/// The folder every test works in. The shell's `PWD` is set to it, so no test depends on the
/// process directory (other tests change it).
const HERE: &str = "/work/proj";

fn history() -> History {
    History {
        store: Store::in_memory(),
        filter: Filter::new(&[]),
    }
}

fn record(history: &History, command: &str, cwd: &str, status: i32) {
    let token = history.store.start(command, cwd);
    history
        .store
        .finish(token, status, Duration::from_millis(3));
}

fn fixed_now() -> DateTime<Local> {
    Local.with_ymd_and_hms(2026, 11, 11, 15, 30, 0).unwrap()
}

fn setup_with(fake: &Arc<FakeProvider>, connects: &Arc<AtomicU32>) -> PlanSetup {
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
        language: AiLanguage::Auto,
        context: omsh_config::AiContext::default(),
    }
}

fn go(fake: FakeProvider, request: &str, history: Option<&History>) -> Ran {
    go_with(fake, request, history, false)
}

fn go_with(fake: FakeProvider, request: &str, history: Option<&History>, no_ai: bool) -> Ran {
    let fake = Arc::new(fake);
    let connects = Arc::new(AtomicU32::new(0));
    let mut shell = Shell::new();
    let mut files = Io::process();
    shell.run_source(&format!("PWD={HERE}"), true, &mut files);
    assert_eq!(directory(&shell), HERE);
    let mut tty = Script::default();
    let setup = if no_ai {
        Err("AI is not set up; run `omsh ai setup`".to_owned())
    } else {
        Ok(setup_with(&fake, &connects))
    };
    let result = run(
        setup,
        request,
        fixed_now(),
        SearchEnv {
            shell: &mut shell,
            files: &mut files,
            tty: &mut tty,
            history,
        },
    );
    let state = tty.0.into_inner();
    Ran {
        result,
        out: state.out,
        err: state.err,
        requests: fake.requests(),
        connects: connects.load(Ordering::Relaxed),
        alive: Arc::strong_count(&fake) - 1,
    }
}

fn says(answer: &str) -> FakeProvider {
    FakeProvider::new(Kind::Ollama).says(&[answer])
}

// -- which lines are searches -----------------------------------------------------------------

#[test]
fn only_the_word_history_makes_a_search() {
    assert_eq!(request_of("history"), Some(""));
    assert_eq!(request_of("history "), Some(""));
    assert_eq!(
        request_of("history docker yesterday"),
        Some("docker yesterday")
    );
    assert_eq!(request_of("history\tdocker"), Some("docker"));
    assert_eq!(request_of("  history   docker  "), Some("docker"));
    for other in [
        "historyx",
        "histories",
        "show history",
        "History docker",
        "",
        "his",
        "history-docker",
    ] {
        assert_eq!(request_of(other), None, "{other:?}");
    }
}

// -- the query ---------------------------------------------------------------------------------

fn filter(json: &str) -> HistoryFilter {
    HistoryFilter::parse(json).unwrap()
}

#[test]
fn a_filter_becomes_a_query() {
    let query = query_for(
        &filter(
            r#"{"all": ["Docker"], "any": ["up", "down"], "here": true, "status": "failed",
                "newest": 7}"#,
        ),
        HERE,
    )
    .unwrap();
    assert_eq!(query.cwd.as_deref(), Some(HERE));
    assert_eq!(query.words_all, ["Docker"]);
    assert_eq!(query.words_any, ["up", "down"]);
    assert!(query.failed && !query.succeeded);
    assert_eq!(
        query.limit,
        Some(8),
        "one more than shown, for the search itself"
    );
    assert!(query.newest_first);
    assert_eq!((query.since_ms, query.until_ms), (None, None));

    let query = query_for(&filter(r#"{"status": "succeeded"}"#), HERE).unwrap();
    assert!(query.succeeded && !query.failed);
    assert_eq!(query.cwd, None, "`here` is off unless asked for");
    assert_eq!(query.limit, Some(21));
}

#[test]
fn days_run_from_local_midnight_to_the_next_local_midnight() {
    let query = query_for(
        &filter(r#"{"from": "2026-11-10", "to": "2026-11-10"}"#),
        HERE,
    )
    .unwrap();
    let midnight = |day: u32| {
        Local
            .with_ymd_and_hms(2026, 11, day, 0, 0, 0)
            .earliest()
            .unwrap()
            .timestamp_millis()
    };
    assert_eq!(query.since_ms, Some(midnight(10)));
    // `to` is a day that is included: the search ends where the next day begins.
    assert_eq!(query.until_ms, Some(midnight(11)));
    // A month end and a year end roll over.
    let query = query_for(&filter(r#"{"to": "2026-12-31"}"#), HERE).unwrap();
    let new_year = Local
        .with_ymd_and_hms(2027, 1, 1, 0, 0, 0)
        .earliest()
        .unwrap()
        .timestamp_millis();
    assert_eq!(query.until_ms, Some(new_year));
}

// -- the flow ----------------------------------------------------------------------------------

#[test]
fn the_model_writes_a_filter_and_the_local_history_answers() {
    let history = history();
    record(&history, "docker compose up", HERE, 0);
    record(&history, "docker build .", HERE, 1);
    record(&history, "ls", HERE, 0);
    record(&history, "docker run broken", "/elsewhere", 125);
    let ran = go(
        says(r#"{"all": ["docker"], "status": "failed"}"#),
        "docker commands that failed",
        Some(&history),
    );
    assert_eq!(ran.status(), 0);
    assert!(ran.out.contains("docker build ."), "{}", ran.out);
    assert!(ran.out.contains("docker run broken"), "{}", ran.out);
    assert!(!ran.out.contains("compose up"), "{}", ran.out);
    assert!(!ran.out.contains("ls"), "{}", ran.out);
    // The shell's own words for the filter, then the real count.
    assert!(
        ran.err
            .contains("? history: commands containing \"docker\", that failed, newest 20"),
        "{}",
        ran.err
    );
    assert!(ran.err.contains("2 commands shown"), "{}", ran.err);
    assert_eq!((ran.connects, ran.alive), (1, 0), "no provider outlives it");
    assert!(ran.requests[0].json);
}

#[test]
fn no_command_from_the_history_is_sent() {
    let history = history();
    record(
        &history,
        "curl https://private.example/PLANTED-MARKER",
        HERE,
        0,
    );
    record(&history, "docker ps", HERE, 0);
    let ran = go(
        says(r#"{"all": ["docker"]}"#),
        "what did I do with docker",
        Some(&history),
    );
    let sent = ran.sent_text();
    assert!(!sent.contains("PLANTED-MARKER"), "{sent}");
    assert!(!sent.contains("docker ps"), "{sent}");
    assert!(!sent.contains(HERE), "the folder name is not sent: {sent}");
    assert_eq!(ran.requests.len(), 1);
    assert_eq!(ran.requests[0].messages.len(), 1);
    assert_eq!(
        ran.requests[0].messages[0].text,
        "what did I do with docker"
    );
    assert!(sent.contains("2026-11-11 (Wednesday)"), "{sent}");
    assert!(ran.err.contains("no command from your history is sent"));
}

#[test]
fn here_is_the_folder_itself_and_not_the_folders_below_it() {
    let history = history();
    record(&history, "make in-here", HERE, 0);
    record(&history, "make in-sub", &format!("{HERE}/sub"), 0);
    record(&history, "make in-other", "/work/proj2", 0);
    let ran = go(
        says(r#"{"all": ["make"], "here": true}"#),
        "make here",
        Some(&history),
    );
    assert!(ran.out.contains("make in-here"), "{}", ran.out);
    assert!(!ran.out.contains("in-sub"), "{}", ran.out);
    assert!(!ran.out.contains("in-other"), "{}", ran.out);
    assert!(
        ran.err.contains("run in this folder (not its subfolders)"),
        "{}",
        ran.err
    );
}

#[test]
fn the_newest_come_last_in_the_listing_and_the_count_is_respected() {
    let history = history();
    for n in 1..=5 {
        record(&history, &format!("echo n{n}"), HERE, 0);
    }
    let ran = go(
        says(r#"{"all": ["echo"], "newest": 3}"#),
        "echo",
        Some(&history),
    );
    let order: Vec<usize> = ["n3", "n4", "n5"]
        .iter()
        .map(|n| {
            ran.out
                .find(n)
                .unwrap_or_else(|| panic!("{n} in {}", ran.out))
        })
        .collect();
    assert!(
        order.windows(2).all(|pair| pair[0] < pair[1]),
        "{}",
        ran.out
    );
    assert!(!ran.out.contains("n2"), "{}", ran.out);
    assert!(ran.err.contains("3 commands shown"), "{}", ran.err);
}

#[test]
fn the_search_that_is_running_does_not_find_itself() {
    let history = history();
    record(&history, "docker one", HERE, 0);
    record(&history, "docker two", HERE, 0);
    // An older search that ended is history like any other line.
    record(&history, "? history docker earlier", HERE, 0);
    // This one has started and not ended: it is in the database and has the word.
    let _ = history.store.start("? history docker", HERE);
    assert!(history.store.settle(PATIENT));
    let ran = go(
        says(r#"{"all": ["docker"], "newest": 3}"#),
        "docker",
        Some(&history),
    );
    assert_eq!(ran.status(), 0);
    assert!(ran.out.contains("docker one"), "{}", ran.out);
    assert!(ran.out.contains("docker two"), "{}", ran.out);
    assert!(ran.out.contains("? history docker earlier"), "{}", ran.out);
    assert!(
        !ran.out.lines().any(|l| l.ends_with("? history docker")),
        "{}",
        ran.out
    );
    assert!(ran.err.contains("3 commands shown"), "{}", ran.err);
}

#[test]
fn an_unfinished_command_that_is_not_a_search_is_still_listed() {
    let history = history();
    let _ = history.store.start("vim docker-notes", HERE);
    assert!(history.store.settle(PATIENT));
    let ran = go(says(r#"{"all": ["docker"]}"#), "docker", Some(&history));
    assert!(ran.out.contains("vim docker-notes"), "{}", ran.out);
}

#[test]
fn nothing_found_says_so_and_fails() {
    let history = history();
    record(&history, "ls", HERE, 0);
    let ran = go(says(r#"{"all": ["kubectl"]}"#), "kubectl", Some(&history));
    assert_eq!(ran.status(), 1);
    assert_eq!(ran.out, "");
    assert!(ran.err.contains("no command matches"), "{}", ran.err);
}

#[test]
fn a_bad_first_answer_gets_one_retry_with_the_reason() {
    let history = history();
    record(&history, "docker ps", HERE, 0);
    let fake = FakeProvider::new(Kind::Ollama)
        .says(&[r#"{"newest": 0}"#])
        .says(&[r#"{"all": ["docker"]}"#]);
    let ran = go(fake, "docker", Some(&history));
    assert_eq!(ran.status(), 0);
    assert_eq!(ran.requests.len(), 2);
    let again = &ran.requests[1];
    assert_eq!(again.messages.len(), 3);
    assert!(
        again.messages[2].text.contains("newest must be from 1 to"),
        "{:?}",
        again.messages[2]
    );
    assert!(ran.out.contains("docker ps"));
}

#[test]
fn two_bad_answers_end_with_one_clear_line_and_no_search() {
    let history = history();
    record(&history, "docker ps", HERE, 0);
    let fake = FakeProvider::new(Kind::Ollama)
        .says(&["I cannot help"])
        .says(&[r#"{"sql": "select * from history"}"#]);
    let ran = go(fake, "docker", Some(&history));
    assert_eq!(ran.status(), 1);
    assert_eq!(ran.requests.len(), 2);
    assert_eq!(ran.out, "");
    assert!(ran.err.contains("couldn't read the search"), "{}", ran.err);
    assert!(ran.err.contains("unknown field"), "{}", ran.err);
    assert!(ran.err.contains("history search TEXT"), "{}", ran.err);
}

#[test]
fn a_provider_error_is_one_line_and_nothing_is_searched() {
    let history = history();
    record(&history, "docker ps", HERE, 0);
    let fake = FakeProvider::new(Kind::Ollama)
        .fails(ProviderError::new(ErrorKind::Offline, "connection refused"));
    let ran = go(fake, "docker", Some(&history));
    assert_eq!(ran.status(), 1);
    assert_eq!(ran.out, "");
    assert!(ran.err.contains("Ollama"), "{}", ran.err);
    assert_eq!((ran.connects, ran.alive), (1, 0));
}

#[test]
fn an_empty_request_is_usage_and_sends_nothing() {
    let history = history();
    let ran = go(says("{}"), "  ", Some(&history));
    assert_eq!(ran.status(), 2);
    assert!(ran.err.contains("usage: ? history"), "{}", ran.err);
    assert_eq!((ran.connects, ran.requests.len()), (0, 0));
}

#[test]
fn without_the_ai_nothing_is_sent_and_the_plain_search_is_pointed_to() {
    let history = history();
    let ran = go_with(says("{}"), "docker", Some(&history), true);
    assert_eq!(ran.status(), 1);
    assert!(ran.err.contains("omsh ai setup"), "{}", ran.err);
    assert!(ran.err.contains("history search TEXT"), "{}", ran.err);
    assert_eq!((ran.connects, ran.requests.len()), (0, 0));
}

#[test]
fn without_a_history_nothing_is_sent() {
    let ran = go(says("{}"), "docker", None);
    assert_eq!(ran.status(), 1);
    assert!(ran.err.contains("no history"), "{}", ran.err);
    assert_eq!((ran.connects, ran.requests.len()), (0, 0));
}

#[test]
fn a_secret_in_the_request_is_masked_and_said_so() {
    let history = history();
    record(&history, "ls", HERE, 0);
    let key = format!("ghp_{}", "a1B2".repeat(9));
    let ran = go(
        says(r#"{}"#),
        &format!("where did I use {key}"),
        Some(&history),
    );
    assert!(!ran.sent_text().contains(&key));
    assert!(ran.sent_text().contains("[MASKED]"));
    assert!(
        ran.err.contains("a secret in the request was hidden"),
        "{}",
        ran.err
    );
    let clean = go(says(r#"{}"#), "git push", Some(&history));
    assert!(!clean.err.contains("a secret"), "{}", clean.err);
}

#[test]
fn text_from_the_model_never_reaches_the_terminal_except_through_the_checks() {
    let history = history();
    record(&history, "ls", HERE, 0);
    // The reorder mark is refused by the checks, so it can never be printed back.
    let fake = FakeProvider::new(Kind::Ollama)
        .says(&["{\"all\": [\"x\u{202e}y\"]}"])
        .says(&["{\"all\": [\"a\\u001b[31mb\"]}"]);
    let ran = go(fake, "x", Some(&history));
    assert_eq!(ran.status(), 1);
    assert!(!ran.err.contains('\u{202e}'), "{:?}", ran.err);
    assert!(!ran.err.contains('\u{1b}'), "{:?}", ran.err);
    assert!(ran.err.contains("control character"), "{:?}", ran.err);
}

#[test]
fn the_flow_sets_the_exit_status() {
    let history = history();
    record(&history, "docker ps", HERE, 0);
    let mut shell = Shell::new();
    let mut files = Io::process();
    let mut tty = Script::default();
    let fake = Arc::new(says(r#"{"all": ["docker"]}"#));
    let connects = Arc::new(AtomicU32::new(0));
    shell.set_last_status(ExitStatus(5));
    let _ = run(
        Ok(setup_with(&fake, &connects)),
        "docker",
        fixed_now(),
        SearchEnv {
            shell: &mut shell,
            files: &mut files,
            tty: &mut tty,
            history: Some(&history),
        },
    );
    assert_eq!(shell.last_status(), ExitStatus(0));
}
