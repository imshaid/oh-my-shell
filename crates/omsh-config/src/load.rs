//! Reading `config.toml`: parsing, checking every setting on its own, and reporting what is
//! wrong with line and column.
//!
//! The rule of blueprint §9 is that a broken file never stops the shell. So this does not
//! deserialize the file into the typed `Config` in one go (one bad value would fail the whole
//! thing). It parses the file into a table whose every key and value knows where it came from,
//! and then looks at the settings one by one: a setting that is wrong keeps its default and
//! produces a [`Problem`]; the others apply.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{ErrorKind, Read};
use std::ops::{Range, RangeInclusive};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use toml::Spanned;
use toml::de::{DeTable, DeValue};

use crate::first_run;
use crate::paths::Paths;
use crate::schema::{
    Ai, AiLanguage, Channel, Config, ConfirmLevel, EditMode, Icons, Motion, PROMPT_SEGMENTS,
    Provider, SECTIONS, Ui,
};

/// A file larger than this is not a configuration file.
const MAX_BYTES: u64 = 1024 * 1024;

/// `O_NONBLOCK` on Linux. Opening a FIFO for reading with it returns at once, so that a
/// `config.toml` that is a FIFO is refused instead of hanging the shell at startup.
const O_NONBLOCK: i32 = 0o4000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    /// A setting is wrong and its default is used.
    Error,
    /// Something is odd but nothing was lost (an unknown setting).
    Warning,
}

/// One thing that is wrong with the file. Line and column start at 1; the column counts
/// characters, not bytes. Both are missing for a problem that is not about a place in the
/// text (the file cannot be read).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub severity: Severity,
    pub line: Option<usize>,
    pub column: Option<usize>,
    pub message: String,
}

impl Problem {
    /// `4:10: message`, or `message` when there is no place. Warnings say so.
    #[must_use]
    pub fn describe(&self) -> String {
        let label = match self.severity {
            Severity::Error => "",
            Severity::Warning => "warning: ",
        };
        match (self.line, self.column) {
            (Some(line), Some(column)) => format!("{line}:{column}: {label}{}", self.message),
            _ => format!("{label}{}", self.message),
        }
    }

    fn without_place(severity: Severity, message: String) -> Self {
        Self {
            severity,
            line: None,
            column: None,
            message,
        }
    }
}

/// The result of reading a text: the configuration, with defaults for everything that was
/// wrong, and what was wrong.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parsed {
    pub config: Config,
    pub problems: Vec<Problem>,
}

/// The result of loading the configuration file of the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Loaded {
    pub config: Config,
    pub problems: Vec<Problem>,
    /// The file that was read, if the paths allowed one.
    pub file: Option<PathBuf>,
    /// The file did not exist and the commented first-run file was written.
    pub created: bool,
}

impl Loaded {
    /// The lines to show the user, at most `limit` of them (then one line says how many more).
    /// Each starts with the file name and, if there is one, the line and column.
    #[must_use]
    pub fn report(&self, limit: usize) -> Vec<String> {
        let place = self
            .file
            .as_ref()
            .map_or_else(|| "config".to_owned(), |file| file.display().to_string());
        let mut lines: Vec<String> = self
            .problems
            .iter()
            .take(limit)
            .map(|problem| {
                // A problem with a place reads `file:line:column: message`.
                let described = problem.describe();
                if problem.line.is_some() {
                    format!("{place}:{described}")
                } else {
                    format!("{place}: {described}")
                }
            })
            .collect();
        if self.problems.len() > limit {
            lines.push(format!(
                "{place}: ... and {} more problems",
                self.problems.len() - limit
            ));
        }
        lines
    }
}

/// Reads the user's `config.toml`. A missing file is created from the commented first-run text
/// (the settings are then the defaults). Nothing here can fail: whatever goes wrong becomes a
/// [`Problem`] and the defaults stay.
#[must_use]
pub fn load(paths: &Paths) -> Loaded {
    let Some(file) = paths.config_file() else {
        return Loaded {
            config: Config::default(),
            problems: vec![Problem::without_place(
                Severity::Warning,
                "no home directory is known (HOME is not set), so no configuration file is \
                 read; using the default settings"
                    .to_owned(),
            )],
            file: None,
            created: false,
        };
    };
    let mut loaded = Loaded {
        config: Config::default(),
        problems: Vec::new(),
        file: Some(file.clone()),
        created: false,
    };
    match read_file(&file) {
        Ok(Text::Found(text)) => {
            let parsed = parse(&text);
            loaded.config = parsed.config;
            loaded.problems = parsed.problems;
        }
        Ok(Text::Missing) => match first_run::create(&file, first_run::TEMPLATE) {
            Ok(created) => loaded.created = created,
            Err(error) => loaded.problems.push(Problem::without_place(
                Severity::Warning,
                format!(
                    "the first-run file could not be written ({error}); \
                     create the file yourself to stop this message"
                ),
            )),
        },
        Err(problem) => loaded.problems.push(problem),
    }
    loaded
}

enum Text {
    Found(String),
    Missing,
}

/// Reads the file without ever blocking or reading without a limit.
fn read_file(file: &Path) -> Result<Text, Problem> {
    let fail = |what: String| Problem::without_place(Severity::Error, what);
    let opened = OpenOptions::new()
        .read(true)
        .custom_flags(O_NONBLOCK)
        .open(file);
    let mut opened = match opened {
        Ok(opened) => opened,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Text::Missing),
        Err(error) => return Err(fail(format!("cannot read the file: {error}"))),
    };
    let meta = opened
        .metadata()
        .map_err(|error| fail(format!("cannot read the file: {error}")))?;
    if !meta.is_file() {
        return Err(fail(
            "this is not a regular file; using the default settings".to_owned(),
        ));
    }
    if meta.len() > MAX_BYTES {
        return Err(fail(format!(
            "the file is larger than {} KiB; using the default settings",
            MAX_BYTES / 1024
        )));
    }
    let mut bytes = Vec::new();
    opened
        .by_ref()
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| fail(format!("cannot read the file: {error}")))?;
    match String::from_utf8(bytes) {
        Ok(text) => Ok(Text::Found(text)),
        Err(error) => {
            let valid = error.utf8_error().valid_up_to();
            let text = String::from_utf8_lossy(&error.as_bytes()[..valid]).into_owned();
            let (line, column) = position(&text, valid);
            Err(Problem {
                severity: Severity::Error,
                line: Some(line),
                column: Some(column),
                message: "the file is not valid UTF-8; using the default settings".to_owned(),
            })
        }
    }
}

/// Reads a configuration from text. Every setting that is wrong keeps its default.
#[must_use]
pub fn parse(text: &str) -> Parsed {
    let (root, syntax_errors) = DeTable::parse_recoverable(text);
    let mut reader = Reader {
        text,
        problems: Vec::new(),
        damaged: Vec::new(),
        first_error: None,
    };
    for error in &syntax_errors {
        let span = error.span().unwrap_or(0..0);
        reader.damaged.push(span.clone());
        reader.first_error = Some(
            reader
                .first_error
                .map_or(span.start, |at| at.min(span.start)),
        );
        reader.report(Severity::Error, span.start, error.message().to_owned());
    }
    let config = read_config(&mut reader, root.get_ref());
    let mut problems = reader.problems;
    // The table is walked in key order; the user reads the file from the top.
    problems.sort_by_key(|problem| (problem.line, problem.column));
    Parsed { config, problems }
}

/// Line and column (from 1) of a byte offset. The column counts characters.
fn position(text: &str, offset: usize) -> (usize, usize) {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    let before = &text[..offset];
    let line = before.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let line_start = before.rfind('\n').map_or(0, |index| index + 1);
    (line, before[line_start..].chars().count() + 1)
}

/// What is needed to look at a parsed table and report on it.
struct Reader<'t> {
    text: &'t str,
    problems: Vec<Problem>,
    /// Where the parser found a syntax error. A value there is not trusted and not reported
    /// a second time.
    damaged: Vec<Range<usize>>,
    /// Where the first syntax error starts. After a broken line the parser guesses what the
    /// rest means (an unclosed `[editor` header makes `mode` look like a section), so keys
    /// found after it are not reported as unknown: that would be noise made by the error.
    first_error: Option<usize>,
}

type Value<'a, 'i> = &'a Spanned<DeValue<'i>>;

impl Reader<'_> {
    fn report(&mut self, severity: Severity, offset: usize, message: String) {
        let (line, column) = position(self.text, offset);
        self.problems.push(Problem {
            severity,
            line: Some(line),
            column: Some(column),
            message,
        });
    }

    fn error(&mut self, span: &Range<usize>, message: String) {
        self.report(Severity::Error, span.start, message);
    }

    fn is_damaged(&self, span: &Range<usize>) -> bool {
        self.damaged
            .iter()
            .any(|bad| span.start <= bad.end && bad.start <= span.end)
    }

    /// The value of `key`, unless it is missing or sits in a damaged place.
    fn entry<'a, 'i>(&self, table: &'a DeTable<'i>, key: &str) -> Option<Value<'a, 'i>> {
        let value = table.get(key)?;
        (!self.is_damaged(&value.span())).then_some(value)
    }

    fn wrong_type(&mut self, path: &str, expected: &str, value: Value<'_, '_>) {
        self.error(
            &value.span(),
            format!(
                "{path} must be {expected}, not {}; using the default",
                describe(value.get_ref())
            ),
        );
    }

    /// Reports every key of `table` that is not in `known`.
    fn unknown(&mut self, table: &DeTable<'_>, section: &str) {
        let known = known_keys(section);
        for (key, value) in table {
            let name: &str = key.get_ref();
            let after_error = self.first_error.is_some_and(|at| key.span().start >= at);
            if known.contains(&name) || self.is_damaged(&key.span()) || after_error {
                continue;
            }
            let path = join(section, name);
            let message = if looks_like_a_secret(name) {
                format!(
                    "{path} is not a setting: API keys and tokens are never kept in this file \
                     (they belong in the system keyring); ignored"
                )
            } else if matches!(value.get_ref(), DeValue::Table(_)) {
                format!("unknown section [{path}]; ignored")
            } else {
                format!("unknown setting {path}; ignored")
            };
            self.report(Severity::Warning, key.span().start, message);
        }
    }

    fn section<'a, 'i>(
        &mut self,
        parent: &'a DeTable<'i>,
        parent_path: &str,
        name: &str,
    ) -> Option<&'a DeTable<'i>> {
        let value = self.entry(parent, name)?;
        if let DeValue::Table(table) = value.get_ref() {
            Some(table)
        } else {
            self.wrong_type(&join(parent_path, name), "a table", value);
            None
        }
    }

    fn boolean(&mut self, table: &DeTable<'_>, section: &str, key: &str) -> Option<bool> {
        let value = self.entry(table, key)?;
        if let DeValue::Boolean(flag) = value.get_ref() {
            Some(*flag)
        } else {
            self.wrong_type(&join(section, key), "true or false", value);
            None
        }
    }

    /// A string, with the place of its value for messages.
    fn string<'a>(
        &mut self,
        table: &'a DeTable<'_>,
        section: &str,
        key: &str,
    ) -> Option<(&'a str, Range<usize>)> {
        let value = self.entry(table, key)?;
        if let DeValue::String(text) = value.get_ref() {
            Some((text.as_ref(), value.span()))
        } else {
            self.wrong_type(&join(section, key), "a string", value);
            None
        }
    }

    /// One of a fixed list of words.
    fn choice<T: Copy>(
        &mut self,
        table: &DeTable<'_>,
        section: &str,
        key: &str,
        options: &[(&str, T)],
    ) -> Option<T> {
        let (word, span) = self.string(table, section, key)?;
        if let Some((_, found)) = options.iter().find(|(name, _)| *name == word) {
            return Some(*found);
        }
        let quoted: Vec<String> = options
            .iter()
            .map(|(name, _)| format!("\"{name}\""))
            .collect();
        let list = match quoted.split_last() {
            Some((last, rest)) if !rest.is_empty() => format!("{} or {last}", rest.join(", ")),
            _ => quoted.join(""),
        };
        self.error(
            &span,
            format!(
                "{} must be {list}, not \"{}\"; using the default",
                join(section, key),
                word.escape_debug()
            ),
        );
        None
    }

    fn integer(
        &mut self,
        table: &DeTable<'_>,
        section: &str,
        key: &str,
        range: RangeInclusive<u32>,
    ) -> Option<u32> {
        let value = self.entry(table, key)?;
        let path = join(section, key);
        let DeValue::Integer(integer) = value.get_ref() else {
            self.wrong_type(&path, "an integer", value);
            return None;
        };
        let number = i64::from_str_radix(integer.as_str(), integer.radix()).ok();
        match number
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| range.contains(n))
        {
            Some(n) => Some(n),
            None => {
                self.error(
                    &value.span(),
                    format!(
                        "{path} must be a whole number from {} to {}, not {integer}; \
                         using the default",
                        range.start(),
                        range.end()
                    ),
                );
                None
            }
        }
    }

    /// An array of strings. One element that is not a string rejects the whole setting.
    fn strings<'a>(
        &mut self,
        table: &'a DeTable<'_>,
        section: &str,
        key: &str,
    ) -> Option<Vec<(&'a str, Range<usize>)>> {
        let value = self.entry(table, key)?;
        let path = join(section, key);
        let DeValue::Array(items) = value.get_ref() else {
            self.wrong_type(&path, "an array of strings", value);
            return None;
        };
        let mut out = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            if let DeValue::String(text) = item.get_ref() {
                out.push((text.as_ref(), item.span()));
            } else {
                self.wrong_type(&format!("{path}[{index}]"), "a string", item);
                return None;
            }
        }
        Some(out)
    }
}

fn join(section: &str, key: &str) -> String {
    if section.is_empty() {
        key.to_owned()
    } else {
        format!("{section}.{key}")
    }
}

fn describe(value: &DeValue<'_>) -> &'static str {
    match value {
        DeValue::String(_) => "a string",
        DeValue::Integer(_) => "an integer",
        DeValue::Float(_) => "a float",
        DeValue::Boolean(_) => "a boolean",
        DeValue::Datetime(_) => "a date or time",
        DeValue::Array(_) => "an array",
        DeValue::Table(_) => "a table",
    }
}

/// The keys the schema has for a section. The empty name is the top of the file.
fn known_keys(section: &str) -> Vec<&'static str> {
    if section.is_empty() {
        SECTIONS
            .iter()
            .map(|(name, _)| *name)
            .filter(|name| !name.contains('.'))
            .collect()
    } else {
        SECTIONS
            .iter()
            .find(|(name, _)| *name == section)
            .map_or_else(Vec::new, |(_, keys)| keys.to_vec())
    }
}

/// Names that suggest a credential. Users try `api_key = "..."` in config files; the message
/// says where keys go instead (§14.1).
fn looks_like_a_secret(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name == "key"
        || name.ends_with("_key")
        || name.ends_with("-key")
        || ["token", "secret", "password", "passwd"]
            .iter()
            .any(|word| name.contains(word))
}

fn read_config(r: &mut Reader<'_>, root: &DeTable<'_>) -> Config {
    let mut config = Config::default();
    r.unknown(root, "");
    if let Some(table) = r.section(root, "", "ui") {
        read_ui(r, table, &mut config.ui);
    }
    if let Some(table) = r.section(root, "", "editor") {
        read_editor(r, table, &mut config);
    }
    if let Some(table) = r.section(root, "", "prompt") {
        read_prompt(r, table, &mut config);
    }
    if let Some(table) = r.section(root, "", "history") {
        read_history(r, table, &mut config);
    }
    if let Some(table) = r.section(root, "", "safety") {
        read_safety(r, table, &mut config);
    }
    if let Some(table) = r.section(root, "", "ai") {
        read_ai(r, table, &mut config.ai);
    }
    if let Some(table) = r.section(root, "", "update") {
        r.unknown(table, "update");
        if let Some(check) = r.boolean(table, "update", "check") {
            config.update.check = check;
        }
        if let Some(channel) = r.choice(table, "update", "channel", &[("stable", Channel::Stable)])
        {
            config.update.channel = channel;
        }
    }
    config
}

fn read_ui(r: &mut Reader<'_>, table: &DeTable<'_>, ui: &mut Ui) {
    r.unknown(table, "ui");
    if let Some((name, span)) = r.string(table, "ui", "theme") {
        // The name becomes a file name under `themes/`, so it must not be a path.
        let valid = !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if valid {
            name.clone_into(&mut ui.theme);
        } else {
            r.error(
                &span,
                format!(
                    "ui.theme must be a name made of letters, digits, \"-\" and \"_\", \
                     not \"{}\"; using the default",
                    name.escape_debug()
                ),
            );
        }
    }
    let motion = [
        ("full", Motion::Full),
        ("reduced", Motion::Reduced),
        ("off", Motion::Off),
    ];
    if let Some(motion) = r.choice(table, "ui", "motion", &motion) {
        ui.motion = Some(motion);
    }
    let icons = [
        ("unicode", Icons::Unicode),
        ("ascii", Icons::Ascii),
        ("auto", Icons::Auto),
    ];
    if let Some(icons) = r.choice(table, "ui", "icons", &icons) {
        ui.icons = icons;
    }
}

fn read_editor(r: &mut Reader<'_>, table: &DeTable<'_>, config: &mut Config) {
    r.unknown(table, "editor");
    let editor = &mut config.editor;
    let modes = [("emacs", EditMode::Emacs), ("vi", EditMode::Vi)];
    if let Some(mode) = r.choice(table, "editor", "mode", &modes) {
        editor.mode = mode;
    }
    if let Some(hints) = r.boolean(table, "editor", "hints") {
        editor.hints = hints;
    }
    if let Some(words) = r.section(table, "editor", "abbreviations") {
        let mut found = BTreeMap::new();
        for (key, value) in words {
            let word: &str = key.get_ref();
            let path = format!("editor.abbreviations.{word}");
            if r.is_damaged(&key.span()) || r.is_damaged(&value.span()) {
                continue;
            }
            if word.is_empty() || word.chars().any(char::is_whitespace) {
                r.error(
                    &key.span(),
                    format!(
                        "an abbreviation must be one word without spaces, not \"{}\"; ignored",
                        word.escape_debug()
                    ),
                );
            } else if let DeValue::String(text) = value.get_ref() {
                if text.is_empty() {
                    r.error(&value.span(), format!("{path} must not be empty; ignored"));
                } else {
                    found.insert(word.to_owned(), text.to_string());
                }
            } else {
                r.wrong_type(&path, "a string", value);
            }
        }
        editor.abbreviations = found;
    }
}

fn read_prompt(r: &mut Reader<'_>, table: &DeTable<'_>, config: &mut Config) {
    r.unknown(table, "prompt");
    for (key, target) in [
        ("left", &mut config.prompt.left),
        ("right", &mut config.prompt.right),
    ] {
        let Some(items) = r.strings(table, "prompt", key) else {
            continue;
        };
        if let Some((name, span)) = items
            .iter()
            .find(|(name, _)| !PROMPT_SEGMENTS.contains(name))
        {
            r.error(
                span,
                format!(
                    "prompt.{key} has an unknown segment \"{}\" (the segments are {}); \
                     using the default",
                    name.escape_debug(),
                    PROMPT_SEGMENTS.join(", ")
                ),
            );
        } else {
            *target = items.iter().map(|(name, _)| (*name).to_owned()).collect();
        }
    }
    if let Some(transient) = r.boolean(table, "prompt", "transient") {
        config.prompt.transient = transient;
    }
}

fn read_history(r: &mut Reader<'_>, table: &DeTable<'_>, config: &mut Config) {
    r.unknown(table, "history");
    if let Some(size) = r.integer(table, "history", "size", 1..=10_000_000) {
        config.history.size = size;
    }
    if let Some(items) = r.strings(table, "history", "ignore") {
        config.history.ignore = items.iter().map(|(text, _)| (*text).to_owned()).collect();
    }
}

fn read_safety(r: &mut Reader<'_>, table: &DeTable<'_>, config: &mut Config) {
    r.unknown(table, "safety");
    let safety = &mut config.safety;
    let levels = [
        ("caution", ConfirmLevel::Caution),
        ("dangerous", ConfirmLevel::Dangerous),
        ("critical", ConfirmLevel::Critical),
    ];
    if let Some(level) = r.choice(table, "safety", "confirm", &levels) {
        safety.confirm = level;
    }
    if let Some(days) = r.integer(table, "safety", "trash_retention_days", 1..=3650) {
        safety.trash_retention_days = days;
    }
    if let Some(n) = r.integer(table, "safety", "blast_radius_max_entries", 1..=10_000_000) {
        safety.blast_radius_max_entries = n;
    }
    if let Some(ms) = r.integer(table, "safety", "blast_radius_max_ms", 1..=60_000) {
        safety.blast_radius_max_ms = ms;
    }
}

fn read_ai(r: &mut Reader<'_>, table: &DeTable<'_>, ai: &mut Ai) {
    r.unknown(table, "ai");
    if let Some(enabled) = r.boolean(table, "ai", "enabled") {
        ai.enabled = enabled;
    }
    let providers = [
        ("ollama", Provider::Ollama),
        ("anthropic", Provider::Anthropic),
        ("openai", Provider::OpenAi),
        ("gemini", Provider::Gemini),
        ("openai-compatible", Provider::OpenAiCompatible),
    ];
    if let Some(provider) = r.choice(table, "ai", "provider", &providers) {
        ai.provider = Some(provider);
    }
    if let Some((model, span)) = r.string(table, "ai", "model") {
        if model.trim().is_empty() {
            r.error(
                &span,
                "ai.model must not be empty; using the default".to_owned(),
            );
        } else {
            ai.model = Some(model.to_owned());
        }
    }
    if let Some((url, span)) = r.string(table, "ai", "base_url") {
        match check_url(url) {
            Ok(()) => ai.base_url = Some(url.to_owned()),
            // The text is not repeated: it may hold a password.
            Err(why) => r.error(&span, format!("ai.base_url {why}; using the default")),
        }
    }
    let languages = [
        ("auto", AiLanguage::Auto),
        ("en", AiLanguage::English),
        ("bn", AiLanguage::Bangla),
    ];
    if let Some(language) = r.choice(table, "ai", "language", &languages) {
        ai.language = language;
    }
    if let Some(context) = r.section(table, "ai", "context") {
        r.unknown(context, "ai.context");
        for (key, target) in [
            ("os", &mut ai.context.os),
            ("cwd", &mut ai.context.cwd),
            ("git", &mut ai.context.git),
            ("recent_commands", &mut ai.context.recent_commands),
        ] {
            if let Some(flag) = r.boolean(context, "ai.context", key) {
                *target = flag;
            }
        }
    }
}

/// An `http://` or `https://` address without credentials (§14.1: nothing secret in this file).
fn check_url(url: &str) -> Result<(), &'static str> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .ok_or("must start with http:// or https://")?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() {
        Err("must name a host")
    } else if authority.contains('@') {
        Err("must not contain a user name or password")
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
