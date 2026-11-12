//! Theme files: `themes/<name>.toml` next to `config.toml`.
//!
//! ```toml
//! [tokens]
//! danger = "magenta"                          # a color name: the color changes, the
//!                                             # attributes of `nord` stay
//! dim = { color = "bright-black", dim = true } # a table: the whole look is given
//!
//! [roles]
//! quote = "green"
//! ```
//!
//! Only the 16 ANSI color names (and `"default"` for the terminal's own color) are accepted.
//! Hex values, 256-color numbers and `bold` are refused with their line and column, and the key
//! keeps the look it has in `nord`. A key that is not a token or a role is a warning. A file
//! that is not valid TOML is ignored as a whole (`nord` is used) with the syntax error.

use std::ops::Range;
use std::path::PathBuf;

use omsh_config::{Paths, Problem, Severity, read_text_file};
use toml::Spanned;
use toml::de::{DeTable, DeValue};

use crate::theme::{AnsiColor, BUILTIN, Role, RoleStyle, Theme, Token};

/// A theme, with what went wrong while reading it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedTheme {
    /// The theme to use: always usable, `nord` where the file said nothing or said something
    /// wrong.
    pub theme: Theme,
    /// Problems with their place in `file`.
    pub problems: Vec<Problem>,
    /// The file that was read, if any. Built-in themes read none.
    pub file: Option<PathBuf>,
}

impl LoadedTheme {
    /// The lines to show the user, at most `limit` of them (then one line says how many more).
    /// A problem with a place reads `file:line:column: message`.
    #[must_use]
    pub fn report(&self, limit: usize) -> Vec<String> {
        let place = self
            .file
            .as_ref()
            .map_or_else(|| "theme".to_owned(), |file| file.display().to_string());
        let mut lines: Vec<String> = self
            .problems
            .iter()
            .take(limit)
            .map(|problem| {
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

/// Looks for the theme called `name`. A built-in name never touches the disk. Any other name
/// is read from `themes/<name>.toml`; a missing or unreadable file gives `nord` and one problem.
#[must_use]
pub fn load_theme(name: &str, paths: &Paths) -> LoadedTheme {
    if let Some(theme) = Theme::builtin(name) {
        return LoadedTheme {
            theme,
            problems: Vec::new(),
            file: None,
        };
    }
    let fallback = |file: Option<PathBuf>, message: String| LoadedTheme {
        theme: Theme::default(),
        problems: vec![Problem {
            severity: Severity::Error,
            line: None,
            column: None,
            message,
        }],
        file,
    };
    let Some(file) = paths.theme_file(name) else {
        return fallback(None, format!("no theme called \"{name}\"; using {BUILTIN}"));
    };
    match read_text_file(&file) {
        Ok(Some(text)) => {
            let (theme, problems) = parse_theme(&text);
            LoadedTheme {
                theme,
                problems,
                file: Some(file),
            }
        }
        Ok(None) => fallback(
            Some(file),
            format!("no theme called \"{name}\" (there is no such file); using {BUILTIN}"),
        ),
        Err(error) => fallback(Some(file), format!("{error}; using {BUILTIN}")),
    }
}

/// Reads a theme file's text. Starts from `nord`.
#[must_use]
pub fn parse_theme(text: &str) -> (Theme, Vec<Problem>) {
    let mut theme = Theme::default();
    // What a key falls back to is the built-in look, not what an earlier key just set.
    let nord = Theme::default();
    let mut reader = Reader {
        text,
        problems: Vec::new(),
    };
    let root = match DeTable::parse(text) {
        Ok(root) => root,
        Err(error) => {
            let at = error.span().map_or(0, |span| span.start);
            reader.report(
                Severity::Error,
                at,
                format!("{}; the whole file is ignored", error.message()),
            );
            return (theme, reader.problems);
        }
    };
    for (key, value) in root.get_ref() {
        let name: &str = key.get_ref();
        match (name, value.get_ref()) {
            ("tokens", DeValue::Table(table)) => {
                reader.entries(
                    table,
                    "tokens",
                    Token::from_name,
                    |kind, style| {
                        theme.set_token(kind, style);
                    },
                    |kind| nord.token(kind),
                );
            }
            ("roles", DeValue::Table(table)) => {
                reader.entries(
                    table,
                    "roles",
                    Role::from_name,
                    |kind, style| {
                        theme.set(kind, style);
                    },
                    |kind| nord.get(kind),
                );
            }
            ("tokens" | "roles", other) => reader.report(
                Severity::Error,
                value.span().start,
                format!("[{name}] must be a table, not {}; ignored", describe(other)),
            ),
            _ => reader.report(
                Severity::Warning,
                key.span().start,
                format!("unknown section [{name}]; ignored"),
            ),
        }
    }
    reader.problems.sort_by_key(|p| (p.line, p.column));
    (theme, reader.problems)
}

struct Reader<'t> {
    text: &'t str,
    problems: Vec<Problem>,
}

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

    /// Every key of a `[tokens]` or `[roles]` table.
    fn entries<K: Copy>(
        &mut self,
        table: &DeTable<'_>,
        section: &str,
        find: impl Fn(&str) -> Option<K>,
        mut apply: impl FnMut(K, RoleStyle),
        current: impl Fn(K) -> RoleStyle,
    ) {
        for (key, value) in table {
            let name: &str = key.get_ref();
            let Some(kind) = find(name) else {
                self.report(
                    Severity::Warning,
                    key.span().start,
                    format!("unknown key {section}.{name}; ignored"),
                );
                continue;
            };
            if let Some(style) = self.style(&format!("{section}.{name}"), value, current(kind)) {
                apply(kind, style);
            }
        }
    }

    /// The look a value describes, or `None` after reporting why not.
    fn style(
        &mut self,
        path: &str,
        value: &Spanned<DeValue<'_>>,
        base: RoleStyle,
    ) -> Option<RoleStyle> {
        match value.get_ref() {
            DeValue::String(word) => {
                let color = self.color(path, word, &value.span())?;
                Some(RoleStyle { color, ..base })
            }
            DeValue::Table(table) => self.attributes(path, table),
            other => {
                self.error(
                    &value.span(),
                    format!(
                        "{path} must be a color name or a table, not {}; keeping {BUILTIN}'s look",
                        describe(other)
                    ),
                );
                None
            }
        }
    }

    /// `None` inside `Some` is the terminal's own color (`"default"`).
    fn color(&mut self, path: &str, word: &str, span: &Range<usize>) -> Option<Option<AnsiColor>> {
        if word == "default" {
            return Some(None);
        }
        if let Some(color) = AnsiColor::from_name(word) {
            return Some(Some(color));
        }
        self.error(
            span,
            format!(
                "{path}: \"{word}\" is not a color name; themes use the 16 ANSI names \
                 (black, red, green, yellow, blue, magenta, cyan, white and bright-black \
                 to bright-white) or \"default\", not hex or 256-color numbers; \
                 keeping {BUILTIN}'s look"
            ),
        );
        None
    }

    fn attributes(&mut self, path: &str, table: &DeTable<'_>) -> Option<RoleStyle> {
        let mut style = RoleStyle::PLAIN;
        let mut good = true;
        for (key, value) in table {
            let name: &str = key.get_ref();
            match (name, value.get_ref()) {
                ("color", DeValue::String(word)) => {
                    match self.color(&format!("{path}.color"), word, &value.span()) {
                        Some(color) => style.color = color,
                        None => good = false,
                    }
                }
                ("dim", DeValue::Boolean(flag)) => style.dim = *flag,
                ("underline", DeValue::Boolean(flag)) => style.underline = *flag,
                ("reverse", DeValue::Boolean(flag)) => style.reverse = *flag,
                ("bold", _) => {
                    self.error(
                        &key.span(),
                        format!(
                            "{path}.bold: bold is not available; a look is a color and the \
                             attributes dim, underline and reverse"
                        ),
                    );
                    good = false;
                }
                ("color", other) => {
                    self.error(
                        &value.span(),
                        format!("{path}.color must be a color name, not {}", describe(other)),
                    );
                    good = false;
                }
                ("dim" | "underline" | "reverse", other) => {
                    self.error(
                        &value.span(),
                        format!(
                            "{path}.{name} must be true or false, not {}",
                            describe(other)
                        ),
                    );
                    good = false;
                }
                _ => {
                    self.report(
                        Severity::Warning,
                        key.span().start,
                        format!("unknown key {path}.{name}; ignored"),
                    );
                }
            }
        }
        if !good {
            return None;
        }
        Some(style)
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

/// Line and column (from 1) of a byte offset. The column counts characters.
fn position(text: &str, offset: usize) -> (usize, usize) {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    let before = &text[..offset];
    let line = before.matches('\n').count() + 1;
    let column = before
        .rsplit('\n')
        .next()
        .map_or(0, |last| last.chars().count())
        + 1;
    (line, column)
}

#[cfg(test)]
mod tests;
