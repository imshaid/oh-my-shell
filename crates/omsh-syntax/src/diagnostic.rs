//! Error reports for the user, built on `miette`.
//!
//! [`ParseError`] only records what went wrong and where. It has no `Display`, because a good
//! message quotes the source text, and the error does not own that. [`ParseError::to_diagnostic`]
//! takes the source and builds a [`SyntaxDiagnostic`], which owns everything it needs and
//! implements `miette::Diagnostic`. A caller can print it with any `miette` handler.

use std::borrow::Cow;
use std::fmt;
use std::sync::Arc;

use miette::{Diagnostic, LabeledSpan, SourceCode, SourceSpan};

use crate::parser::{
    Expected, MAX_DEPTH, ParseError, ParseErrorKind, ParseFailure, Parsed, is_name,
};
use crate::scanner::MAX_NESTING;
use crate::span::{Span, strip_line_continuations};
use crate::token::LexErrorKind;

/// The longest piece of source text that a message quotes.
const QUOTE_LIMIT: usize = 24;

/// One problem, with everything needed to show it: a message, a labeled place in the source,
/// and a hint. It owns its text, so it can be kept, sent between threads and shown later.
#[derive(Clone, Debug)]
pub struct SyntaxDiagnostic {
    message: String,
    label: String,
    help: Option<String>,
    code: &'static str,
    span: Span,
    source: Arc<str>,
    related: Vec<Self>,
}

impl SyntaxDiagnostic {
    /// The short message, such as ``unexpected `fi` ``.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The text next to the marked place in the source.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// A hint about what to do, if there is one.
    #[must_use]
    pub fn help_text(&self) -> Option<&str> {
        self.help.as_deref()
    }

    /// A stable name for the kind of problem, such as `omsh::syntax::unsupported`.
    #[must_use]
    pub const fn code_name(&self) -> &'static str {
        self.code
    }

    /// The marked place in the source.
    #[must_use]
    pub const fn span(&self) -> Span {
        self.span
    }

    /// The span to mark in the report. `miette` draws nothing for an empty span, and a problem
    /// at the end of the input has one, so the last character before it is marked instead.
    fn shown_span(&self) -> Span {
        if self.span.start != self.span.end {
            return self.span;
        }
        let before = self.source.get(..self.span.start).unwrap_or("").trim_end();
        match before.chars().next_back() {
            Some(last) => Span::new(before.len() - last.len_utf8(), before.len()),
            None => self.span,
        }
    }

    /// The further problems, when the report comes from a [`ParseFailure`] or a [`Parsed`].
    #[must_use]
    pub fn further(&self) -> &[Self] {
        &self.related
    }
}

impl fmt::Display for SyntaxDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SyntaxDiagnostic {}

impl Diagnostic for SyntaxDiagnostic {
    fn code<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        Some(Box::new(self.code))
    }

    fn help<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        let help = self.help.as_deref()?;
        Some(Box::new(help))
    }

    fn source_code(&self) -> Option<&dyn SourceCode> {
        Some(&self.source)
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = LabeledSpan> + '_>> {
        let shown = self.shown_span();
        let span = SourceSpan::new(shown.start.into(), shown.end - shown.start);
        Some(Box::new(std::iter::once(LabeledSpan::new_with_span(
            Some(self.label.clone()),
            span,
        ))))
    }

    fn related<'a>(&'a self) -> Option<Box<dyn Iterator<Item = &'a dyn Diagnostic> + 'a>> {
        if self.related.is_empty() {
            return None;
        }
        Some(Box::new(self.related.iter().map(|d| d as &dyn Diagnostic)))
    }
}

impl ParseError {
    /// Builds the report for this problem. `src` must be the text that was parsed.
    #[must_use]
    pub fn to_diagnostic(&self, src: &str) -> SyntaxDiagnostic {
        self.describe(&Arc::from(src))
    }

    fn describe(&self, source: &Arc<str>) -> SyntaxDiagnostic {
        let text = self.span.text(source);
        let message = Message::new(self.kind, text);
        SyntaxDiagnostic {
            message: message.message,
            label: message.label,
            help: message.help,
            code: message.code,
            span: self.span,
            source: Arc::clone(source),
            related: Vec::new(),
        }
    }
}

/// Builds one report for several problems: the first is the main one, and the others hang
/// off it. Returns nothing if there are no problems.
fn combine(errors: &[ParseError], src: &str) -> Option<SyntaxDiagnostic> {
    let source: Arc<str> = Arc::from(src);
    let (first, rest) = errors.split_first()?;
    let mut main = first.describe(&source);
    main.related = rest.iter().map(|error| error.describe(&source)).collect();
    Some(main)
}

impl ParseFailure {
    /// Builds the report for a failed parse. `src` must be the text that was parsed.
    #[must_use]
    pub fn to_diagnostic(&self, src: &str) -> SyntaxDiagnostic {
        combine(&self.errors, src).unwrap_or_else(|| empty_report(src))
    }
}

impl Parsed {
    /// Builds the report for the problems of a tolerant parse, or nothing if there are none.
    /// `src` must be the text that was parsed.
    #[must_use]
    pub fn to_diagnostic(&self, src: &str) -> Option<SyntaxDiagnostic> {
        combine(&self.errors, src)
    }
}

/// A [`ParseFailure`] is never empty, but its field is public, so something has to be said.
fn empty_report(src: &str) -> SyntaxDiagnostic {
    SyntaxDiagnostic {
        message: "the line could not be parsed".to_owned(),
        label: "here".to_owned(),
        help: None,
        code: "omsh::syntax::unknown",
        span: Span::new(0, 0),
        source: Arc::from(src),
        related: Vec::new(),
    }
}

/// The words of one report.
struct Message {
    code: &'static str,
    message: String,
    label: String,
    help: Option<String>,
}

impl Message {
    fn new(kind: ParseErrorKind, text: &str) -> Self {
        let quoted = quote(text);
        match kind {
            ParseErrorKind::Lex(kind) => Self::lex(kind),
            ParseErrorKind::Unexpected { expected } => Self {
                code: "omsh::syntax::unexpected",
                message: format!("unexpected {quoted}"),
                label: format!("expected {} here", expected_text(expected)),
                help: None,
            },
            ParseErrorKind::UnexpectedEnd { expected } => Self {
                code: "omsh::syntax::unexpected_end",
                message: "the input ends too early".to_owned(),
                label: format!("expected {}", expected_text(expected)),
                help: None,
            },
            ParseErrorKind::BadFunctionName => {
                let name = strip_line_continuations(text);
                let message = if is_name(&name) {
                    format!("{quoted} is a special builtin and cannot be a function name")
                } else {
                    format!("{quoted} is not a valid function name")
                };
                Self {
                    code: "omsh::syntax::bad_name",
                    message,
                    label: "this name is not allowed".to_owned(),
                    help: Some(
                        "a name has letters, digits and `_`, and does not start with a digit"
                            .to_owned(),
                    ),
                }
            }
            ParseErrorKind::BadForName => {
                let message = if text.is_empty() {
                    "the `for` loop needs a variable name".to_owned()
                } else {
                    format!("{quoted} is not a valid variable name")
                };
                Self {
                    code: "omsh::syntax::bad_name",
                    message,
                    label: "expected a name such as `i`".to_owned(),
                    help: Some("write `for name in words; do ...; done`".to_owned()),
                }
            }
            ParseErrorKind::NestingTooDeep => Self {
                code: "omsh::syntax::nesting",
                message: "commands are nested too deeply".to_owned(),
                label: "the shell stops reading here".to_owned(),
                help: Some(format!("at most {MAX_DEPTH} levels can be nested")),
            },
            ParseErrorKind::BadSubstitution => Self {
                code: "omsh::syntax::bad_substitution",
                message: "bad substitution".to_owned(),
                label: format!("{quoted} is not a valid expansion"),
                help: Some(
                    "write `${name}`, `${#name}`, or `${name:-word}` with `-`, `=`, `?`, `+`, \
                     `#` or `%`"
                        .to_owned(),
                ),
            },
            ParseErrorKind::Unsupported(feature) => Self {
                code: "omsh::syntax::unsupported",
                message: format!("{} is not supported in Oh My Shell", feature.what()),
                label: "not supported".to_owned(),
                help: Some(feature.help().to_owned()),
            },
        }
    }

    fn lex(kind: LexErrorKind) -> Self {
        let (code, message, label) = match kind {
            LexErrorKind::UnterminatedSingleQuote => (
                "omsh::syntax::unterminated",
                "the single quote is never closed",
                "this quote has no partner",
            ),
            LexErrorKind::UnterminatedAnsiCQuote => (
                "omsh::syntax::unterminated",
                "the `$'` quote is never closed",
                "this quote has no partner",
            ),
            LexErrorKind::UnterminatedDoubleQuote => (
                "omsh::syntax::unterminated",
                "the double quote is never closed",
                "this quote has no partner",
            ),
            LexErrorKind::UnterminatedBacktick => (
                "omsh::syntax::unterminated",
                "the backtick is never closed",
                "this backtick has no partner",
            ),
            LexErrorKind::UnterminatedCommandSub => (
                "omsh::syntax::unterminated",
                "the `$(` is never closed",
                "expected a `)` for this",
            ),
            LexErrorKind::UnterminatedArithmetic => (
                "omsh::syntax::unterminated",
                "the `$((` is never closed",
                "expected a `))` for this",
            ),
            LexErrorKind::UnterminatedParameter => (
                "omsh::syntax::unterminated",
                "the `${` is never closed",
                "expected a `}` for this",
            ),
            LexErrorKind::UnterminatedHereDoc => (
                "omsh::syntax::unterminated",
                "the here-document is never closed",
                "its delimiter line never came",
            ),
            LexErrorKind::TrailingBackslash => (
                "omsh::syntax::trailing_backslash",
                "the input ends in a backslash",
                "nothing follows this backslash",
            ),
            LexErrorKind::NestingTooDeep => (
                "omsh::syntax::nesting",
                "quotes and substitutions are nested too deeply",
                "the shell stops reading here",
            ),
        };
        let help = match kind {
            LexErrorKind::NestingTooDeep => {
                Some(format!("at most {MAX_NESTING} levels can be nested"))
            }
            LexErrorKind::TrailingBackslash => {
                Some("remove the backslash, or write the rest of the line after it".to_owned())
            }
            _ => None,
        };
        Self {
            code,
            message: message.to_owned(),
            label: label.to_owned(),
            help,
        }
    }
}

/// What the parser wanted, as it reads in "expected ... here".
fn expected_text(expected: Expected) -> Cow<'static, str> {
    match expected {
        Expected::Command => Cow::Borrowed("a command"),
        Expected::CommandEnd => Cow::Borrowed("`;`, `&` or a line ending"),
        Expected::RedirectTarget => Cow::Borrowed("a file name after the redirection"),
        Expected::HereDocDelimiter => Cow::Borrowed("a delimiter word after `<<`"),
        Expected::CaseWord => Cow::Borrowed("a word after `case`"),
        Expected::CasePattern => Cow::Borrowed("a `case` pattern"),
        Expected::Reserved(word) => Cow::Owned(format!("`{}`", word.as_str())),
        Expected::RightParen => Cow::Borrowed("`)`"),
    }
}

/// Puts source text in backticks for a message. A line ending is called what it is, and long
/// text is cut short.
fn quote(text: &str) -> String {
    if text.is_empty() {
        return "end of input".to_owned();
    }
    if text.trim().is_empty() {
        return "line ending".to_owned();
    }
    let mut shown: String = text.chars().take(QUOTE_LIMIT).collect();
    if text.chars().nth(QUOTE_LIMIT).is_some() {
        shown.push('…');
    }
    format!("`{}`", shown.replace('\n', "\\n"))
}
