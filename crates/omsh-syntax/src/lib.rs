//! Lexer, parser, AST, source spans and diagnostics for the shell language.
//!
//! Only the lexer exists so far. [`lex()`] turns source text into tokens for the parser, and
//! [`lex_for_highlight`] is the cheaper path for coloring text as it is typed. Both work
//! on unfinished input and report problems in a list instead of failing.

mod highlight;
mod lex;
mod scanner;
mod span;
mod token;

pub use highlight::{HighlightKind, HighlightSpan, Highlighted, lex_for_highlight};
pub use lex::lex;
pub use span::{Span, strip_line_continuations};
pub use token::{
    HereDoc, LexError, LexErrorKind, Lexed, Op, ParamOp, Parameter, PartKind, ReservedWord, Token,
    TokenKind, Word, WordPart,
};
