//! Lexer, parser, AST, source spans and diagnostics for the shell language.
//!
//! [`parse`] turns a line into the tree in [`ast`], for running it, and fails if anything is
//! wrong. [`parse_tolerant`] does the same as far as it can, for features that work on
//! half-typed input. [`lex`] and [`lex_for_highlight`] are the steps below that: tokens for
//! the parser, and the cheaper path for coloring text as it is typed.

pub mod ast;
mod diagnostic;
mod highlight;
mod lex;
mod parser;
mod scanner;
mod span;
mod token;

pub use diagnostic::SyntaxDiagnostic;
pub use highlight::{HighlightKind, HighlightSpan, Highlighted, lex_for_highlight};
pub use lex::lex;
pub use parser::{
    Expected, ParseError, ParseErrorKind, ParseFailure, Parsed, Unsupported, parse, parse_tolerant,
};
pub use span::{Span, strip_line_continuations};
pub use token::{
    HereDoc, LexError, LexErrorKind, Lexed, Op, ParamOp, Parameter, PartKind, ReservedWord, Token,
    TokenKind, Word, WordPart,
};
