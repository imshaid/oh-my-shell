//! `omsh ai ...`: setting up and checking the AI layer (S20).
//!
//! The executor knows the `omsh` builtin only as a hook ([`omsh_exec::OmshCommand`]); this is
//! what the hook runs. **Nothing here is reached by a normal command**: a person has to type
//! `omsh ai ...` first. Only then are the keyring, the TLS setup, the tokio runtime and the
//! HTTP client created, by the calls below, and not before.
//!
//! | Command | What it does | Touches |
//! |---|---|---|
//! | `omsh ai status` | shows the settings and where the key is | keyring (only to see if a key is there) |
//! | `omsh ai setup` | asks for provider, address, key and model, checks them, saves | keyring or file, `config.toml`, the network |
//! | `omsh ai test` | one small request with the stored key | keyring or file, the network |
//! | `omsh ai forget` | removes the stored key | keyring or file |
//!
//! Under `--no-ai` everything but `status` and the help is refused, and none of them touches
//! anything.

pub mod explain;
pub mod history_search;
pub mod plan;
pub mod recent;
mod setup;
#[cfg(test)]
mod tests;
mod wait;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use omsh_ai::{
    ErrorKind, GenaiProvider, Keys, Policy, Provider, ProviderError, Secret, Settings, runtime,
    verify,
};
use omsh_config::{Ai, AiState, Config, Paths, Provider as Kind};
use omsh_exec::{OmshCommand, Terminal};

/// Builds the provider for some settings and a key. Replaced in tests, which have no network.
pub type MakeProvider = Arc<
    dyn Fn(&Settings, Option<&Secret>) -> Result<Box<dyn Provider>, ProviderError> + Send + Sync,
>;

/// What the commands need from outside.
pub struct Environment {
    pub keys: Keys,
    /// `config.toml`, when this shell may write one (not under `--no-config`, and only with a
    /// configuration directory).
    pub config_file: Option<PathBuf>,
    pub make_provider: MakeProvider,
}

impl Environment {
    /// The real thing: the system keyring with a credentials file beside `config.toml`, and
    /// genai.
    #[must_use]
    pub fn system(paths: &Paths, no_config: bool) -> Self {
        Self {
            keys: Keys::system(paths.config_dir().map(|dir| dir.join("credentials"))),
            config_file: if no_config { None } else { paths.config_file() },
            make_provider: Arc::new(|settings, key| {
                GenaiProvider::new(settings, key).map(|p| Box::new(p) as Box<dyn Provider>)
            }),
        }
    }
}

struct Inner {
    /// The `[ai]` section as the file has it (changed in memory after a setup).
    ai: Ai,
    /// `--no-ai`.
    no_ai: bool,
    state: AiState,
    env: Environment,
}

/// The hook behind the `omsh` builtin. Shared by every copy of the shell; commands run one at
/// a time.
pub struct AiSession {
    inner: Mutex<Inner>,
}

impl AiSession {
    #[must_use]
    pub fn new(config: &Config, no_ai: bool, env: Environment) -> Self {
        Self {
            inner: Mutex::new(Inner {
                ai: config.ai.clone(),
                no_ai,
                state: AiState::new(&config.ai, no_ai),
                env,
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A panic in an earlier command must not lock the session for good.
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

const USAGE: &str = "\
usage: omsh ai <command>
  setup    choose a provider and a model, store the key, check that it works
  status   show the settings and where the key is kept
  test     send one small request to check the key and the model
  forget   remove the stored key of the chosen provider
";

const OMSH_USAGE: &str = "\
usage: omsh <command>
  ai       set up and check the AI layer (omsh ai for the list)
  tour     a one-minute walk through what the shell does
";

/// Exit status of a command that was used wrongly (as the other builtins).
const USAGE_STATUS: u8 = 2;
/// Exit status of a command that was given up with Ctrl+C: 128 + SIGINT.
const CANCELLED: u8 = 130;

impl OmshCommand for AiSession {
    fn run(&self, args: &[Vec<u8>], terminal: &mut dyn Terminal) -> u8 {
        let words: Vec<String> = args
            .iter()
            .map(|word| String::from_utf8_lossy(word).into_owned())
            .collect();
        let words: Vec<&str> = words.iter().map(String::as_str).collect();
        match words.as_slice() {
            [] | ["help" | "--help" | "-h"] => {
                terminal.out(OMSH_USAGE);
                0
            }
            ["tour"] => crate::tour::run(terminal),
            ["ai"] | ["ai", "help" | "--help" | "-h"] => {
                terminal.out(USAGE);
                0
            }
            ["ai", "status"] => self.lock().status(terminal),
            ["ai", "setup"] => self.lock().setup(terminal),
            ["ai", "test"] => self.lock().test(terminal),
            ["ai", "forget"] => self.lock().forget(terminal),
            ["ai", other, ..] => {
                terminal.err(&format!("omsh: ai: {other}: unknown command\n{USAGE}"));
                USAGE_STATUS
            }
            [other, ..] => {
                terminal.err(&format!("omsh: {other}: unknown command\n{OMSH_USAGE}"));
                USAGE_STATUS
            }
        }
    }
}

impl AiSession {
    /// Everything a `? request` needs, taken under the lock and then handed over: the lock is
    /// not held while the plan runs, because a step may itself run `omsh ai ...`.
    ///
    /// # Errors
    ///
    /// One line saying why the AI layer cannot be used right now.
    pub fn plan_setup(&self) -> Result<plan::PlanSetup, String> {
        let inner = self.lock();
        if let Some(why) = inner.unavailable() {
            return Err(why);
        }
        let settings = inner.settings().ok_or("AI is not set up".to_owned())?;
        let key = inner.stored_key(settings.kind)?;
        // A provider (and with it a connection) is made for each request and dropped when the
        // request ends, so that no socket stays open while a panel waits for a key (S20 rule).
        let make = Arc::clone(&inner.env.make_provider);
        let own = settings.clone();
        Ok(plan::PlanSetup {
            connect: Box::new(move || make(&own, key.as_ref())),
            settings,
            context: inner.ai.context.clone(),
        })
    }
}

impl Inner {
    fn kind(&self) -> Option<Kind> {
        self.ai.provider
    }

    /// The settings of a provider that is set up.
    fn settings(&self) -> Option<Settings> {
        match &self.state {
            AiState::Ready {
                provider,
                model,
                base_url,
            } => Some(Settings {
                kind: *provider,
                model: model.clone(),
                base_url: base_url.clone(),
            }),
            _ => None,
        }
    }

    /// Why the AI layer cannot be used right now, as a line, if it cannot.
    fn unavailable(&self) -> Option<String> {
        match &self.state {
            AiState::Ready { .. } => None,
            other => Some(format!("AI is {}", other.describe())),
        }
    }

    // -- status -----------------------------------------------------------------------

    fn status(&self, terminal: &mut dyn Terminal) -> u8 {
        let mut text = format!("AI: {}\n", self.state.describe());
        if let Some(kind) = self.kind() {
            text.push_str(&format!("provider: {} ({})\n", kind.label(), kind.as_str()));
        }
        if let Some(model) = &self.ai.model {
            text.push_str(&format!("model: {model}\n"));
        }
        if let Some(kind) = self.kind() {
            let own = self.ai.base_url.as_deref();
            let settings = Settings {
                kind,
                model: String::new(),
                base_url: own.map(str::to_owned),
            };
            let address = settings
                .endpoint()
                .map_or("not set".to_owned(), |url| omsh_ai::server_of(&url));
            let how = if own.is_some() {
                "from config.toml"
            } else {
                "default"
            };
            text.push_str(&format!("address: {address} ({how})\n"));
        }
        // The flag means nothing starts, so the keyring is not even asked.
        if !self.no_ai
            && let Some(kind) = self.kind()
        {
            text.push_str(&format!("key: {}\n", self.key_line(kind)));
        }
        terminal.out(&text);
        0
    }

    /// Where the key of `kind` is, in words. Never the key.
    fn key_line(&self, kind: Kind) -> String {
        if !kind.takes_a_key() {
            return "not needed".to_owned();
        }
        match self.env.keys.get(kind) {
            Ok(Some((_key, place))) => format!("stored in {place}"),
            Ok(None) if kind.key_is_required() => "not stored (run: omsh ai setup)".to_owned(),
            Ok(None) => "not stored (optional for this server)".to_owned(),
            Err(error) => format!("cannot be read: {error}"),
        }
    }

    // -- test -------------------------------------------------------------------------

    fn test(&self, terminal: &mut dyn Terminal) -> u8 {
        if let Some(why) = self.unavailable() {
            terminal.err(&format!("omsh: ai: {why}\n"));
            return 1;
        }
        let Some(settings) = self.settings() else {
            return 1;
        };
        let name = settings.kind.label();
        let key = match self.stored_key(settings.kind) {
            Ok(key) => key,
            Err(line) => {
                terminal.err(&format!("omsh: ai: {line}\n"));
                return 1;
            }
        };
        let provider = match (self.env.make_provider)(&settings, key.as_ref()) {
            Ok(provider) => provider,
            Err(error) => {
                terminal.err(&format!("omsh: ai: {}\n", error.one_line(settings.kind)));
                return 1;
            }
        };
        // The provider has its own copy now.
        drop(key);

        terminal.err(&format!("Asking {name} ({})...\n", settings.model));
        let started = Instant::now();
        let outcome = {
            let terminal: &dyn Terminal = &*terminal;
            // The provider is dropped inside the request's runtime, so that the connection it
            // keeps open is closed before the runtime stops (see `runtime::block_on`).
            runtime::block_on(
                async move {
                    let result = verify(provider.as_ref(), &Policy::standard()).await;
                    drop(provider);
                    result
                },
                &|| terminal.interrupted(),
            )
        };
        let result = outcome.unwrap_or_else(Err);
        match result {
            Ok(reply) => {
                terminal.out(&format!(
                    "ok: {name} answered in {:.1} s with model {}{}\n",
                    started.elapsed().as_secs_f64(),
                    settings.model,
                    one_line_reply(&reply),
                ));
                0
            }
            Err(error) if error.kind == ErrorKind::Cancelled => {
                terminal.err("cancelled\n");
                CANCELLED
            }
            Err(error) => {
                terminal.err(&format!("omsh: ai: {}\n", error.one_line(settings.kind)));
                1
            }
        }
    }

    /// The key of `kind` from the stores. `Ok(None)` for a provider that has none or needs
    /// none; an error line when a required key is missing or the stores cannot be read.
    fn stored_key(&self, kind: Kind) -> Result<Option<Secret>, String> {
        if !kind.takes_a_key() {
            return Ok(None);
        }
        match self.env.keys.get(kind) {
            Ok(Some((key, _place))) => Ok(Some(key)),
            Ok(None) if kind.key_is_required() => {
                Err(ProviderError::new(ErrorKind::NoKey, "").one_line(kind))
            }
            Ok(None) => Ok(None),
            Err(error) => Err(format!("the key cannot be read: {error}")),
        }
    }

    // -- forget -----------------------------------------------------------------------

    fn forget(&self, terminal: &mut dyn Terminal) -> u8 {
        if self.no_ai {
            terminal.err("omsh: ai: AI is off for this session (--no-ai)\n");
            return 1;
        }
        let Some(kind) = self.kind() else {
            terminal.err("omsh: ai: no provider is chosen, so there is no key to forget\n");
            return 1;
        };
        if !kind.takes_a_key() {
            terminal.out(&format!("{} has no key.\n", kind.label()));
            return 0;
        }
        match self.env.keys.delete(kind) {
            Ok(true) => terminal.out(&format!("The key for {} was removed.\n", kind.label())),
            Ok(false) => terminal.out(&format!("No key was stored for {}.\n", kind.label())),
            Err(error) => {
                terminal.err(&format!(
                    "omsh: ai: the key could not be removed: {error}\n"
                ));
                return 1;
            }
        }
        0
    }
}

/// ` (it said: "OK")` for a short reply, nothing for an empty one.
fn one_line_reply(reply: &str) -> String {
    let line = reply
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or_default();
    let line: String = line.trim().chars().take(60).collect();
    if line.is_empty() {
        String::new()
    } else {
        format!(" (it said: \"{line}\")")
    }
}
