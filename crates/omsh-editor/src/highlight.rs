//! Syntax highlighting for the command line, in the style of fish.
//!
//! [`ShellHighlighter`] asks the lexer's cheap path ([`omsh_syntax::lex_for_highlight`]) what
//! each piece of the text is, turns that into a [`Role`], and looks the role up in the
//! [`Theme`]. Two roles need to look at the system, not just at the text:
//!
//! - a **command name** is red if nothing would run (no builtin, function, alias or program of
//!   that name), and has no color otherwise;
//! - an **argument** that names an existing file or directory is underlined, and keeps its color.
//!
//! Both questions are answered by [`Context`], which remembers each answer until the next
//! prompt, so typing a long line asks the file system about each distinct word only once.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use nu_ansi_term::{Color, Style};
use omsh_syntax::{HighlightKind, HighlightWord, lex_for_highlight, trigger};
use omsh_ui::{AnsiColor, Role, RoleStyle, Theme};
use reedline::{Highlighter, StyledText};

/// What the editor asks the shell about command names. The shell passes one in for each prompt
/// (see `Editor::read`), so this crate does not depend on the executor.
pub trait CommandLookup: Send {
    /// True if running `name` would do something: it is a builtin, a function, an alias, or a
    /// program that can be run (found in `PATH`, or at the path it names).
    fn is_command(&self, name: &str) -> bool;
}

/// The facts a highlight needs, and what has been asked already.
pub(crate) struct Context {
    lookup: Option<Box<dyn CommandLookup>>,
    cwd: PathBuf,
    home: Option<PathBuf>,
    commands: HashMap<String, bool>,
    paths: HashMap<PathBuf, bool>,
}

impl Context {
    pub(crate) fn new() -> Self {
        Self {
            lookup: None,
            cwd: PathBuf::from("/"),
            home: None,
            commands: HashMap::new(),
            paths: HashMap::new(),
        }
    }

    /// Starts a new prompt: new facts, and every remembered answer is forgotten. That is how a
    /// program installed by the last command, a new function, a changed `PATH` or a `cd`
    /// are seen by the next line.
    pub(crate) fn reset(
        &mut self,
        lookup: Box<dyn CommandLookup>,
        cwd: PathBuf,
        home: Option<PathBuf>,
    ) {
        self.lookup = Some(lookup);
        self.cwd = cwd;
        self.home = home;
        self.commands.clear();
        self.paths.clear();
    }

    /// `~` and `~/x` become the home directory; `~user` cannot be checked (`None`). A word that
    /// does not start with a tilde is returned as it is.
    fn expand_tilde(&self, value: &str, tilde: bool) -> Option<String> {
        if !tilde {
            return Some(value.to_owned());
        }
        let home = self.home.as_ref()?.to_str()?;
        match value.strip_prefix('~')? {
            "" => Some(home.to_owned()),
            rest if rest.starts_with('/') => Some(format!("{home}{rest}")),
            _ => None,
        }
    }

    /// True if `value` (the text of a command word after quote removal) would run something.
    /// Before the first prompt there are no facts, and everything counts as found.
    fn is_command(&mut self, value: &str, tilde: bool) -> bool {
        let Some(name) = self.expand_tilde(value, tilde) else {
            return true;
        };
        if name.is_empty() {
            return false;
        }
        if let Some(known) = self.commands.get(&name) {
            return *known;
        }
        let Some(lookup) = &self.lookup else {
            return true;
        };
        let found = lookup.is_command(&name);
        self.commands.insert(name, found);
        found
    }

    /// True if `value` names an existing file or directory, relative to the prompt's directory.
    fn is_path(&mut self, value: &str, tilde: bool) -> bool {
        let Some(text) = self.expand_tilde(value, tilde) else {
            return false;
        };
        if text.is_empty() {
            return false;
        }
        let path = self.cwd.join(&text);
        if let Some(known) = self.paths.get(&path) {
            return *known;
        }
        let exists = fs::metadata(&path).is_ok();
        self.paths.insert(path, exists);
        exists
    }
}

/// The highlighter that reedline calls on every keystroke.
pub struct ShellHighlighter {
    theme: Theme,
    context: Arc<Mutex<Context>>,
}

impl ShellHighlighter {
    pub(crate) fn new(theme: Theme, context: Arc<Mutex<Context>>) -> Self {
        Self { theme, context }
    }

    /// A highlighter that is not tied to an [`Editor`](crate::Editor): it has its own facts, for
    /// the benchmark. The answers it remembers are forgotten by [`ShellHighlighter::new_prompt`].
    #[must_use]
    pub fn standalone(
        theme: Theme,
        lookup: Box<dyn CommandLookup>,
        cwd: PathBuf,
        home: Option<PathBuf>,
    ) -> Self {
        let mut context = Context::new();
        context.reset(lookup, cwd, home);
        Self::new(theme, Arc::new(Mutex::new(context)))
    }

    /// Forgets what was remembered, as the start of a new prompt does. For the benchmark's
    /// worst case, where every word is asked about again.
    pub fn new_prompt(&self) {
        let mut context = self.context.lock().unwrap_or_else(PoisonError::into_inner);
        context.commands.clear();
        context.paths.clear();
    }

    fn style(&self, role: Role) -> RoleStyle {
        self.theme.get(role)
    }
}

/// The role of a piece of text. Words that are commands or arguments get a second look in
/// [`ShellHighlighter::highlight`].
fn role_of(kind: HighlightKind) -> Role {
    match kind {
        HighlightKind::Command => Role::Command,
        HighlightKind::Keyword => Role::Keyword,
        HighlightKind::Argument | HighlightKind::Assignment => Role::Param,
        HighlightKind::Operator => Role::Operator,
        HighlightKind::Separator => Role::End,
        HighlightKind::Redirect | HighlightKind::IoNumber | HighlightKind::HereDocDelimiter => {
            Role::Redirection
        }
        HighlightKind::Comment => Role::Comment,
        HighlightKind::SingleQuoted | HighlightKind::DoubleQuoted | HighlightKind::HereDocBody => {
            Role::Quote
        }
        HighlightKind::Escape => Role::Escape,
        HighlightKind::Variable | HighlightKind::CommandSub | HighlightKind::Arithmetic => {
            Role::Expansion
        }
    }
}

/// What a word says when its quotes and escapes are taken away, if that can be known from the
/// text alone: `None` when it holds a `$`, a backtick or a `$'...'` outside single quotes.
/// An unfinished quote simply runs to the end.
fn literal_value(word: &str) -> Option<String> {
    let mut out = String::with_capacity(word.len());
    let mut chars = word.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                for q in chars.by_ref() {
                    if q == '\'' {
                        break;
                    }
                    out.push(q);
                }
            }
            '"' => loop {
                match chars.next() {
                    None | Some('"') => break,
                    Some('\\') => match chars.peek().copied() {
                        Some(n @ ('$' | '`' | '"' | '\\')) => {
                            out.push(n);
                            chars.next();
                        }
                        Some('\n') => {
                            chars.next();
                        }
                        _ => out.push('\\'),
                    },
                    Some('$' | '`') => return None,
                    Some(q) => out.push(q),
                }
            },
            '\\' => match chars.next() {
                None | Some('\n') => {}
                Some(n) => out.push(n),
            },
            '$' | '`' => return None,
            other => out.push(other),
        }
    }
    Some(out)
}

/// One colored piece of the line.
struct Piece {
    start: usize,
    end: usize,
    kind: HighlightKind,
    style: RoleStyle,
}

impl Highlighter for ShellHighlighter {
    fn highlight(&self, line: &str, _cursor: usize) -> StyledText {
        // A request to the AI is natural language: no red `?`, no quote or glob colors, and
        // no lookup of a "command".
        if trigger(line).is_some() {
            return render(
                line,
                &[Piece {
                    start: 0,
                    end: line.len(),
                    kind: HighlightKind::Argument,
                    style: self.style(Role::Normal),
                }],
            );
        }
        let lexed = lex_for_highlight(line);
        let mut pieces: Vec<Piece> = lexed
            .spans
            .iter()
            .map(|span| Piece {
                start: span.span.start,
                end: span.span.end,
                kind: span.kind,
                style: self.style(role_of(span.kind)),
            })
            .collect();

        {
            let mut context = self.context.lock().unwrap_or_else(PoisonError::into_inner);
            for word in &lexed.words {
                self.look_at_word(&mut context, line, word, &mut pieces);
            }
        }

        // Only a problem that more input cannot fix is shown as one. A quote that is still open
        // is just being typed.
        for error in lexed.errors.iter().filter(|e| !e.kind.is_incomplete()) {
            for piece in &mut pieces {
                if piece.start < error.span.end && error.span.start < piece.end {
                    piece.style = self.style(Role::Error);
                }
            }
        }

        render(line, &pieces)
    }
}

impl ShellHighlighter {
    /// Gives the pieces of one word their final look.
    fn look_at_word(
        &self,
        context: &mut Context,
        line: &str,
        word: &HighlightWord,
        pieces: &mut [Piece],
    ) {
        if !matches!(word.kind, HighlightKind::Command | HighlightKind::Argument) {
            return;
        }
        let Some(text) = line.get(word.span.start..word.span.end) else {
            return;
        };
        let Some(value) = literal_value(text) else {
            // It holds an expansion; what it comes to is known only when it runs.
            return;
        };
        let tilde = text.starts_with('~');
        // The pieces of this word. They are in order, so the first one is found by search.
        let first = pieces.partition_point(|piece| piece.end <= word.span.start);
        let inside = pieces[first..]
            .iter_mut()
            .take_while(|piece| piece.start < word.span.end);

        match word.kind {
            HighlightKind::Command => {
                if !context.is_command(&value, tilde) {
                    let error = self.style(Role::Error);
                    for piece in inside {
                        piece.style = error;
                    }
                }
            }
            _ => {
                let is_option = value.starts_with('-');
                let underline = !is_option && context.is_path(&value, tilde);
                for piece in inside {
                    if is_option && piece.kind == HighlightKind::Argument {
                        piece.style = self.style(Role::Option);
                    }
                    if underline {
                        piece.style = piece.style.overlaid_with(self.style(Role::ValidPath));
                    }
                }
            }
        }
    }
}

/// Builds the styled text: the pieces, and the blanks between them in the default look.
/// If the pieces do not fit the text (they always do), the line is returned unstyled.
fn render(line: &str, pieces: &[Piece]) -> StyledText {
    let mut text = StyledText::new();
    let mut at = 0;
    for piece in pieces {
        let (Some(gap), Some(body)) = (line.get(at..piece.start), line.get(piece.start..piece.end))
        else {
            let mut plain = StyledText::new();
            plain.push((Style::new(), line.to_owned()));
            return plain;
        };
        if !gap.is_empty() {
            text.push((Style::new(), gap.to_owned()));
        }
        text.push((to_style(piece.style), body.to_owned()));
        at = piece.end;
    }
    if let Some(rest) = line.get(at..).filter(|rest| !rest.is_empty()) {
        text.push((Style::new(), rest.to_owned()));
    }
    text
}

/// The terminal color for one of the 16 names.
fn to_color(color: AnsiColor) -> Color {
    match color {
        AnsiColor::Black => Color::Black,
        AnsiColor::Red => Color::Red,
        AnsiColor::Green => Color::Green,
        AnsiColor::Yellow => Color::Yellow,
        AnsiColor::Blue => Color::Blue,
        AnsiColor::Magenta => Color::Magenta,
        AnsiColor::Cyan => Color::Cyan,
        AnsiColor::White => Color::White,
        AnsiColor::BrightBlack => Color::DarkGray,
        AnsiColor::BrightRed => Color::LightRed,
        AnsiColor::BrightGreen => Color::LightGreen,
        AnsiColor::BrightYellow => Color::LightYellow,
        AnsiColor::BrightBlue => Color::LightBlue,
        AnsiColor::BrightMagenta => Color::LightMagenta,
        AnsiColor::BrightCyan => Color::LightCyan,
        AnsiColor::BrightWhite => Color::LightGray,
    }
}

/// The drawing style for a role's look. Bold is never set.
pub(crate) fn to_style(style: RoleStyle) -> Style {
    let mut out = Style::new();
    if let Some(color) = style.color {
        out = out.fg(to_color(color));
    }
    if style.dim {
        out = out.dimmed();
    }
    if style.underline {
        out = out.underline();
    }
    if style.reverse {
        out = out.reverse();
    }
    out
}

#[cfg(test)]
mod tests;
