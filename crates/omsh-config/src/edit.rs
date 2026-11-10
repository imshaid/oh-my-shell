//! Writing the `[ai]` keys into `config.toml` after `omsh ai setup`.
//!
//! The file is the user's: it has comments, a layout and maybe a symbolic link into a dotfiles
//! repository. [`set_ai`] changes only the `provider`, `model` and `base_url` keys (and puts
//! `enabled` back to `true` if it was `false`), through `toml_edit`, which keeps everything
//! else as it was. The new text goes to a temporary file next to the real one and is renamed
//! over it, so a reader never sees half a file.
//!
//! API keys never come here (blueprint §14.1).

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, ErrorKind, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use toml_edit::{DocumentMut, Item, TableLike, Value};

use crate::first_run::TEMPLATE;
use crate::load::O_NONBLOCK;
use crate::schema::Provider;

/// The largest file this will edit, the same limit as for reading it.
const MAX_BYTES: u64 = 1024 * 1024;

/// What the wizard chose.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AiChoice {
    pub provider: Provider,
    pub model: String,
    /// `None` removes an old `base_url`: a leftover address of another provider would
    /// receive this provider's key.
    pub base_url: Option<String>,
}

impl AiChoice {
    /// The lines to paste into `config.toml` by hand, for when the file cannot be edited.
    #[must_use]
    pub fn lines(&self) -> String {
        let mut text = String::from("[ai]\n");
        push_pair(&mut text, "provider", self.provider.as_str());
        push_pair(&mut text, "model", &self.model);
        if let Some(url) = &self.base_url {
            push_pair(&mut text, "base_url", url);
        }
        text
    }
}

fn push_pair(text: &mut String, key: &str, value: &str) {
    // A TOML basic string: the same escaping `toml_edit` would use.
    text.push_str(key);
    text.push_str(" = ");
    text.push_str(&Value::from(value).to_string());
    text.push('\n');
}

/// What changed besides the three keys.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Edited {
    /// `[ai] enabled = false` was found and set to `true`.
    pub switched_on: bool,
}

/// Why the file was not edited.
#[derive(Debug)]
pub enum EditError {
    /// The file could not be read or written.
    Io(io::Error),
    /// Bigger than 1 MiB, or not a regular file.
    NotUsable(&'static str),
    /// A syntax error, with where it is and what it is. Editing a broken file could make it
    /// worse, so nothing is written.
    Syntax(String),
    /// `ai` exists but is not a table (`ai = 3`).
    NotATable,
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::NotUsable(why) => f.write_str(why),
            Self::Syntax(why) => write!(f, "the file has a syntax error ({why})"),
            Self::NotATable => f.write_str("`ai` is not a table"),
        }
    }
}

impl std::error::Error for EditError {}

impl From<io::Error> for EditError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Sets `[ai] provider`, `model` and `base_url` in the file, keeping the rest of it.
///
/// A missing file is created from the commented first-run text. A symbolic link is followed:
/// the file it points to is the one that changes.
///
/// # Errors
///
/// [`EditError`] when the file cannot be read or written, is not usable, or has a syntax
/// error. Nothing is written then.
pub fn set_ai(path: &Path, choice: &AiChoice) -> Result<Edited, EditError> {
    let target = fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    let (text, mode) = read(&target)?;
    let mut document: DocumentMut = text.parse().map_err(|error: toml_edit::TomlError| {
        let at = error
            .span()
            .map(|span| position(&text, span.start))
            .unwrap_or_default();
        EditError::Syntax(format!("{at}{}", error.message()))
    })?;

    // No `[ai]` yet: add the block at the end of the text, byte for byte after the old text.
    // (`toml_edit` keeps a comment-only file's comments at the end of the document, so
    // adding the table through it would put `[ai]` above all of them.)
    let Some(item) = document.get_mut("ai") else {
        let mut appended = text.clone();
        if !appended.is_empty() && !appended.ends_with('\n') {
            appended.push('\n');
        }
        if !appended.is_empty() && !appended.ends_with("\n\n") {
            appended.push('\n');
        }
        appended.push_str(&choice.lines());
        // The result must be the document we mean; a surprise here stops before writing.
        appended
            .parse::<DocumentMut>()
            .map_err(|error| EditError::Syntax(error.message().to_owned()))?;
        write(&target, &appended, mode)?;
        return Ok(Edited::default());
    };

    if let Item::Table(table) = item {
        // A table that only held `[ai.context]` is "implicit" and would print no header.
        table.set_implicit(false);
    }
    let table = item.as_table_like_mut().ok_or(EditError::NotATable)?;
    set_string(table, "provider", choice.provider.as_str());
    set_string(table, "model", &choice.model);
    match &choice.base_url {
        Some(url) => set_string(table, "base_url", url),
        None => {
            table.remove("base_url");
        }
    }
    let switched_on = matches!(table.get("enabled").and_then(Item::as_bool), Some(false));
    if switched_on {
        set_item(table, "enabled", Value::from(true));
    }

    write(&target, &document.to_string(), mode)?;
    Ok(Edited { switched_on })
}

/// "line 3, column 5: " for the byte offset `at`, or nothing.
fn position(text: &str, at: usize) -> String {
    let before = text.get(..at).unwrap_or_default();
    let line = before.matches('\n').count() + 1;
    let column = before
        .rsplit('\n')
        .next()
        .unwrap_or_default()
        .chars()
        .count()
        + 1;
    format!("line {line}, column {column}: ")
}

fn set_string(table: &mut dyn TableLike, key: &str, text: &str) {
    set_item(table, key, Value::from(text));
}

/// Gives `key` the new value. An existing key is changed in place, so the comments around it
/// (above the key, and after the value on the same line) stay.
fn set_item(table: &mut dyn TableLike, key: &str, mut value: Value) {
    if let Some(old) = table.get_mut(key).and_then(Item::as_value_mut) {
        *value.decor_mut() = old.decor().clone();
        *old = value;
    } else {
        table.insert(key, Item::Value(value));
    }
}

/// The file's text and permission bits; the commented first-run text if there is no file.
fn read(path: &Path) -> Result<(String, u32), EditError> {
    let mut file = match OpenOptions::new()
        .read(true)
        // A FIFO at this path must not hang the shell.
        .custom_flags(O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Ok((TEMPLATE.to_owned(), 0o644));
        }
        Err(error) => return Err(error.into()),
    };
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(EditError::NotUsable("not a regular file"));
    }
    if meta.len() > MAX_BYTES {
        return Err(EditError::NotUsable("the file is bigger than 1 MiB"));
    }
    let mut text = String::new();
    file.read_to_string(&mut text)
        .map_err(|error| match error.kind() {
            ErrorKind::InvalidData => EditError::NotUsable("the file is not valid UTF-8"),
            _ => EditError::Io(error),
        })?;
    Ok((text, meta.permissions().mode() & 0o7777))
}

/// Writes `text` to a temporary file beside `path` (same mode as the old file) and renames it
/// over `path`.
fn write(path: &Path, text: &str, mode: u32) -> Result<(), EditError> {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = path
        .parent()
        .ok_or(EditError::NotUsable("the file has no directory"))?;
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temporary: PathBuf = dir.join(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let written = (|| {
        let mut out = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&temporary)?;
        // The umask may have taken bits away; the old file's mode is kept exactly.
        out.set_permissions(fs::Permissions::from_mode(mode))?;
        out.write_all(text.as_bytes())?;
        out.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    written.map_err(EditError::Io)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;
    use crate::{Config, parse};

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!(
                "omsh-edit-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn file(&self, text: &str) -> PathBuf {
            let path = self.0.join("config.toml");
            fs::write(&path, text).unwrap();
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn choice(provider: Provider, model: &str, base_url: Option<&str>) -> AiChoice {
        AiChoice {
            provider,
            model: model.to_owned(),
            base_url: base_url.map(str::to_owned),
        }
    }

    #[test]
    fn a_new_ai_table_is_added_and_the_rest_is_untouched() {
        let dir = Scratch::new();
        let original = "# my notes\n[ui]\nicons = \"ascii\" # plain\n\n[history]\nsize = 5\n";
        let path = dir.file(original);
        set_ai(&path, &choice(Provider::Anthropic, "claude-x", None)).unwrap();
        let after = fs::read_to_string(&path).unwrap();
        assert!(after.starts_with(original), "{after}");
        assert!(
            after.contains("[ai]\nprovider = \"anthropic\"\nmodel = \"claude-x\"\n"),
            "{after}"
        );
        let parsed = parse(&after);
        assert!(parsed.problems.is_empty(), "{:?}", parsed.problems);
        assert_eq!(parsed.config.ai.provider, Some(Provider::Anthropic));
        assert_eq!(parsed.config.ai.model.as_deref(), Some("claude-x"));
        assert_eq!(parsed.config.history.size, 5);
    }

    #[test]
    fn existing_keys_are_replaced_and_their_comments_stay() {
        let dir = Scratch::new();
        let path = dir.file(
            "[ai]\n# which one\nprovider = \"ollama\" # local\nmodel = \"old\"\nlanguage = \"bn\"\n",
        );
        set_ai(&path, &choice(Provider::OpenAi, "gpt-x", None)).unwrap();
        let after = fs::read_to_string(&path).unwrap();
        assert!(after.contains("# which one\n"), "{after}");
        assert!(after.contains("provider = \"openai\" # local\n"), "{after}");
        assert!(after.contains("model = \"gpt-x\"\n"), "{after}");
        assert!(after.contains("language = \"bn\"\n"), "{after}");
    }

    #[test]
    fn an_old_base_url_is_removed_when_the_new_choice_has_none() {
        let dir = Scratch::new();
        let path = dir.file(
            "[ai]\nprovider = \"openai-compatible\"\nmodel = \"m\"\nbase_url = \"http://localhost:8080/v1\"\n",
        );
        set_ai(&path, &choice(Provider::Anthropic, "claude-x", None)).unwrap();
        let after = fs::read_to_string(&path).unwrap();
        assert!(!after.contains("base_url"), "{after}");
        assert_eq!(parse(&after).config.ai.base_url, None);
    }

    #[test]
    fn a_base_url_is_written_when_given() {
        let dir = Scratch::new();
        let path = dir.file("");
        set_ai(
            &path,
            &choice(
                Provider::OpenAiCompatible,
                "m",
                Some("http://localhost:8080/v1"),
            ),
        )
        .unwrap();
        let config = parse(&fs::read_to_string(&path).unwrap()).config;
        assert_eq!(
            config.ai.base_url.as_deref(),
            Some("http://localhost:8080/v1")
        );
        assert_eq!(config.ai.provider, Some(Provider::OpenAiCompatible));
    }

    #[test]
    fn a_table_that_held_only_a_subtable_gets_its_header() {
        let dir = Scratch::new();
        let path = dir.file("[ai.context]\ngit = false\n");
        set_ai(&path, &choice(Provider::Gemini, "g", None)).unwrap();
        let config = parse(&fs::read_to_string(&path).unwrap()).config;
        assert_eq!(config.ai.provider, Some(Provider::Gemini));
        assert!(!config.ai.context.git, "the subtable is kept");
    }

    #[test]
    fn a_disabled_ai_is_switched_on_and_it_is_reported() {
        let dir = Scratch::new();
        let path = dir.file("[ai]\nenabled = false # off for now\n");
        let edited = set_ai(&path, &choice(Provider::Ollama, "llama", None)).unwrap();
        assert!(edited.switched_on);
        let after = fs::read_to_string(&path).unwrap();
        assert!(after.contains("enabled = true # off for now"), "{after}");
        let enabled_already = dir.file("[ai]\nenabled = true\n");
        assert!(
            !set_ai(&enabled_already, &choice(Provider::Ollama, "l", None))
                .unwrap()
                .switched_on
        );
    }

    #[test]
    fn a_broken_file_is_not_touched() {
        let dir = Scratch::new();
        let broken = "[ui\nicons = 1\n";
        let path = dir.file(broken);
        let error = set_ai(&path, &choice(Provider::Ollama, "l", None)).unwrap_err();
        assert!(matches!(error, EditError::Syntax(_)), "{error}");
        assert!(error.to_string().contains("line 1"), "{error}");
        assert_eq!(fs::read_to_string(&path).unwrap(), broken);
        // And no temporary file is left behind.
        assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
    }

    #[test]
    fn ai_that_is_not_a_table_is_refused() {
        let dir = Scratch::new();
        let path = dir.file("ai = 3\n");
        assert!(matches!(
            set_ai(&path, &choice(Provider::Ollama, "l", None)),
            Err(EditError::NotATable)
        ));
        assert_eq!(fs::read_to_string(&path).unwrap(), "ai = 3\n");
    }

    #[test]
    fn the_mode_of_the_file_is_kept() {
        let dir = Scratch::new();
        let path = dir.file("");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        set_ai(&path, &choice(Provider::Ollama, "l", None)).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn a_symbolic_link_stays_a_link_and_its_target_changes() {
        let dir = Scratch::new();
        let real = dir.0.join("dotfiles-config.toml");
        fs::write(&real, "# mine\n").unwrap();
        let link = dir.0.join("link.toml");
        symlink(&real, &link).unwrap();
        set_ai(&link, &choice(Provider::Ollama, "l", None)).unwrap();
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let after = fs::read_to_string(&real).unwrap();
        assert!(after.starts_with("# mine\n") && after.contains("provider = \"ollama\""));
    }

    #[test]
    fn a_missing_file_is_created_from_the_first_run_text() {
        let dir = Scratch::new();
        let path = dir.0.join("config.toml");
        set_ai(&path, &choice(Provider::Ollama, "llama", None)).unwrap();
        let after = fs::read_to_string(&path).unwrap();
        assert!(after.starts_with(TEMPLATE), "keeps the comments");
        let parsed = parse(&after);
        assert!(parsed.problems.is_empty(), "{:?}", parsed.problems);
        assert_eq!(parsed.config.ai.model.as_deref(), Some("llama"));
        // Everything else is still the default.
        let mut expected = Config::default();
        expected.ai.provider = Some(Provider::Ollama);
        expected.ai.model = Some("llama".to_owned());
        assert_eq!(parsed.config, expected);
    }

    #[test]
    fn a_fifo_in_place_of_the_file_does_not_hang() {
        let dir = Scratch::new();
        let path = dir.0.join("config.toml");
        let status = std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .unwrap();
        assert!(status.success());
        let error = set_ai(&path, &choice(Provider::Ollama, "l", None)).unwrap_err();
        assert!(matches!(error, EditError::NotUsable(_)), "{error}");
    }

    #[test]
    fn the_lines_to_paste_parse_back_to_the_choice() {
        let c = choice(
            Provider::OpenAiCompatible,
            "name with \"quotes\"",
            Some("https://example.org/v1"),
        );
        let config = parse(&c.lines()).config;
        assert_eq!(config.ai.provider, Some(c.provider));
        assert_eq!(config.ai.model.as_deref(), Some(c.model.as_str()));
        assert_eq!(config.ai.base_url, c.base_url);
    }
}
