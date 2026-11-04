//! The parser's results and error types.

use crate::ast::List;
use crate::span::Span;
use crate::token::{LexError, LexErrorKind, ReservedWord};

/// The result of [`parse_tolerant`](crate::parse_tolerant): as much of a tree as could be
/// built, and every problem found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parsed {
    /// The tree. If there are errors, parts of it are missing, and it must not be run.
    pub list: List,
    /// The problems, ordered by position.
    pub errors: Vec<ParseError>,
}

impl Parsed {
    /// True if more typed text could fix the problems. See [`ParseError::incomplete`].
    #[must_use]
    pub fn is_incomplete(&self) -> bool {
        all_incomplete(&self.errors)
    }
}

/// The error from [`parse`](crate::parse): why a line cannot run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseFailure {
    /// The problems, ordered by position: every problem the lexer found, and the first one
    /// the parser found. Never empty.
    pub errors: Vec<ParseError>,
}

impl ParseFailure {
    /// True if more typed text could fix the problems, so an interactive shell should read
    /// another line instead of reporting an error.
    #[must_use]
    pub fn is_incomplete(&self) -> bool {
        all_incomplete(&self.errors)
    }
}

fn all_incomplete(errors: &[ParseError]) -> bool {
    !errors.is_empty() && errors.iter().all(|error| error.incomplete)
}

/// A problem found while parsing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParseError {
    /// What went wrong.
    pub kind: ParseErrorKind,
    /// Where. Empty and at the end of the input if the input ran out.
    pub span: Span,
    /// True if text typed after the end of the input could fix this: `a |` can go on,
    /// while `a >` cannot, because a line ending is not allowed after `>`. This is false
    /// for anything inside a `$(...)` or backticks that are already closed.
    pub incomplete: bool,
}

impl ParseError {
    pub(super) const fn lex(error: LexError) -> Self {
        Self {
            kind: ParseErrorKind::Lex(error.kind),
            span: error.span,
            incomplete: error.kind.is_incomplete(),
        }
    }

    /// True if nothing more can be parsed after this error.
    pub(super) const fn stops_parsing(&self) -> bool {
        matches!(
            self.kind,
            ParseErrorKind::NestingTooDeep | ParseErrorKind::Lex(LexErrorKind::NestingTooDeep)
        )
    }
}

/// The kinds of parser problem.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseErrorKind {
    /// A problem the lexer found.
    Lex(LexErrorKind),
    /// A token that does not fit here.
    Unexpected {
        /// What would have fit.
        expected: Expected,
    },
    /// The input ended where something else had to come.
    UnexpectedEnd {
        /// What had to come.
        expected: Expected,
    },
    /// A function name that is not a valid name, or that names a special builtin such as
    /// `export`. Both `/bin/sh` and POSIX reject those.
    BadFunctionName,
    /// A `for` loop variable that is missing or is not a valid name.
    BadForName,
    /// Commands nested too deeply. Parsing stops at this point.
    NestingTooDeep,
}

/// What the parser wanted to find.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expected {
    /// A command.
    Command,
    /// The end of a command: `;`, `&`, a line ending or a closing word.
    CommandEnd,
    /// The word after a redirection operator.
    RedirectTarget,
    /// The word after `<<`.
    HereDocDelimiter,
    /// The word after `case`.
    CaseWord,
    /// A pattern in a `case` command.
    CasePattern,
    /// A reserved word such as `then` or `fi`.
    Reserved(ReservedWord),
    /// `)`
    RightParen,
}
