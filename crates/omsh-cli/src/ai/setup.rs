//! `omsh ai setup`: the wizard (blueprint §13.3).
//!
//! Order of events: provider, address, key, model, then a check with one small real request,
//! and only when that works (or the person says to save anyway) is anything written: first the
//! key (keyring or 0600 file), then the three `[ai]` keys of `config.toml`. Ctrl+C or the end
//! of input at any question leaves everything as it was.
//!
//! The key is typed with echo off, read straight from the terminal (never through the line
//! editor, so it cannot reach the history), and dropped as soon as it has been checked and
//! stored.

use omsh_ai::{ErrorKind, Policy, ProviderError, Secret, Settings, key_may_go_to, runtime, verify};
use omsh_config::{AiChoice, AiState, Provider as Kind, check_url, set_ai};
use omsh_exec::{ReadError, Terminal};

use super::{CANCELLED, Inner};

/// How many times a wrong answer to the same question is tried again.
const TRIES: u32 = 3;
/// The address Ollama listens on, offered as the default.
const OLLAMA_ADDRESS: &str = "http://localhost:11434";
/// The longest model list that is shown.
const LIST_LIMIT: usize = 30;

/// Why the wizard ended without saving.
enum Stop {
    /// Ctrl+C.
    Cancelled,
    /// The input ended.
    EndOfInput,
    /// Anything else, with the sentence to show.
    Failed(String),
}

type Step<T> = Result<T, Stop>;

fn failed<T>(message: impl Into<String>) -> Step<T> {
    Err(Stop::Failed(message.into()))
}

/// What has been decided so far.
struct Draft {
    kind: Kind,
    /// `[ai] base_url`: `None` for the provider's own address.
    address: Option<String>,
    /// A key that is in use for the checks.
    key: Option<Secret>,
    /// Whether `key` was typed now (and so has to be stored) and not read from a store.
    key_is_new: bool,
    /// Whether the key question is still to be asked: for every provider that takes a key
    /// (Enter answers "none" where it is optional), and for the others only after the server
    /// asked for one.
    wants_key: bool,
    model: String,
    /// The model that is set now, which Enter keeps, when setup is run again.
    keep_model: Option<String>,
}

impl Draft {
    fn settings(&self, model: &str) -> Settings {
        Settings {
            kind: self.kind,
            model: model.to_owned(),
            base_url: self.address.clone(),
        }
    }
}

/// How the check ended, when it did not end the wizard.
enum Check {
    Passed,
    /// The provider did not accept the key.
    WrongKey,
    /// The provider does not know the model.
    WrongModel,
    /// Something else is wrong (network, server): the person decides.
    Trouble(ProviderError),
}

impl Inner {
    pub(super) fn setup(&mut self, terminal: &mut dyn Terminal) -> u8 {
        match self.wizard(terminal) {
            Ok(()) => 0,
            Err(Stop::Cancelled) => {
                terminal.err("\ncancelled; nothing was saved\n");
                CANCELLED
            }
            Err(Stop::EndOfInput) => {
                terminal.err("\nthe input ended; nothing was saved\n");
                1
            }
            Err(Stop::Failed(message)) => {
                terminal.err(&format!("omsh: ai: setup: {message}\n"));
                1
            }
        }
    }

    fn wizard(&mut self, t: &mut dyn Terminal) -> Step<()> {
        if self.no_ai {
            return failed("AI is off for this session (--no-ai)");
        }
        let Some(config_file) = self.env.config_file.clone() else {
            return failed(
                "setup writes config.toml, and this shell does not use one \
                 (--no-config, or no configuration directory)",
            );
        };
        if !t.input_is_terminal() {
            return failed("setup asks questions and needs a terminal");
        }

        t.err(
            "Set up the AI layer. Press Ctrl+C at any question to stop; nothing is saved then.\n\n",
        );
        let kind = self.ask_provider(t)?;
        let address = ask_address(
            t,
            kind,
            self.same_provider(kind, |ai| ai.base_url.as_deref()),
        )?;
        let mut draft = Draft {
            kind,
            address,
            key: None,
            key_is_new: false,
            wants_key: kind.takes_a_key(),
            model: String::new(),
            keep_model: self
                .same_provider(kind, |ai| ai.model.as_deref())
                .map(str::to_owned),
        };
        // A key goes only to https or to this machine. Say so now, before asking for it.
        if kind.takes_a_key() && !destination_ok(&draft) {
            self.plain_http_without_key(t, &mut draft)?;
        }

        let trouble = self.check_until_it_passes(t, &mut draft)?;

        // The check is over: from here the key is only stored, then dropped.
        let mut stored_in = None;
        if let Some(key) = draft.key.take() {
            if draft.key_is_new {
                match self.env.keys.set(kind, &key) {
                    Ok(stored) => stored_in = Some(stored),
                    Err(error) => {
                        return failed(format!("the key could not be stored: {error}"));
                    }
                }
            }
            drop(key);
        }

        let choice = AiChoice {
            provider: kind,
            model: draft.model.clone(),
            base_url: draft.address.clone(),
        };
        let saved = set_ai(&config_file, &choice);
        // This session uses the new settings at once, whether or not the file took them.
        self.ai.provider = Some(kind);
        self.ai.model = Some(draft.model.clone());
        self.ai.base_url = draft.address.clone();
        self.ai.enabled = true;
        self.state = AiState::new(&self.ai, self.no_ai);

        let mut report = String::new();
        if let Some(stored) = &stored_in {
            report.push_str(&format!("\nThe key is stored in {}.\n", stored.place));
            if let Some(problem) = &stored.keyring_problem {
                report.push_str(&format!(
                    "(The system keyring could not be used: {problem}. The file is readable \
                     only by you, mode 0600.)\n"
                ));
            }
        }
        match saved {
            Ok(edited) => {
                if edited.switched_on {
                    report.push_str("[ai] enabled was false in config.toml; it is true now.\n");
                }
                report.push_str(&format!(
                    "The settings are in {}.\n\nAI is set up: {}, model {}.\n",
                    config_file.display(),
                    kind.label(),
                    draft.model
                ));
                if let Some(problem) = trouble {
                    report.push_str(&format!(
                        "It was saved without a successful check ({}). \
                         Run `omsh ai test` when the server can be reached.\n",
                        problem.detail
                    ));
                } else {
                    report.push_str("Check it again at any time with: omsh ai test\n");
                }
                t.err(&report);
                Ok(())
            }
            Err(error) => {
                report.push_str(&format!(
                    "\nconfig.toml could not be changed ({error}). This session already uses \
                     the settings. To keep them, put these lines in {}:\n\n{}",
                    config_file.display(),
                    choice.lines()
                ));
                t.err(&report);
                failed("the settings were not saved to config.toml")
            }
        }
    }

    /// The value of a field of `[ai]` as it is now, if the provider has not changed.
    fn same_provider<'a, T>(
        &'a self,
        kind: Kind,
        field: impl FnOnce(&'a omsh_config::Ai) -> Option<T>,
    ) -> Option<T> {
        (self.ai.provider == Some(kind))
            .then(|| field(&self.ai))
            .flatten()
    }

    // -- questions ---------------------------------------------------------------------

    fn ask_provider(&self, t: &mut dyn Terminal) -> Step<Kind> {
        let mut menu = String::from("Choose a provider:\n");
        let notes = [
            "runs on this computer, needs no key",
            "",
            "",
            "",
            "LM Studio, llama.cpp, vLLM and the like",
        ];
        for (number, (kind, note)) in Kind::ALL.iter().zip(notes).enumerate() {
            let note = if note.is_empty() {
                String::new()
            } else {
                format!(" - {note}")
            };
            menu.push_str(&format!("  {}) {}{note}\n", number + 1, kind.label()));
        }
        t.err(&menu);
        let current = self.ai.provider;
        let hint = current.map_or(String::new(), |kind| {
            format!(" [Enter keeps {}]", kind.as_str())
        });
        for _ in 0..TRIES {
            let answer = ask(t, &format!("Number (1-{}){hint}: ", Kind::ALL.len()), false)?;
            if answer.is_empty()
                && let Some(kind) = current
            {
                return Ok(kind);
            }
            let by_number = answer
                .parse::<usize>()
                .ok()
                .and_then(|n| n.checked_sub(1))
                .and_then(|i| Kind::ALL.get(i).copied());
            let by_name = Kind::ALL
                .iter()
                .copied()
                .find(|kind| kind.as_str().eq_ignore_ascii_case(&answer));
            if let Some(kind) = by_number.or(by_name) {
                return Ok(kind);
            }
            t.err("That is not one of the choices.\n");
        }
        failed("no provider was chosen")
    }

    /// Asks for the key until there is one, or the person says there is none.
    fn ask_key(&self, t: &mut dyn Terminal, draft: &mut Draft) -> Step<()> {
        let required = draft.kind.key_is_required();
        let prompt = if required {
            format!("{} API key (what you type is hidden): ", draft.kind.label())
        } else {
            "API key, or Enter for none (what you type is hidden): ".to_owned()
        };
        match ask_secret(t, &prompt)? {
            Typed::Key(key) => {
                draft.key = Some(key);
                draft.key_is_new = true;
            }
            Typed::Nothing => {
                // Enter keeps a key that is stored already.
                match self.env.keys.get(draft.kind) {
                    Ok(Some((key, place))) => {
                        t.err(&format!("Using the key already stored in {place}.\n"));
                        draft.key = Some(key);
                        draft.key_is_new = false;
                    }
                    Ok(None) | Err(_) if !required => draft.wants_key = false,
                    Ok(None) | Err(_) => t.err("A key is needed for this provider.\n"),
                }
            }
            Typed::Invalid => t.err("That is not a usable key (it has a control character).\n"),
        }
        Ok(())
    }

    /// The address is plain http to another machine, so a key would not be sent to it.
    fn plain_http_without_key(&self, t: &mut dyn Terminal, draft: &mut Draft) -> Step<()> {
        let server = omsh_ai::server_of(&draft.settings("").endpoint().unwrap_or_default());
        t.err(&format!(
            "\n{server} is reached over plain http and is not this computer, so an API key \
             would not be sent to it: anyone on the way could read it. Use an https:// address \
             if the server needs a key.\n"
        ));
        if ask_yes_no(t, "Continue without a key?")? {
            draft.wants_key = false;
            Ok(())
        } else {
            failed("stopped; nothing was saved")
        }
    }

    // -- the check ---------------------------------------------------------------------

    /// Asks for what is missing and checks it, again after a wrong key or a wrong model.
    /// Returns the trouble that the person chose to ignore, if any.
    fn check_until_it_passes(
        &self,
        t: &mut dyn Terminal,
        draft: &mut Draft,
    ) -> Step<Option<ProviderError>> {
        for _ in 0..TRIES {
            if draft.wants_key && draft.key.is_none() {
                self.ask_key(t, draft)?;
                if draft.wants_key && draft.key.is_none() {
                    continue;
                }
            }
            if draft.model.is_empty() {
                match self.ask_model(t, draft)? {
                    Some(model) => draft.model = model,
                    // The list call showed that the key is wrong.
                    None => continue,
                }
            }
            match self.check(t, draft)? {
                Check::Passed => return Ok(None),
                Check::WrongKey => {
                    draft.key = None;
                    draft.key_is_new = false;
                    if !draft.kind.takes_a_key() {
                        return failed("the server refused the request");
                    }
                    // A server that did not need a key has asked for one.
                    draft.wants_key = true;
                    t.err("The provider did not accept the key.\n");
                }
                Check::WrongModel => {
                    t.err(&format!(
                        "{} does not know the model {}.\n",
                        draft.kind.label(),
                        draft.model
                    ));
                    draft.model.clear();
                    draft.keep_model = None;
                }
                Check::Trouble(error) => {
                    t.err(&format!("{}\n", error.one_line(draft.kind)));
                    if ask_yes_no(t, "Save the settings anyway?")? {
                        return Ok(Some(error));
                    }
                    return failed("stopped; nothing was saved");
                }
            }
        }
        failed("giving up after 3 tries; nothing was saved")
    }

    /// The model: picked from the list the provider offers, or typed. `None` when asking for
    /// the list showed that the key is wrong.
    fn ask_model(&self, t: &mut dyn Terminal, draft: &Draft) -> Step<Option<String>> {
        let listed = self.list_models(t, draft);
        let mut models = match listed {
            Ok(models) => models,
            Err(error) if error.kind == ErrorKind::Cancelled => return Err(Stop::Cancelled),
            Err(error) if error.kind == ErrorKind::Auth && draft.kind.takes_a_key() => {
                t.err("The provider did not accept the key.\n");
                return Ok(None);
            }
            Err(error) => {
                // Not fatal: the model can be typed. The check at the end says more.
                t.err(&format!(
                    "(The list of models could not be fetched: {}.)\n",
                    error.one_line(draft.kind)
                ));
                Vec::new()
            }
        };
        models.sort();
        models.dedup();
        models.truncate(LIST_LIMIT);
        if models.is_empty() {
            let hint = if draft.kind == Kind::Ollama {
                " (see: ollama list)"
            } else {
                ""
            };
            let keep = keep_hint(draft);
            for _ in 0..TRIES {
                let answer = ask(t, &format!("Model name{hint}{keep}: "), false)?;
                if answer.is_empty()
                    && let Some(current) = &draft.keep_model
                {
                    return Ok(Some(current.clone()));
                }
                if valid_model(&answer) {
                    return Ok(Some(answer));
                }
            }
            return failed("no model was named");
        }
        let mut text = String::from("\nModels offered:\n");
        for (number, name) in models.iter().enumerate() {
            text.push_str(&format!("  {:>2}) {name}\n", number + 1));
        }
        t.err(&text);
        let keep = keep_hint(draft);
        for _ in 0..TRIES {
            let answer = ask(t, &format!("Model (number or name){keep}: "), false)?;
            if answer.is_empty()
                && let Some(current) = &draft.keep_model
            {
                return Ok(Some(current.clone()));
            }
            if let Some(name) = answer
                .parse::<usize>()
                .ok()
                .and_then(|n| n.checked_sub(1))
                .and_then(|i| models.get(i))
            {
                return Ok(Some(name.clone()));
            }
            if valid_model(&answer) {
                return Ok(Some(answer));
            }
        }
        failed("no model was chosen")
    }

    fn list_models(&self, t: &dyn Terminal, draft: &Draft) -> Result<Vec<String>, ProviderError> {
        let provider = (self.env.make_provider)(&draft.settings(""), draft.key.as_ref())?;
        runtime::block_on(
            async move {
                let result = provider.models().await;
                drop(provider);
                result
            },
            &|| t.interrupted(),
        )?
    }

    /// One small request with the settings so far.
    fn check(&self, t: &mut dyn Terminal, draft: &Draft) -> Step<Check> {
        let settings = draft.settings(&draft.model);
        let provider = match (self.env.make_provider)(&settings, draft.key.as_ref()) {
            Ok(provider) => provider,
            Err(error) => return Ok(Check::Trouble(error)),
        };
        t.err(&format!(
            "\nChecking with {} (model {})...\n",
            draft.kind.label(),
            draft.model
        ));
        let outcome = {
            let t: &dyn Terminal = &*t;
            // The provider (and the copy of the key it holds) is dropped inside the runtime,
            // so that the connection it keeps open is closed before the runtime stops.
            runtime::block_on(
                async move {
                    let result = verify(provider.as_ref(), &Policy::standard()).await;
                    drop(provider);
                    result
                },
                &|| t.interrupted(),
            )
        };
        let result = match outcome {
            Ok(result) => result,
            Err(error) => Err(error),
        };
        match result {
            Ok(_) => Ok(Check::Passed),
            Err(error) => match error.kind {
                ErrorKind::Cancelled => Err(Stop::Cancelled),
                ErrorKind::Auth => Ok(Check::WrongKey),
                ErrorKind::NotFound if draft.address.is_none() || draft.kind.takes_a_key() => {
                    Ok(Check::WrongModel)
                }
                _ => Ok(Check::Trouble(error)),
            },
        }
    }
}

/// Whether a key may be sent with these settings.
fn destination_ok(draft: &Draft) -> bool {
    draft
        .settings("")
        .endpoint()
        .is_none_or(|url| key_may_go_to(&url))
}

/// " [Enter keeps X]" when a model is set already.
fn keep_hint(draft: &Draft) -> String {
    draft
        .keep_model
        .as_ref()
        .map_or(String::new(), |model| format!(" [Enter keeps {model}]"))
}

fn valid_model(name: &str) -> bool {
    !name.is_empty() && !name.chars().any(char::is_control)
}

/// The address question, for the providers that have one.
fn ask_address(t: &mut dyn Terminal, kind: Kind, current: Option<&str>) -> Step<Option<String>> {
    let (prompt, default) = match kind {
        Kind::Ollama => {
            let default = current.unwrap_or(OLLAMA_ADDRESS);
            (format!("Ollama address [{default}]: "), Some(default))
        }
        Kind::OpenAiCompatible => match current {
            Some(current) => (format!("Server address [{current}]: "), Some(current)),
            None => (
                "Server address, for example http://localhost:8080/v1: ".to_owned(),
                None,
            ),
        },
        // The companies' own addresses are built in.
        _ => return Ok(None),
    };
    for _ in 0..TRIES {
        let mut answer = ask(t, &prompt, false)?;
        if answer.is_empty()
            && let Some(default) = default
        {
            answer = default.to_owned();
        }
        match check_url(&answer) {
            // The default address needs no entry in the file.
            Ok(()) if kind == Kind::Ollama && answer == OLLAMA_ADDRESS => return Ok(None),
            Ok(()) => return Ok(Some(answer)),
            Err(why) if answer.is_empty() => {
                let _ = why;
                t.err("An address is needed.\n");
            }
            Err(why) => t.err(&format!("That address {why}.\n")),
        }
    }
    failed("no usable address was given")
}

// -- reading answers --------------------------------------------------------------------

fn stop_for(error: ReadError) -> Stop {
    match error {
        ReadError::Interrupted => Stop::Cancelled,
        ReadError::EndOfInput => Stop::EndOfInput,
        ReadError::Failed => Stop::Failed("standard input cannot be read".to_owned()),
    }
}

/// A line, trimmed.
fn ask(t: &mut dyn Terminal, prompt: &str, hidden: bool) -> Step<String> {
    let mut line = Vec::with_capacity(256);
    t.read_line(prompt, hidden, &mut line).map_err(stop_for)?;
    Ok(String::from_utf8_lossy(&line).trim().to_owned())
}

fn ask_yes_no(t: &mut dyn Terminal, question: &str) -> Step<bool> {
    let answer = ask(t, &format!("{question} [y/N] "), false)?;
    Ok(matches!(answer.as_str(), "y" | "Y" | "yes" | "Yes" | "YES"))
}

/// What was typed at a secret prompt.
enum Typed {
    Key(Secret),
    Nothing,
    Invalid,
}

fn ask_secret(t: &mut dyn Terminal, prompt: &str) -> Step<Typed> {
    // Room for any key up front, so that typing never moves the text to a new buffer and
    // leaves a copy behind.
    let mut line = Vec::with_capacity(1024);
    let read = t.read_line(prompt, true, &mut line);
    let blank = line.iter().all(u8::is_ascii_whitespace);
    // Wipes `line` on every path.
    let key = Secret::take(&mut line);
    read.map_err(stop_for)?;
    Ok(match (key, blank) {
        (Some(key), _) => Typed::Key(key),
        (None, true) => Typed::Nothing,
        (None, false) => Typed::Invalid,
    })
}
