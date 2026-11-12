//! The commands of `omsh ai`, driven with a scripted terminal and fake providers.
//!
//! The real keyboard, the real file store and a real HTTP server are in
//! `omsh-tests/tests/ai_setup.rs`; here every branch of the wizard runs in milliseconds.

use std::collections::VecDeque;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use omsh_ai::fake::{FakeProvider, MemoryStore};
use omsh_ai::{
    CredentialsFile, ErrorKind, KeyError, KeyStore, Keys, Provider, ProviderError, Secret,
};
use omsh_config::{Provider as Kind, TEMPLATE, parse};
use omsh_exec::{OmshCommand, ReadError, Terminal};

use super::{AiSession, Environment};

// -- a terminal that answers from a script ----------------------------------------------

/// What the person types in answer to a question.
const INTERRUPT: &str = "<Ctrl+C>";
const END: &str = "<Ctrl+D>";

struct Scripted {
    answers: VecDeque<&'static str>,
    is_terminal: bool,
    out: String,
    err: String,
    /// Every question asked, and whether the answer was to be hidden.
    prompts: Vec<(String, bool)>,
}

impl Scripted {
    fn new(answers: &[&'static str]) -> Self {
        Self {
            answers: answers.iter().copied().collect(),
            is_terminal: true,
            out: String::new(),
            err: String::new(),
            prompts: Vec::new(),
        }
    }

    fn everything(&self) -> String {
        format!("{}{}", self.out, self.err)
    }

    fn asked(&self, fragment: &str) -> bool {
        self.prompts.iter().any(|(p, _)| p.contains(fragment))
    }
}

impl Terminal for Scripted {
    fn input_is_terminal(&self) -> bool {
        self.is_terminal
    }

    fn out(&mut self, text: &str) {
        self.out.push_str(text);
    }

    fn err(&mut self, text: &str) {
        self.err.push_str(text);
    }

    fn read_line(
        &mut self,
        prompt: &str,
        hidden: bool,
        line: &mut Vec<u8>,
    ) -> Result<(), ReadError> {
        line.clear();
        self.prompts.push((prompt.to_owned(), hidden));
        self.err.push_str(prompt);
        match self.answers.pop_front() {
            Some(INTERRUPT) => Err(ReadError::Interrupted),
            Some(END) => Err(ReadError::EndOfInput),
            Some(answer) => {
                line.extend_from_slice(answer.as_bytes());
                Ok(())
            }
            None => panic!("the wizard asked a question nobody answered: {prompt:?}"),
        }
    }

    fn interrupted(&self) -> bool {
        false
    }
}

// -- the world around the commands ------------------------------------------------------

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "omsh-cli-ai-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// What the commands asked the provider factory for.
#[derive(Clone, Debug)]
struct Made {
    kind: Kind,
    model: String,
    base_url: Option<String>,
    key: Option<String>,
}

type Behavior = Box<dyn Fn(&Made) -> Result<FakeProvider, ProviderError> + Send + Sync>;

/// A provider that accepts everything and offers two models.
fn accepting() -> Behavior {
    Box::new(|made| {
        Ok(FakeProvider::new(made.kind)
            .says(&["OK"])
            .offering(&["alpha", "beta"]))
    })
}

struct World {
    dir: Scratch,
    ring: MemoryStore,
    made: Arc<Mutex<Vec<Made>>>,
    session: AiSession,
    config_file: PathBuf,
    /// Set when the keyring was connected to.
    connected: Arc<Mutex<bool>>,
}

struct Setup {
    config_text: &'static str,
    no_ai: bool,
    no_config: bool,
    keyring: bool,
    behavior: Behavior,
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            config_text: TEMPLATE,
            no_ai: false,
            no_config: false,
            keyring: true,
            behavior: accepting(),
        }
    }
}

impl Setup {
    fn build(self) -> World {
        let dir = Scratch::new();
        let config_file = dir.0.join("config.toml");
        fs::write(&config_file, self.config_text).unwrap();
        let ring = MemoryStore::named("the test keyring");
        let connected = Arc::new(Mutex::new(false));
        let keyring = self.keyring;
        let keys = Keys::with(
            Box::new({
                let ring = ring.clone();
                let connected = Arc::clone(&connected);
                move || {
                    *connected.lock().unwrap() = true;
                    if keyring {
                        Ok(Box::new(ring.clone()) as Box<dyn KeyStore>)
                    } else {
                        Err(KeyError::Unavailable("no session bus".to_owned()))
                    }
                }
            }),
            Box::new(CredentialsFile::new(dir.0.join("credentials"))),
        );
        let made = Arc::new(Mutex::new(Vec::new()));
        let behavior = self.behavior;
        let record = Arc::clone(&made);
        let env = Environment {
            keys,
            config_file: (!self.no_config).then(|| config_file.clone()),
            make_provider: Arc::new(move |settings, key| {
                let made = Made {
                    kind: settings.kind,
                    model: settings.model.clone(),
                    base_url: settings.base_url.clone(),
                    key: key.map(|k| k.expose().to_owned()),
                };
                record.lock().unwrap().push(made.clone());
                behavior(&made).map(|p| Box::new(p) as Box<dyn Provider>)
            }),
        };
        let config = parse(self.config_text).config;
        World {
            dir,
            ring,
            made,
            session: AiSession::new(&config, self.no_ai, env),
            config_file,
            connected,
        }
    }
}

impl World {
    fn run(&self, words: &[&str], terminal: &mut Scripted) -> u8 {
        let args: Vec<Vec<u8>> = words.iter().map(|w| w.as_bytes().to_vec()).collect();
        self.session.run(&args, terminal)
    }

    fn config_text(&self) -> String {
        fs::read_to_string(&self.config_file).unwrap()
    }

    fn made(&self) -> Vec<Made> {
        self.made.lock().unwrap().clone()
    }

    fn keyring_was_used(&self) -> bool {
        *self.connected.lock().unwrap()
    }

    fn stored(&self, kind: Kind) -> Option<String> {
        self.ring
            .get(kind)
            .unwrap()
            .map(|key| key.expose().to_owned())
    }

    fn credentials_file(&self) -> PathBuf {
        self.dir.0.join("credentials")
    }

    fn nothing_was_saved(&self, original_config: &str) {
        assert_eq!(self.config_text(), original_config, "config.toml changed");
        for kind in Kind::ALL {
            assert_eq!(self.stored(kind), None, "a key was stored for {kind:?}");
        }
        assert!(
            !self.credentials_file().exists(),
            "a credentials file appeared"
        );
    }
}

fn store_key(world: &World, kind: Kind, key: &str) {
    world
        .ring
        .set(kind, &Secret::new(key.to_owned()).unwrap())
        .unwrap();
}

const KEY: &str = "sk-test-9d41c7e2aa";

/// The answers of a successful setup of an OpenAI-compatible server.
fn compatible_server() -> [&'static str; 4] {
    ["5", "http://localhost:8080/v1", KEY, "1"]
}

// -- setup: the happy paths -------------------------------------------------------------

#[test]
fn setup_of_an_openai_compatible_server_stores_the_key_and_the_settings() {
    let world = Setup::default().build();
    let mut t = Scripted::new(&compatible_server());
    assert_eq!(world.run(&["ai", "setup"], &mut t), 0, "{}", t.everything());

    assert_eq!(world.stored(Kind::OpenAiCompatible).as_deref(), Some(KEY));
    let text = world.config_text();
    assert!(text.starts_with(TEMPLATE), "the comments stay");
    let config = parse(&text).config;
    assert_eq!(config.ai.provider, Some(Kind::OpenAiCompatible));
    assert_eq!(config.ai.model.as_deref(), Some("alpha"));
    assert_eq!(
        config.ai.base_url.as_deref(),
        Some("http://localhost:8080/v1")
    );
    // The key is in the keyring and nowhere else.
    assert!(!text.contains(KEY));
    assert!(!t.everything().contains(KEY));
    assert!(!world.credentials_file().exists());
    assert!(
        t.err.contains("The key is stored in the test keyring"),
        "{}",
        t.err
    );
}

#[test]
fn the_key_is_asked_hidden_and_nothing_else_is() {
    let world = Setup::default().build();
    let mut t = Scripted::new(&compatible_server());
    assert_eq!(world.run(&["ai", "setup"], &mut t), 0);
    for (prompt, hidden) in &t.prompts {
        assert_eq!(*hidden, prompt.contains("API key"), "{prompt:?}");
    }
    assert!(t.asked("API key"));
}

#[test]
fn the_key_that_was_typed_is_the_key_that_is_checked() {
    let world = Setup::default().build();
    let mut t = Scripted::new(&compatible_server());
    world.run(&["ai", "setup"], &mut t);
    let made = world.made();
    // One request for the list of models, one for the check.
    assert_eq!(made.len(), 2, "{made:?}");
    assert_eq!(made[0].model, "");
    assert_eq!(made[1].model, "alpha");
    assert!(
        made.iter().all(|m| m.key.as_deref() == Some(KEY)),
        "{made:?}"
    );
    assert!(
        made.iter()
            .all(|m| m.base_url.as_deref() == Some("http://localhost:8080/v1"))
    );
}

#[test]
fn the_new_key_works_at_once_in_the_same_session() {
    let world = Setup::default().build();
    world.run(&["ai", "setup"], &mut Scripted::new(&compatible_server()));
    let before = world.made().len();
    let mut t = Scripted::new(&[]);
    assert_eq!(world.run(&["ai", "test"], &mut t), 0, "{}", t.everything());
    let made = world.made();
    assert_eq!(made.len(), before + 1);
    let last = made.last().unwrap();
    assert_eq!(
        (last.kind, last.model.as_str()),
        (Kind::OpenAiCompatible, "alpha")
    );
    assert_eq!(last.key.as_deref(), Some(KEY), "read back from the store");
    assert!(
        t.out
            .starts_with("ok: OpenAI-compatible server answered in "),
        "{}",
        t.out
    );
    assert!(t.out.contains("(it said: \"OK\")"), "{}", t.out);
}

#[test]
fn anthropic_has_no_address_question_and_a_model_can_be_picked_by_number() {
    let world = Setup::default().build();
    let mut t = Scripted::new(&["2", KEY, "2"]);
    assert_eq!(world.run(&["ai", "setup"], &mut t), 0, "{}", t.everything());
    let config = parse(&world.config_text()).config;
    assert_eq!(config.ai.provider, Some(Kind::Anthropic));
    assert_eq!(config.ai.model.as_deref(), Some("beta"));
    assert_eq!(config.ai.base_url, None);
    assert!(!t.asked("address"), "{:?}", t.prompts);
    assert_eq!(world.stored(Kind::Anthropic).as_deref(), Some(KEY));
}

#[test]
fn a_provider_can_be_named_instead_of_numbered() {
    let world = Setup::default().build();
    let mut t = Scripted::new(&["Gemini", KEY, "alpha"]);
    assert_eq!(world.run(&["ai", "setup"], &mut t), 0, "{}", t.everything());
    assert_eq!(
        parse(&world.config_text()).config.ai.provider,
        Some(Kind::Gemini)
    );
}

#[test]
fn ollama_needs_no_key_and_its_default_address_is_not_written() {
    let world = Setup::default().build();
    let mut t = Scripted::new(&["1", "", "1"]);
    assert_eq!(world.run(&["ai", "setup"], &mut t), 0, "{}", t.everything());
    let config = parse(&world.config_text()).config;
    assert_eq!(config.ai.provider, Some(Kind::Ollama));
    assert_eq!(config.ai.base_url, None);
    assert!(!t.asked("API key"));
    assert!(world.made().iter().all(|m| m.key.is_none()));
    assert!(
        !world.keyring_was_used(),
        "no key, so the keyring is not touched"
    );
}

#[test]
fn another_ollama_address_is_written() {
    let world = Setup::default().build();
    let mut t = Scripted::new(&["1", "http://gpu-box:11434", "alpha"]);
    assert_eq!(world.run(&["ai", "setup"], &mut t), 0, "{}", t.everything());
    assert_eq!(
        parse(&world.config_text()).config.ai.base_url.as_deref(),
        Some("http://gpu-box:11434")
    );
}

#[test]
fn an_existing_key_is_kept_with_enter() {
    let world = Setup::default().build();
    store_key(&world, Kind::Anthropic, "sk-stored-earlier");
    let mut t = Scripted::new(&["2", "", "1"]);
    assert_eq!(world.run(&["ai", "setup"], &mut t), 0, "{}", t.everything());
    assert!(
        t.err
            .contains("Using the key already stored in the test keyring")
    );
    assert!(
        world
            .made()
            .iter()
            .all(|m| m.key.as_deref() == Some("sk-stored-earlier"))
    );
    assert_eq!(
        world.stored(Kind::Anthropic).as_deref(),
        Some("sk-stored-earlier")
    );
}

#[test]
fn running_setup_again_offers_the_current_choice() {
    let config = "[ai]\nprovider = \"anthropic\"\nmodel = \"old-model\"\n";
    let world = Setup {
        config_text: config,
        ..Setup::default()
    }
    .build();
    store_key(&world, Kind::Anthropic, KEY);
    // Enter for the provider, Enter for the key, a new model by number.
    let mut t = Scripted::new(&["", "", "2"]);
    assert_eq!(world.run(&["ai", "setup"], &mut t), 0, "{}", t.everything());
    assert!(
        t.prompts[0].0.contains("[Enter keeps anthropic]"),
        "{:?}",
        t.prompts[0]
    );
    assert_eq!(
        parse(&world.config_text()).config.ai.model.as_deref(),
        Some("beta")
    );
}

#[test]
fn enter_keeps_the_current_model_when_setup_is_run_again() {
    let config = "[ai]\nprovider = \"anthropic\"\nmodel = \"old-model\"\n";
    let world = Setup {
        config_text: config,
        ..Setup::default()
    }
    .build();
    store_key(&world, Kind::Anthropic, KEY);
    let mut t = Scripted::new(&["", "", ""]);
    assert_eq!(world.run(&["ai", "setup"], &mut t), 0, "{}", t.everything());
    assert!(t.err.contains("[Enter keeps old-model]"), "{}", t.err);
    assert_eq!(
        parse(&world.config_text()).config.ai.model.as_deref(),
        Some("old-model")
    );
    assert_eq!(world.made().last().unwrap().model, "old-model");
}

#[test]
fn without_a_keyring_the_key_goes_to_a_file_only_the_user_can_read() {
    let world = Setup {
        keyring: false,
        ..Setup::default()
    }
    .build();
    let mut t = Scripted::new(&compatible_server());
    assert_eq!(world.run(&["ai", "setup"], &mut t), 0, "{}", t.everything());
    let file = world.credentials_file();
    assert_eq!(
        fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(fs::read_to_string(&file).unwrap().contains(KEY));
    assert!(
        t.err
            .contains("The system keyring could not be used: no session bus"),
        "{}",
        t.err
    );
    assert!(t.err.contains("mode 0600"));
    assert!(!t.everything().contains(KEY));
    // And `status` finds it there.
    let mut status = Scripted::new(&[]);
    world.run(&["ai", "status"], &mut status);
    assert!(
        status
            .out
            .contains(&format!("stored in {}", file.display())),
        "{}",
        status.out
    );
}

#[test]
fn a_disabled_ai_is_switched_on_by_setup() {
    let world = Setup {
        config_text: "[ai]\nenabled = false\n",
        ..Setup::default()
    }
    .build();
    let mut t = Scripted::new(&compatible_server());
    assert_eq!(world.run(&["ai", "setup"], &mut t), 0, "{}", t.everything());
    assert!(t.err.contains("enabled was false"));
    assert!(parse(&world.config_text()).config.ai.enabled);
    let mut status = Scripted::new(&[]);
    world.run(&["ai", "status"], &mut status);
    assert!(status.out.starts_with("AI: ready"), "{}", status.out);
}

// -- setup: things that go wrong --------------------------------------------------------

/// Rejects every request that carries `bad`, as a provider does with a wrong key.
fn rejecting_key(bad: &'static str) -> Behavior {
    Box::new(move |made| {
        if made.key.as_deref() == Some(bad) {
            Ok(FakeProvider::new(made.kind).fails(ProviderError::from_status(401, "Unauthorized")))
        } else {
            accepting()(made)
        }
    })
}

#[test]
fn a_wrong_key_is_asked_for_again_and_never_stored() {
    let world = Setup {
        behavior: rejecting_key("sk-wrong"),
        ..Setup::default()
    }
    .build();
    let mut t = Scripted::new(&["2", "sk-wrong", "1", KEY]);
    assert_eq!(world.run(&["ai", "setup"], &mut t), 0, "{}", t.everything());
    assert!(
        t.err.contains("The provider did not accept the key."),
        "{}",
        t.err
    );
    assert_eq!(world.stored(Kind::Anthropic).as_deref(), Some(KEY));
    assert!(!t.everything().contains("sk-wrong"));
}

#[test]
fn three_wrong_keys_end_the_setup_and_nothing_is_saved() {
    let world = Setup {
        behavior: rejecting_key("sk-wrong"),
        ..Setup::default()
    }
    .build();
    let mut t = Scripted::new(&["2", "sk-wrong", "1", "sk-wrong", "sk-wrong"]);
    assert_eq!(world.run(&["ai", "setup"], &mut t), 1);
    assert!(t.err.contains("giving up after 3 tries"), "{}", t.err);
    world.nothing_was_saved(TEMPLATE);
}

#[test]
fn a_model_the_provider_does_not_know_is_asked_for_again() {
    let world = Setup {
        behavior: Box::new(|made| {
            let provider = FakeProvider::new(made.kind);
            if made.model == "nope" {
                Ok(provider.fails(ProviderError::from_status(404, "Not Found")))
            } else {
                // No list, so the model is typed.
                Ok(provider.says(&["OK"]).offering(&[]))
            }
        }),
        ..Setup::default()
    }
    .build();
    let mut t = Scripted::new(&["3", KEY, "nope", "real-model"]);
    assert_eq!(world.run(&["ai", "setup"], &mut t), 0, "{}", t.everything());
    assert!(t.err.contains("does not know the model nope"), "{}", t.err);
    assert_eq!(
        parse(&world.config_text()).config.ai.model.as_deref(),
        Some("real-model")
    );
}

fn unreachable_server() -> Behavior {
    Box::new(|made| {
        Ok(FakeProvider::new(made.kind)
            .fails(ProviderError::new(ErrorKind::Offline, "connection refused")))
    })
}

#[test]
fn a_server_that_cannot_be_reached_may_be_saved_anyway() {
    let world = Setup {
        behavior: unreachable_server(),
        ..Setup::default()
    }
    .build();
    // The list call finds nothing (FakeProvider::models works, so type the name), then the
    // check fails, then "y".
    let mut t = Scripted::new(&["1", "", "llama", "y"]);
    let status = world.run(&["ai", "setup"], &mut t);
    assert_eq!(status, 0, "{}", t.everything());
    assert!(t.err.contains("cannot reach Ollama"), "{}", t.err);
    assert!(
        t.err.contains("saved without a successful check"),
        "{}",
        t.err
    );
    let config = parse(&world.config_text()).config;
    assert_eq!(config.ai.provider, Some(Kind::Ollama));
}

#[test]
fn a_server_that_cannot_be_reached_is_not_saved_unless_asked() {
    let world = Setup {
        behavior: unreachable_server(),
        ..Setup::default()
    }
    .build();
    let mut t = Scripted::new(&["1", "", "llama", ""]);
    assert_eq!(world.run(&["ai", "setup"], &mut t), 1);
    assert!(t.asked("Save the settings anyway? [y/N]"));
    world.nothing_was_saved(TEMPLATE);
}

#[test]
fn ctrl_c_at_any_question_leaves_everything_as_it_was() {
    let script = compatible_server();
    for stop_at in 0..script.len() {
        let world = Setup::default().build();
        let mut answers: Vec<&'static str> = script.to_vec();
        answers[stop_at] = INTERRUPT;
        let mut t = Scripted::new(&answers);
        assert_eq!(
            world.run(&["ai", "setup"], &mut t),
            130,
            "at question {stop_at}"
        );
        assert!(t.err.contains("cancelled; nothing was saved"), "{}", t.err);
        world.nothing_was_saved(TEMPLATE);
        assert!(!t.everything().contains(KEY));
        // And the session did not change: still not set up.
        let mut status = Scripted::new(&[]);
        world.run(&["ai", "status"], &mut status);
        assert!(status.out.starts_with("AI: not set up"), "{}", status.out);
    }
}

#[test]
fn the_end_of_the_input_at_any_question_leaves_everything_as_it_was() {
    let script = compatible_server();
    for stop_at in 0..script.len() {
        let world = Setup::default().build();
        let mut answers: Vec<&'static str> = script.to_vec();
        answers[stop_at] = END;
        let mut t = Scripted::new(&answers);
        assert_eq!(
            world.run(&["ai", "setup"], &mut t),
            1,
            "at question {stop_at}"
        );
        world.nothing_was_saved(TEMPLATE);
    }
}

#[test]
fn wrong_answers_are_met_with_a_message_and_three_tries() {
    let world = Setup::default().build();
    let mut t = Scripted::new(&["0", "9", "banana"]);
    assert_eq!(world.run(&["ai", "setup"], &mut t), 1);
    assert_eq!(t.err.matches("That is not one of the choices.").count(), 3);
    assert!(t.err.contains("no provider was chosen"));
    world.nothing_was_saved(TEMPLATE);
}

#[test]
fn an_address_with_a_password_in_it_is_refused() {
    let world = Setup::default().build();
    let mut t = Scripted::new(&[
        "5",
        "http://me:hunter2@localhost/v1",
        "localhost:8080",
        "ftp://x",
    ]);
    assert_eq!(world.run(&["ai", "setup"], &mut t), 1);
    assert!(
        t.err.contains("must not contain a user name or password"),
        "{}",
        t.err
    );
    assert!(!t.err.contains("hunter2"), "the password is not repeated");
    world.nothing_was_saved(TEMPLATE);
}

// -- the key's destination --------------------------------------------------------------

#[test]
fn a_key_is_not_asked_for_an_address_it_would_not_be_sent_to() {
    let world = Setup::default().build();
    // Provider 5, a plain-http address elsewhere, "y" to going on without a key, model 1.
    let mut t = Scripted::new(&["5", "http://example.com/v1", "y", "1"]);
    assert_eq!(world.run(&["ai", "setup"], &mut t), 0, "{}", t.everything());
    assert!(
        !t.asked("API key"),
        "the key must not even be asked: {:?}",
        t.prompts
    );
    assert!(
        t.err.contains("example.com is reached over plain http"),
        "{}",
        t.err
    );
    assert!(
        world.made().iter().all(|m| m.key.is_none()),
        "{:?}",
        world.made()
    );
    assert!(!world.keyring_was_used());
    let config = parse(&world.config_text()).config;
    assert_eq!(config.ai.base_url.as_deref(), Some("http://example.com/v1"));
}

#[test]
fn declining_to_go_on_without_a_key_saves_nothing() {
    let world = Setup::default().build();
    let mut t = Scripted::new(&["5", "http://example.com/v1", "n"]);
    assert_eq!(world.run(&["ai", "setup"], &mut t), 1);
    assert!(
        world.made().is_empty(),
        "no request, not even a list of models"
    );
    world.nothing_was_saved(TEMPLATE);
}

#[test]
fn the_machine_itself_and_https_get_the_key_question() {
    for (n, address) in [
        "http://localhost:1234/v1",
        "http://127.0.0.1:1234/v1",
        "http://[::1]:1234/v1",
        "https://llm.example.org/v1",
    ]
    .into_iter()
    .enumerate()
    {
        let world = Setup::default().build();
        let mut t = Scripted::new(&["5", address, KEY, "1"]);
        assert_eq!(
            world.run(&["ai", "setup"], &mut t),
            0,
            "#{n} {address}: {}",
            t.everything()
        );
        assert!(t.asked("API key"), "{address}");
        assert_eq!(world.stored(Kind::OpenAiCompatible).as_deref(), Some(KEY));
    }
}

#[test]
fn a_look_alike_host_does_not_get_the_key() {
    for address in [
        "http://localhost.evil.example/v1",
        "http://127.0.0.1.evil.example/v1",
        "http://10.0.0.5:8080/v1",
    ] {
        let world = Setup::default().build();
        let mut t = Scripted::new(&["5", address, "n"]);
        assert_eq!(world.run(&["ai", "setup"], &mut t), 1, "{address}");
        assert!(!t.asked("API key"), "{address}");
    }
}

#[test]
fn a_stored_key_is_not_sent_to_a_plain_http_address_by_test() {
    // The real check, not the fake: `omsh ai test` with a key stored and a base_url that is
    // plain http elsewhere must fail before any request.
    let config = "[ai]\nprovider = \"openai-compatible\"\nmodel = \"m\"\n\
                  base_url = \"http://example.com/v1\"\n";
    let world = Setup {
        config_text: config,
        ..Setup::default()
    }
    .build();
    store_key(&world, Kind::OpenAiCompatible, KEY);
    // Swap in the real provider factory.
    let session = AiSession::new(
        &parse(config).config,
        false,
        Environment {
            keys: Keys::with(
                Box::new({
                    let ring = world.ring.clone();
                    move || Ok(Box::new(ring.clone()) as Box<dyn KeyStore>)
                }),
                Box::new(CredentialsFile::new(world.credentials_file())),
            ),
            config_file: None,
            make_provider: Arc::new(|settings, key| {
                omsh_ai::GenaiProvider::new(settings, key).map(|p| Box::new(p) as Box<dyn Provider>)
            }),
        },
    );
    let mut t = Scripted::new(&[]);
    let status = session.run(&[b"ai".to_vec(), b"test".to_vec()], &mut t);
    assert_eq!(status, 1);
    assert!(
        t.err.contains("the key is not sent to example.com"),
        "{}",
        t.err
    );
    assert!(!t.everything().contains(KEY));
    assert!(!t.err.contains("Asking"), "no request was made: {}", t.err);
}

// -- setup: when it may not run ---------------------------------------------------------

#[test]
fn setup_is_refused_under_no_ai_and_touches_nothing() {
    let world = Setup {
        no_ai: true,
        ..Setup::default()
    }
    .build();
    let mut t = Scripted::new(&[]);
    assert_eq!(world.run(&["ai", "setup"], &mut t), 1);
    assert!(
        t.err.contains("AI is off for this session (--no-ai)"),
        "{}",
        t.err
    );
    assert!(t.prompts.is_empty());
    assert!(!world.keyring_was_used());
    assert!(world.made().is_empty());
}

#[test]
fn setup_is_refused_without_a_config_file_to_write() {
    let world = Setup {
        no_config: true,
        ..Setup::default()
    }
    .build();
    let mut t = Scripted::new(&[]);
    assert_eq!(world.run(&["ai", "setup"], &mut t), 1);
    assert!(t.err.contains("does not use one"), "{}", t.err);
    assert!(t.prompts.is_empty());
}

#[test]
fn setup_needs_a_terminal() {
    let world = Setup::default().build();
    let mut t = Scripted::new(&[]);
    t.is_terminal = false;
    assert_eq!(world.run(&["ai", "setup"], &mut t), 1);
    assert!(t.err.contains("needs a terminal"), "{}", t.err);
    assert!(t.prompts.is_empty());
}

#[test]
fn a_config_file_that_cannot_be_edited_gets_the_lines_to_paste() {
    let broken = "[ui\nicons = 1\n";
    let world = Setup {
        config_text: broken,
        ..Setup::default()
    }
    .build();
    let mut t = Scripted::new(&compatible_server());
    assert_eq!(world.run(&["ai", "setup"], &mut t), 1);
    assert!(
        t.err.contains("config.toml could not be changed"),
        "{}",
        t.err
    );
    assert!(
        t.err.contains("[ai]\nprovider = \"openai-compatible\"\n"),
        "{}",
        t.err
    );
    assert!(t.err.contains("model = \"alpha\""));
    assert_eq!(world.config_text(), broken, "the broken file is untouched");
    // The key was stored, and this session uses the settings.
    assert_eq!(world.stored(Kind::OpenAiCompatible).as_deref(), Some(KEY));
    let mut status = Scripted::new(&[]);
    world.run(&["ai", "status"], &mut status);
    assert!(status.out.starts_with("AI: ready"), "{}", status.out);
    assert!(!t.err.contains(KEY));
}

// -- status, test, forget ---------------------------------------------------------------

#[test]
fn status_before_setup_says_what_to_do_and_asks_the_keyring_nothing() {
    let world = Setup::default().build();
    let mut t = Scripted::new(&[]);
    assert_eq!(world.run(&["ai", "status"], &mut t), 0);
    assert_eq!(t.out, "AI: not set up yet (run: omsh ai setup)\n");
    assert!(!world.keyring_was_used());
}

#[test]
fn status_names_everything_but_the_key() {
    let config = "[ai]\nprovider = \"anthropic\"\nmodel = \"some-model\"\n";
    let world = Setup {
        config_text: config,
        ..Setup::default()
    }
    .build();
    store_key(&world, Kind::Anthropic, KEY);
    let mut t = Scripted::new(&[]);
    assert_eq!(world.run(&["ai", "status"], &mut t), 0);
    assert_eq!(
        t.out,
        "AI: ready\nprovider: Anthropic (anthropic)\nmodel: some-model\n\
         address: api.anthropic.com (default)\nkey: stored in the test keyring\n"
    );
    assert!(!t.everything().contains(KEY));
}

#[test]
fn status_under_no_ai_shows_the_flag_and_does_not_look_for_a_key() {
    let config = "[ai]\nprovider = \"anthropic\"\nmodel = \"m\"\n";
    let world = Setup {
        config_text: config,
        no_ai: true,
        ..Setup::default()
    }
    .build();
    let mut t = Scripted::new(&[]);
    assert_eq!(world.run(&["ai", "status"], &mut t), 0);
    assert!(
        t.out.starts_with("AI: off for this session (--no-ai)\n"),
        "{}",
        t.out
    );
    assert!(!t.out.contains("key:"), "{}", t.out);
    assert!(!world.keyring_was_used(), "--no-ai: nothing is started");
}

#[test]
fn status_under_a_disabled_config() {
    let world = Setup {
        config_text: "[ai]\nenabled = false\nprovider = \"ollama\"\nmodel = \"m\"\n",
        ..Setup::default()
    }
    .build();
    let mut t = Scripted::new(&[]);
    world.run(&["ai", "status"], &mut t);
    assert!(
        t.out
            .starts_with("AI: off ([ai] enabled = false in config.toml)\n"),
        "{}",
        t.out
    );
}

#[test]
fn status_for_a_key_that_is_missing() {
    let world = Setup {
        config_text: "[ai]\nprovider = \"openai\"\nmodel = \"m\"\n",
        ..Setup::default()
    }
    .build();
    let mut t = Scripted::new(&[]);
    world.run(&["ai", "status"], &mut t);
    assert!(
        t.out.contains("key: not stored (run: omsh ai setup)"),
        "{}",
        t.out
    );
}

#[test]
fn test_reports_the_reason_when_it_cannot_run() {
    for (setup, expected) in [
        (
            Setup::default(),
            "AI is not set up yet (run: omsh ai setup)",
        ),
        (
            Setup {
                no_ai: true,
                config_text: "[ai]\nprovider = \"ollama\"\nmodel = \"m\"\n",
                ..Setup::default()
            },
            "AI is off for this session (--no-ai)",
        ),
    ] {
        let world = setup.build();
        let mut t = Scripted::new(&[]);
        assert_eq!(world.run(&["ai", "test"], &mut t), 1);
        assert!(t.err.contains(expected), "{}", t.err);
        assert!(world.made().is_empty());
        assert!(!world.keyring_was_used());
    }
}

#[test]
fn test_without_a_stored_key_says_so() {
    let world = Setup {
        config_text: "[ai]\nprovider = \"openai\"\nmodel = \"m\"\n",
        ..Setup::default()
    }
    .build();
    let mut t = Scripted::new(&[]);
    assert_eq!(world.run(&["ai", "test"], &mut t), 1);
    assert!(
        t.err
            .contains("no key is stored for OpenAI; run `omsh ai setup`"),
        "{}",
        t.err
    );
    assert!(world.made().is_empty(), "no request without a key");
}

#[test]
fn test_with_ollama_needs_no_key() {
    let world = Setup {
        config_text: "[ai]\nprovider = \"ollama\"\nmodel = \"llama\"\n",
        ..Setup::default()
    }
    .build();
    let mut t = Scripted::new(&[]);
    assert_eq!(world.run(&["ai", "test"], &mut t), 0, "{}", t.everything());
    assert!(!world.keyring_was_used());
}

#[test]
fn test_shows_a_wrong_key_as_one_clear_line() {
    let world = Setup {
        config_text: "[ai]\nprovider = \"openai\"\nmodel = \"m\"\n",
        behavior: rejecting_key("sk-wrong"),
        ..Setup::default()
    }
    .build();
    store_key(&world, Kind::OpenAi, "sk-wrong");
    let mut t = Scripted::new(&[]);
    assert_eq!(world.run(&["ai", "test"], &mut t), 1);
    assert!(
        t.err
            .contains("omsh: ai: OpenAI did not accept the key (HTTP 401 Unauthorized)"),
        "{}",
        t.err
    );
    assert!(!t.everything().contains("sk-wrong"));
}

#[test]
fn test_when_offline_points_to_a_local_model() {
    let world = Setup {
        config_text: "[ai]\nprovider = \"anthropic\"\nmodel = \"m\"\n",
        behavior: unreachable_server(),
        ..Setup::default()
    }
    .build();
    store_key(&world, Kind::Anthropic, KEY);
    let mut t = Scripted::new(&[]);
    assert_eq!(world.run(&["ai", "test"], &mut t), 1);
    assert!(t.err.contains("Ollama"), "{}", t.err);
}

#[test]
fn forget_removes_the_key_of_the_chosen_provider_only() {
    let world = Setup {
        config_text: "[ai]\nprovider = \"anthropic\"\nmodel = \"m\"\n",
        ..Setup::default()
    }
    .build();
    store_key(&world, Kind::Anthropic, KEY);
    store_key(&world, Kind::OpenAi, "sk-other");
    let mut t = Scripted::new(&[]);
    assert_eq!(world.run(&["ai", "forget"], &mut t), 0);
    assert_eq!(t.out, "The key for Anthropic was removed.\n");
    assert_eq!(world.stored(Kind::Anthropic), None);
    assert_eq!(world.stored(Kind::OpenAi).as_deref(), Some("sk-other"));
    let mut again = Scripted::new(&[]);
    world.run(&["ai", "forget"], &mut again);
    assert_eq!(again.out, "No key was stored for Anthropic.\n");
}

#[test]
fn forget_under_no_ai_does_nothing() {
    let world = Setup {
        no_ai: true,
        config_text: "[ai]\nprovider = \"anthropic\"\nmodel = \"m\"\n",
        ..Setup::default()
    }
    .build();
    let mut t = Scripted::new(&[]);
    assert_eq!(world.run(&["ai", "forget"], &mut t), 1);
    assert!(!world.keyring_was_used());
}

// -- the words --------------------------------------------------------------------------

#[test]
fn help_and_mistakes() {
    let world = Setup::default().build();
    for words in [&[][..], &["help"], &["--help"]] {
        let mut t = Scripted::new(&[]);
        assert_eq!(world.run(words, &mut t), 0);
        assert!(t.out.starts_with("usage: omsh <command>"), "{}", t.out);
    }
    for words in [&["ai"][..], &["ai", "help"]] {
        let mut t = Scripted::new(&[]);
        assert_eq!(world.run(words, &mut t), 0);
        assert!(t.out.starts_with("usage: omsh ai <command>"), "{}", t.out);
    }
    let mut t = Scripted::new(&[]);
    assert_eq!(world.run(&["ai", "frobnicate"], &mut t), 2);
    assert!(t.err.contains("frobnicate: unknown command"), "{}", t.err);
    let mut t = Scripted::new(&[]);
    assert_eq!(world.run(&["frobnicate"], &mut t), 2);
    assert!(t.err.contains("frobnicate: unknown command"), "{}", t.err);
    // The usage lists the tour.
    let mut t = Scripted::new(&[]);
    assert_eq!(world.run(&[], &mut t), 0);
    assert!(t.out.contains("tour"), "{}", t.out);
    // The tour itself, with no terminal: all six pages, no question, status 0.
    let mut t = Scripted::new(&[]);
    t.is_terminal = false;
    assert_eq!(world.run(&["tour"], &mut t), 0);
    assert!(t.out.contains("(6 of 6)"), "{}", t.out);
    assert!(!world.keyring_was_used());
}

#[test]
fn a_poisoned_lock_does_not_lock_the_commands_out() {
    let world = Setup::default().build();
    // A panic inside a command while the lock is held (a scripted terminal that runs out of
    // answers panics, as a bug would).
    let session = &world.session;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut t = Scripted::new(&[]);
        session.run(&[b"ai".to_vec(), b"setup".to_vec()], &mut t)
    }));
    assert!(result.is_err());
    let mut t = Scripted::new(&[]);
    assert_eq!(
        world.run(&["ai", "status"], &mut t),
        0,
        "{}",
        t.everything()
    );
}
