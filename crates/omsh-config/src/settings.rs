//! The list of settings for the settings view (S27b): every key of `config.toml`, what kind of
//! value it takes, whether the view may change it, and what it is now.
//!
//! The view shows three things for a key: the value this shell runs with (read at start), the
//! value the file has now, and where the file's value comes from. A change made in the view is
//! written to the file (see [`crate::set_setting`]) and applies when a new shell starts, so the
//! two can differ; the row says so ([`Row::pending`]).

use toml_edit::{DocumentMut, Item};

use crate::edit::Change;
use crate::schema::{Config, ConfirmLevel, EditMode, Icons, Motion};

/// What a key holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Bool,
    /// One of a fixed list of words (the words the view cycles through).
    Choice(&'static [&'static str]),
    Number,
    List,
    Text,
    /// A table of its own (`[editor.abbreviations]`).
    Table,
}

/// One key of `config.toml`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Setting {
    /// `section.key`, for example `editor.hints` or `ai.context.git`.
    pub key: &'static str,
    pub kind: Kind,
    /// Whether the settings view may change it.
    pub editable: bool,
    pub description: &'static str,
}

impl Setting {
    /// How to change a setting the view does not change.
    #[must_use]
    pub fn elsewhere(&self) -> &'static str {
        if matches!(self.key, "ai.provider" | "ai.model" | "ai.base_url") {
            "Change it with `omsh ai setup`."
        } else if matches!(self.key, "history.size") {
            "Edit config.toml; it is applied when a shell starts."
        } else {
            "Edit config.toml to change it."
        }
    }
}

const MOTION: &[&str] = &["automatic", "full", "reduced", "off"];
const ICONS: &[&str] = &["auto", "unicode", "ascii"];
const MODE: &[&str] = &["emacs", "vi"];
const CONFIRM: &[&str] = &["caution", "dangerous", "critical"];

const fn setting(
    key: &'static str,
    kind: Kind,
    editable: bool,
    description: &'static str,
) -> Setting {
    Setting {
        key,
        kind,
        editable,
        description,
    }
}

/// Every setting, in the order of the file.
pub const SETTINGS: &[Setting] = &[
    setting(
        "ui.theme",
        Kind::Text,
        false,
        "The colors: \"nord\" or themes/<name>.toml.",
    ),
    setting(
        "ui.motion",
        Kind::Choice(MOTION),
        true,
        "How much the waiting line moves. Automatic: reduced over ssh.",
    ),
    setting(
        "ui.icons",
        Kind::Choice(ICONS),
        true,
        "Unicode or ASCII symbols. Auto follows the locale.",
    ),
    setting(
        "editor.mode",
        Kind::Choice(MODE),
        true,
        "Key bindings of the command line.",
    ),
    setting(
        "editor.hints",
        Kind::Bool,
        true,
        "Gray suggestions from history as you type.",
    ),
    setting(
        "editor.abbreviations",
        Kind::Table,
        false,
        "Words that expand when followed by a space.",
    ),
    setting(
        "prompt.left",
        Kind::List,
        false,
        "Segments on the left of the prompt.",
    ),
    setting(
        "prompt.right",
        Kind::List,
        false,
        "Segments on the right of the prompt.",
    ),
    setting(
        "prompt.transient",
        Kind::Bool,
        true,
        "Collapse old prompts to a short one.",
    ),
    setting(
        "history.size",
        Kind::Number,
        false,
        "How many commands to keep.",
    ),
    setting(
        "history.ignore",
        Kind::List,
        false,
        "Patterns of lines that are not stored.",
    ),
    setting(
        "safety.confirm",
        Kind::Choice(CONFIRM),
        false,
        "The lowest risk level that asks first.",
    ),
    setting(
        "safety.trash_retention_days",
        Kind::Number,
        false,
        "Days the trash keeps deleted files.",
    ),
    setting(
        "safety.blast_radius_max_entries",
        Kind::Number,
        false,
        "Most files counted for a warning.",
    ),
    setting(
        "safety.blast_radius_max_ms",
        Kind::Number,
        false,
        "Most milliseconds spent counting.",
    ),
    setting(
        "ai.enabled",
        Kind::Bool,
        true,
        "The `?` features. Off: nothing is ever sent.",
    ),
    setting("ai.provider", Kind::Text, false, "Which service answers."),
    setting("ai.model", Kind::Text, false, "Which model answers."),
    setting(
        "ai.base_url",
        Kind::Text,
        false,
        "Address of a local or compatible server.",
    ),
    setting(
        "ai.context.os",
        Kind::Bool,
        true,
        "Send the operating system with a request.",
    ),
    setting(
        "ai.context.cwd",
        Kind::Bool,
        true,
        "Send the current folder with a request.",
    ),
    setting(
        "ai.context.git",
        Kind::Bool,
        true,
        "Send the git branch with a request.",
    ),
    setting(
        "ai.context.recent_commands",
        Kind::Bool,
        true,
        "Send your last commands with a request.",
    ),
    setting(
        "notify.enabled",
        Kind::Bool,
        true,
        "A desktop notification when a long command ends.",
    ),
    setting(
        "notify.min_seconds",
        Kind::Number,
        false,
        "Seconds a command runs before it notifies.",
    ),
    setting(
        "notify.ignore",
        Kind::List,
        false,
        "Programs that never notify.",
    ),
    setting(
        "update.check",
        Kind::Bool,
        false,
        "Look for a new release at start (not built yet).",
    ),
    setting(
        "update.channel",
        Kind::Text,
        false,
        "Which releases to follow.",
    ),
];

/// The setting called `key`.
#[must_use]
pub fn find(key: &str) -> Option<&'static Setting> {
    SETTINGS.iter().find(|setting| setting.key == key)
}

/// A row of the settings view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub setting: &'static Setting,
    /// The value this shell runs with.
    pub running: String,
    /// The value `config.toml` gives now (the default if the file does not set it).
    pub saved: String,
    /// The file sets the key.
    pub in_file: bool,
}

impl Row {
    /// The file and the running shell disagree: the change applies at the next start.
    #[must_use]
    pub fn pending(&self) -> bool {
        self.running != self.saved
    }
}

/// The rows: what `running` holds, against the text of the file (`None`: there is no file).
#[must_use]
pub fn rows(running: &Config, file: Option<&str>) -> Vec<Row> {
    let saved = file.map_or_else(Config::default, |text| crate::parse(text).config);
    let document = file.and_then(|text| text.parse::<DocumentMut>().ok());
    SETTINGS
        .iter()
        .map(|setting| Row {
            setting,
            running: display(running, setting.key),
            saved: display(&saved, setting.key),
            in_file: document
                .as_ref()
                .is_some_and(|document| has_key(document, setting.key)),
        })
        .collect()
}

fn has_key(document: &DocumentMut, key: &str) -> bool {
    let mut item: &Item = document.as_item();
    for part in key.split('.') {
        match item.as_table_like().and_then(|table| table.get(part)) {
            Some(next) => item = next,
            None => return false,
        }
    }
    true
}

/// What the view writes for the next value of an editable key after `current` (the value the
/// file has now). `None` when the key is not one the view changes.
#[must_use]
pub fn cycle(key: &str, current: &str, forward: bool) -> Option<Change> {
    let setting = find(key).filter(|setting| setting.editable)?;
    match setting.kind {
        Kind::Bool => Some(Change::Bool(current != "true")),
        Kind::Choice(words) => {
            let at = words.iter().position(|word| *word == current).unwrap_or(0);
            let next = if forward {
                (at + 1) % words.len()
            } else {
                (at + words.len() - 1) % words.len()
            };
            let word = words[next];
            // "automatic" is not a value the file takes: it is the key being absent.
            Some(if key == "ui.motion" && word == "automatic" {
                Change::Remove
            } else {
                Change::Text(word.to_owned())
            })
        }
        _ => None,
    }
}

/// The value of `key` in `config`, as the view shows it.
#[must_use]
pub fn display(config: &Config, key: &str) -> String {
    let list = |items: &[String]| {
        if items.is_empty() {
            "(none)".to_owned()
        } else {
            items.join(", ")
        }
    };
    let optional = |value: Option<&str>| value.unwrap_or("(not set)").to_owned();
    match key {
        "ui.theme" => config.ui.theme.clone(),
        "ui.motion" => match config.ui.motion {
            None => "automatic",
            Some(Motion::Full) => "full",
            Some(Motion::Reduced) => "reduced",
            Some(Motion::Off) => "off",
        }
        .to_owned(),
        "ui.icons" => match config.ui.icons {
            Icons::Auto => "auto",
            Icons::Unicode => "unicode",
            Icons::Ascii => "ascii",
        }
        .to_owned(),
        "editor.mode" => match config.editor.mode {
            EditMode::Emacs => "emacs",
            EditMode::Vi => "vi",
        }
        .to_owned(),
        "editor.hints" => config.editor.hints.to_string(),
        "editor.abbreviations" => match config.editor.abbreviations.len() {
            0 => "(none)".to_owned(),
            1 => "1 defined".to_owned(),
            n => format!("{n} defined"),
        },
        "prompt.left" => list(&config.prompt.left),
        "prompt.right" => list(&config.prompt.right),
        "prompt.transient" => config.prompt.transient.to_string(),
        "history.size" => config.history.size.to_string(),
        "history.ignore" => list(&config.history.ignore),
        "safety.confirm" => match config.safety.confirm {
            ConfirmLevel::Caution => "caution",
            ConfirmLevel::Dangerous => "dangerous",
            ConfirmLevel::Critical => "critical",
        }
        .to_owned(),
        "safety.trash_retention_days" => config.safety.trash_retention_days.to_string(),
        "safety.blast_radius_max_entries" => config.safety.blast_radius_max_entries.to_string(),
        "safety.blast_radius_max_ms" => config.safety.blast_radius_max_ms.to_string(),
        "ai.enabled" => config.ai.enabled.to_string(),
        "ai.provider" => optional(config.ai.provider.map(crate::Provider::as_str)),
        "ai.model" => optional(config.ai.model.as_deref()),
        // The address may hold a password; the view never shows it.
        "ai.base_url" => if config.ai.base_url.is_some() {
            "(set)"
        } else {
            "(not set)"
        }
        .to_owned(),
        "ai.context.os" => config.ai.context.os.to_string(),
        "ai.context.cwd" => config.ai.context.cwd.to_string(),
        "ai.context.git" => config.ai.context.git.to_string(),
        "ai.context.recent_commands" => config.ai.context.recent_commands.to_string(),
        "notify.enabled" => config.notify.enabled.to_string(),
        "notify.min_seconds" => config.notify.min_seconds.to_string(),
        "notify.ignore" => list(&config.notify.ignore),
        "update.check" => config.update.check.to_string(),
        "update.channel" => "stable".to_owned(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::SECTIONS;

    #[test]
    fn every_key_of_the_schema_has_a_row() {
        for (section, keys) in SECTIONS {
            for key in *keys {
                let path = format!("{section}.{key}");
                // `ai.context` is a table whose keys have rows of their own.
                if SECTIONS.iter().any(|(name, _)| *name == path) {
                    continue;
                }
                assert!(find(&path).is_some(), "no row for {path}");
            }
        }
        for setting in SETTINGS {
            let (section, key) = setting.key.rsplit_once('.').unwrap();
            let listed = SECTIONS
                .iter()
                .any(|(name, keys)| *name == section && keys.contains(&key));
            assert!(listed, "{} is not in SECTIONS", setting.key);
        }
    }

    #[test]
    fn exactly_the_switches_and_choices_the_owner_named_are_editable() {
        let mut editable: Vec<&str> = SETTINGS
            .iter()
            .filter(|setting| setting.editable)
            .map(|setting| setting.key)
            .collect();
        editable.sort_unstable();
        assert_eq!(
            editable,
            [
                "ai.context.cwd",
                "ai.context.git",
                "ai.context.os",
                "ai.context.recent_commands",
                "ai.enabled",
                "editor.hints",
                "editor.mode",
                "notify.enabled",
                "prompt.transient",
                "ui.icons",
                "ui.motion",
            ]
        );
        // Only booleans and fixed choices.
        assert!(
            SETTINGS
                .iter()
                .filter(|setting| setting.editable)
                .all(|setting| matches!(setting.kind, Kind::Bool | Kind::Choice(_)))
        );
    }

    #[test]
    fn every_editable_choice_is_a_value_the_loader_accepts() {
        for setting in SETTINGS.iter().filter(|setting| setting.editable) {
            let Kind::Choice(words) = setting.kind else {
                continue;
            };
            for word in words {
                let (section, key) = setting.key.rsplit_once('.').unwrap();
                let text = if *word == "automatic" {
                    String::new()
                } else {
                    format!("[{section}]\n{key} = \"{word}\"\n")
                };
                let parsed = crate::parse(&text);
                assert!(parsed.problems.is_empty(), "{word} for {}", setting.key);
                assert_eq!(display(&parsed.config, setting.key), *word);
            }
        }
    }

    #[test]
    fn a_default_config_with_no_file_has_nothing_pending() {
        let rows = rows(&Config::default(), None);
        assert_eq!(rows.len(), SETTINGS.len());
        assert!(rows.iter().all(|row| !row.pending() && !row.in_file));
    }

    #[test]
    fn a_saved_change_that_the_shell_does_not_run_yet_is_pending() {
        let running = Config::default();
        let rows = rows(&running, Some("[editor]\nhints = false\n# kept\n"));
        let hints = rows
            .iter()
            .find(|r| r.setting.key == "editor.hints")
            .unwrap();
        assert_eq!(
            (hints.running.as_str(), hints.saved.as_str()),
            ("true", "false")
        );
        assert!(hints.pending() && hints.in_file);
        let mode = rows
            .iter()
            .find(|r| r.setting.key == "editor.mode")
            .unwrap();
        assert!(!mode.pending() && !mode.in_file);
        assert_eq!(rows.iter().filter(|r| r.pending()).count(), 1);
    }

    #[test]
    fn a_key_the_file_sets_to_its_default_is_in_the_file_but_not_pending() {
        let rows = rows(&Config::default(), Some("[editor]\nmode = \"emacs\"\n"));
        let mode = rows
            .iter()
            .find(|r| r.setting.key == "editor.mode")
            .unwrap();
        assert!(mode.in_file && !mode.pending());
    }

    #[test]
    fn a_wrong_value_in_the_file_shows_the_default_it_falls_back_to() {
        let rows = rows(&Config::default(), Some("[editor]\nmode = \"nano\"\n"));
        let mode = rows
            .iter()
            .find(|r| r.setting.key == "editor.mode")
            .unwrap();
        assert_eq!(mode.saved, "emacs");
    }

    #[test]
    fn nested_keys_are_found_in_the_file() {
        let rows = rows(&Config::default(), Some("[ai.context]\ngit = false\n"));
        let git = rows
            .iter()
            .find(|r| r.setting.key == "ai.context.git")
            .unwrap();
        assert!(git.in_file && git.saved == "false");
        let os = rows
            .iter()
            .find(|r| r.setting.key == "ai.context.os")
            .unwrap();
        assert!(!os.in_file);
    }

    #[test]
    fn the_address_of_a_server_is_never_shown() {
        let mut config = Config::default();
        config.ai.base_url = Some("http://user:secret@host".to_owned());
        assert_eq!(display(&config, "ai.base_url"), "(set)");
    }

    #[test]
    fn cycling_a_switch_flips_it() {
        assert_eq!(
            cycle("editor.hints", "true", true),
            Some(Change::Bool(false))
        );
        assert_eq!(
            cycle("editor.hints", "false", false),
            Some(Change::Bool(true))
        );
    }

    #[test]
    fn cycling_a_choice_wraps_both_ways() {
        let text = |word: &str| Some(Change::Text(word.to_owned()));
        assert_eq!(cycle("editor.mode", "emacs", true), text("vi"));
        assert_eq!(cycle("editor.mode", "vi", true), text("emacs"));
        assert_eq!(cycle("editor.mode", "emacs", false), text("vi"));
        assert_eq!(cycle("ui.icons", "auto", true), text("unicode"));
        assert_eq!(cycle("ui.icons", "auto", false), text("ascii"));
    }

    #[test]
    fn automatic_motion_removes_the_key() {
        assert_eq!(cycle("ui.motion", "off", true), Some(Change::Remove));
        assert_eq!(
            cycle("ui.motion", "automatic", true),
            Some(Change::Text("full".to_owned()))
        );
        assert_eq!(
            cycle("ui.motion", "automatic", false),
            Some(Change::Text("off".to_owned()))
        );
    }

    #[test]
    fn keys_the_view_does_not_change_are_refused() {
        assert_eq!(cycle("safety.confirm", "dangerous", true), None);
        assert_eq!(cycle("history.size", "5", true), None);
        assert_eq!(cycle("ui.theme", "nord", true), None);
        assert_eq!(cycle("no.such", "x", true), None);
    }
}
