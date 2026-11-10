//! The typed configuration (blueprint §9) and its defaults.
//!
//! Most of these settings are read by parts of the shell that do not exist yet (the line
//! editor, history, the safety layer, the AI layer). They are defined and checked now, so the
//! first-run file can document all of them and a typo is reported from the first day.

use std::collections::BTreeMap;

/// The whole configuration. `Config::default()` is what the shell uses without a file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Config {
    pub ui: Ui,
    pub editor: Editor,
    pub prompt: Prompt,
    pub history: History,
    pub safety: Safety,
    pub ai: Ai,
    pub update: Update,
}

/// `[ui]`
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ui {
    /// Name of a theme: a built-in one, or `themes/<name>.toml` in the config directory.
    pub theme: String,
    /// `None` means automatic: reduced over `ssh`, full otherwise (§15.2).
    pub motion: Option<Motion>,
    pub icons: Icons,
}

impl Default for Ui {
    fn default() -> Self {
        Self {
            theme: "nord".to_owned(),
            motion: None,
            icons: Icons::Auto,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Motion {
    Full,
    Reduced,
    Off,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icons {
    Unicode,
    Ascii,
    /// Unicode when the locale is UTF-8 and the terminal is not `dumb`.
    Auto,
}

/// `[editor]`
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Editor {
    pub mode: EditMode,
    /// Fish-style suggestions from history.
    pub hints: bool,
    /// `[editor.abbreviations]`: a word typed and followed by a space becomes its text.
    pub abbreviations: BTreeMap<String, String>,
}

impl Default for Editor {
    fn default() -> Self {
        Self {
            mode: EditMode::Emacs,
            hints: true,
            abbreviations: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditMode {
    Emacs,
    Vi,
}

/// `[prompt]`: names of segments, in the order they are drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prompt {
    pub left: Vec<String>,
    pub right: Vec<String>,
    /// Collapse old prompts to a short one.
    pub transient: bool,
}

impl Default for Prompt {
    fn default() -> Self {
        Self {
            left: strings(&["cwd", "git", "venv"]),
            right: strings(&["status", "duration"]),
            transient: false,
        }
    }
}

/// `[history]`
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct History {
    /// How many commands to keep.
    pub size: u32,
    /// Glob patterns of lines that are not stored. Lines that look like they hold a secret
    /// are never stored, whatever this says (§14.2).
    pub ignore: Vec<String>,
}

impl Default for History {
    fn default() -> Self {
        Self {
            size: 100_000,
            ignore: strings(&[" *"]),
        }
    }
}

/// `[safety]`
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Safety {
    /// The lowest risk level that asks for confirmation.
    pub confirm: ConfirmLevel,
    pub trash_retention_days: u32,
    /// Limits of the blast-radius walk (§12.3).
    pub blast_radius_max_entries: u32,
    pub blast_radius_max_ms: u32,
}

impl Default for Safety {
    fn default() -> Self {
        Self {
            confirm: ConfirmLevel::Dangerous,
            trash_retention_days: 8,
            blast_radius_max_entries: 50_000,
            blast_radius_max_ms: 300,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfirmLevel {
    Caution,
    Dangerous,
    Critical,
}

/// `[ai]`. API keys are not here: they live in the keyring (§14.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ai {
    pub enabled: bool,
    /// `None` until the user sets one up: the AI layer stays off (§13.1).
    pub provider: Option<Provider>,
    pub model: Option<String>,
    /// Address of an OpenAI-compatible server (or a non-default Ollama).
    pub base_url: Option<String>,
    pub language: AiLanguage,
    pub context: AiContext,
}

impl Default for Ai {
    fn default() -> Self {
        Self {
            enabled: true,
            provider: None,
            model: None,
            base_url: None,
            language: AiLanguage::Auto,
            context: AiContext::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    Ollama,
    Anthropic,
    OpenAi,
    Gemini,
    OpenAiCompatible,
}

impl Provider {
    /// Every provider, in the order the setup wizard lists them.
    pub const ALL: [Self; 5] = [
        Self::Ollama,
        Self::Anthropic,
        Self::OpenAi,
        Self::Gemini,
        Self::OpenAiCompatible,
    ];

    /// The name used in `config.toml` (`provider = "..."`) and as the name of the key's entry
    /// in the keyring (`oh-my-shell/<name>`, blueprint §9).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ollama => "ollama",
            Self::Anthropic => "anthropic",
            Self::OpenAi => "openai",
            Self::Gemini => "gemini",
            Self::OpenAiCompatible => "openai-compatible",
        }
    }

    /// A name for people: "Anthropic", "OpenAI-compatible server", ...
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Ollama => "Ollama (local)",
            Self::Anthropic => "Anthropic",
            Self::OpenAi => "OpenAI",
            Self::Gemini => "Google Gemini",
            Self::OpenAiCompatible => "OpenAI-compatible server",
        }
    }

    /// Whether this provider needs an API key. Ollama runs locally and has none; a server that
    /// only speaks the OpenAI protocol may or may not ask for one, so it is optional there.
    #[must_use]
    pub const fn key_is_required(self) -> bool {
        matches!(self, Self::Anthropic | Self::OpenAi | Self::Gemini)
    }

    /// Whether the key is used at all (Ollama has none).
    #[must_use]
    pub const fn takes_a_key(self) -> bool {
        !matches!(self, Self::Ollama)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AiLanguage {
    /// Answer in the language of the request.
    Auto,
    English,
    Bangla,
}

/// `[ai.context]`: what may be sent with a request (§13.5). Each item can be turned off.
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "one switch for each item of the context"
)]
pub struct AiContext {
    pub os: bool,
    pub cwd: bool,
    pub git: bool,
    pub recent_commands: bool,
}

impl Default for AiContext {
    fn default() -> Self {
        Self {
            os: true,
            cwd: true,
            git: true,
            recent_commands: true,
        }
    }
}

/// `[update]`
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Update {
    /// Look for a new release at startup. Off by default: a check is network traffic, and
    /// the normal path makes none (Gate C). S29 decides how it works.
    pub check: bool,
    pub channel: Channel,
}

impl Default for Update {
    fn default() -> Self {
        Self {
            check: false,
            channel: Channel::Stable,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Stable,
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_owned()).collect()
}

/// The prompt segments the shell knows (§10).
pub const PROMPT_SEGMENTS: &[&str] = &["cwd", "git", "venv", "status", "duration", "time"];

/// The sections and their keys. The loader reports anything else as unknown, and a test checks
/// that the first-run file mentions every key listed here.
pub const SECTIONS: &[(&str, &[&str])] = &[
    ("ui", &["theme", "motion", "icons"]),
    ("editor", &["mode", "hints", "abbreviations"]),
    ("prompt", &["left", "right", "transient"]),
    ("history", &["size", "ignore"]),
    (
        "safety",
        &[
            "confirm",
            "trash_retention_days",
            "blast_radius_max_entries",
            "blast_radius_max_ms",
        ],
    ),
    (
        "ai",
        &[
            "enabled", "provider", "model", "base_url", "language", "context",
        ],
    ),
    ("ai.context", &["os", "cwd", "git", "recent_commands"]),
    ("update", &["check", "channel"]),
];
