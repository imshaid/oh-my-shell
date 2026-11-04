//! What the scanner tells its sink.

use crate::span::Span;
use crate::token::{LexError, ParamOp, TokenKind};

/// A construct that contains other events and ends with a matching [`Close`].
#[derive(Clone, Copy)]
pub(crate) enum Open {
    Word,
    DoubleQuoted,
    Parameter,
    CommandSub,
    Arithmetic,
    HereDoc,
}

/// A construct with nothing inside it worth reporting.
#[derive(Clone, Copy)]
pub(crate) enum Leaf {
    Literal,
    Escaped,
    LineContinuation,
    SingleQuoted { content: Span },
    AnsiCQuoted { content: Span },
    Backtick { body: Span },
}

/// The end of an [`Open`], with what is only known once the construct is complete.
#[derive(Clone, Copy)]
pub(crate) enum Close {
    Word {
        class: WordClass,
        assignment_name: Option<Span>,
    },
    DoubleQuoted,
    Parameter(ParamInfo),
    CommandSub {
        body: Span,
    },
    Arithmetic {
        body: Span,
    },
    HereDoc(HereDocInfo),
}

/// What a word is in its position. The parser decides for real later; this is the cheap
/// answer that highlighting needs and that finding the end of `$(case ...)` depends on.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum WordClass {
    /// An argument, a pattern or a redirection target.
    Plain,
    /// The word that names the command to run.
    Command,
    /// A reserved word in a position where it counts.
    Keyword,
    /// `FOO=bar` in front of a command.
    Assignment,
    /// The digits in `2>file`.
    IoNumber,
    /// The delimiter after `<<`.
    HereDocDelimiter,
}

/// The parts of a parameter expansion known when it closes.
#[derive(Clone, Copy)]
pub(crate) struct ParamInfo {
    pub(crate) name: Span,
    pub(crate) braced: bool,
    pub(crate) length: bool,
    pub(crate) op: Option<(ParamOp, Span)>,
}

/// The parts of a here-document known when it closes.
#[derive(Clone, Copy)]
pub(crate) struct HereDocInfo {
    pub(crate) body: Span,
    pub(crate) end_marker: Option<Span>,
    pub(crate) quoted: bool,
    pub(crate) strip_tabs: bool,
}

/// Receives what the scanner finds, in source order.
pub(crate) trait Sink {
    /// An operator, line ending or comment.
    fn simple(&mut self, kind: TokenKind, span: Span);
    /// The start of a construct, which `close` ends.
    fn open(&mut self, open: Open, start: usize);
    /// A piece with nothing inside.
    fn leaf(&mut self, leaf: Leaf, span: Span);
    /// The end of the innermost open construct.
    fn close(&mut self, close: Close, end: usize);
    /// A problem.
    fn error(&mut self, error: LexError);
}
