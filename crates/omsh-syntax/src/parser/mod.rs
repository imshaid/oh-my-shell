//! The parser: turns tokens into the tree in [`crate::ast`].
//!
//! It is a recursive-descent parser: one function per grammar rule, and functions call each
//! other in the same way that the rules refer to each other. This file holds the entry
//! points, the token cursor, and the rules for lists and pipelines.
//!
//! Both modes share all of this code. The only difference is that strict mode stops at the
//! first problem, while tolerant mode skips to the next command and carries on.
//!
//! Where `dash` and `bash --posix` differ, the parser follows `dash`, which is `/bin/sh`
//! here, with three exceptions. `` `a;;` `` is an error, where `dash` silently drops everything
//! after `;;`. A line ending is not a `case` pattern, where `dash` accepts one after `(` or `|`.
//! And `12>f` redirects file descriptor 12, as in `bash`, where `dash` takes `12` for a word.
//! The forms that `dash` does not have, such as `&>` and `<<<`, work as in `bash`.

mod command;
mod compound;
mod error;
mod unsupported;
mod word;

pub use error::{Expected, ParseError, ParseErrorKind, ParseFailure, Parsed};
pub use unsupported::Unsupported;

use std::collections::BTreeMap;

use crate::ast::{AndOr, AndOrOp, AndOrPart, List, ListItem, Pipeline};
use crate::lex::lex_range;
use crate::span::Span;
use crate::token::{self, Lexed, Op, ReservedWord, Token, TokenKind, Word};

/// How deeply compound commands, substitutions and quotes may nest. Each level uses stack
/// space, and running out of stack would kill the shell. The lexer's own limit of 64 counts
/// only substitutions and quotes, so this leaves room for a compound command at each of
/// those levels.
pub(crate) const MAX_DEPTH: u32 = 128;

/// Parses `src` to run it. Fails if anything is wrong, so a tree that comes back is
/// complete. Never panics.
///
/// # Errors
///
/// Returns the problems found, which include input that is merely unfinished. Use
/// [`ParseFailure::is_incomplete`] to tell those apart.
pub fn parse(src: &str) -> Result<List, ParseFailure> {
    let (list, errors) = parse_source(src, Mode::Strict);
    if errors.is_empty() {
        Ok(list)
    } else {
        Err(ParseFailure { errors })
    }
}

/// Parses `src` as far as possible, for features that work on half-typed input. It always
/// returns a tree and lists every problem it found. Never panics.
///
/// A construct whose first words are already broken, such as `for` with no name, leaves no
/// node in the tree. An unfinished one keeps what was read: `if a; then b` gives an `if`
/// with one branch.
#[must_use]
pub fn parse_tolerant(src: &str) -> Parsed {
    let (list, errors) = parse_source(src, Mode::Tolerant);
    Parsed { list, errors }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Strict,
    Tolerant,
}

/// Which lexer errors a nested parse should report.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LexErrors {
    Report,
    /// The enclosing lexer already walked this text and reported them.
    AlreadyReported,
}

fn parse_source(src: &str, mode: Mode) -> (List, Vec<ParseError>) {
    let whole = Span::new(0, src.len());
    let (list, mut errors) = parse_range(src, whole, mode, 0, LexErrors::Report);
    errors.sort_by_key(|error| error.span.start);
    (list, errors)
}

/// Lexes and parses `range` of `src` as a whole input.
fn parse_range(
    src: &str,
    range: Span,
    mode: Mode,
    depth: u32,
    lex_errors: LexErrors,
) -> (List, Vec<ParseError>) {
    let lexed = lex_range(src, range);
    let mut errors = Vec::new();
    if lex_errors == LexErrors::Report {
        errors.extend(lexed.errors.iter().copied().map(ParseError::lex));
        if errors.iter().any(ParseError::stops_parsing) {
            return (List::empty(range.start), errors);
        }
    }
    let mut parser = Parser::new(src, &lexed, range, mode, depth);
    let list = parser.parse_list(ListPlace::TopLevel);
    errors.append(&mut parser.errors);
    (list, errors)
}

/// Where a list is, which decides what happens at a word such as `fi`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ListPlace {
    /// Nothing is open, so a closing word is an error.
    TopLevel,
    /// Something is open, and the caller checks that the closing word is the right one.
    Nested,
}

struct Parser<'a> {
    src: &'a str,
    /// The tokens, without comments and here-document bodies.
    tokens: Vec<&'a Token>,
    /// The here-document bodies, by the index in `tokens` of their `<<`.
    here_docs: BTreeMap<usize, &'a token::HereDoc>,
    pos: usize,
    /// Where the input ends.
    end: usize,
    /// The end of the last token consumed, which is where a node ends.
    last_end: usize,
    /// The value of `pos` after the last place where the grammar allows line endings.
    /// If the input runs out right there, typing more could still fix it.
    linebreak_at: Option<usize>,
    errors: Vec<ParseError>,
    mode: Mode,
    depth: u32,
    stopped: bool,
}

impl<'a> Parser<'a> {
    fn new(src: &'a str, lexed: &'a Lexed, range: Span, mode: Mode, depth: u32) -> Self {
        let mut tokens = Vec::with_capacity(lexed.tokens.len());
        let mut bodies = Vec::new();
        for token in &lexed.tokens {
            match &token.kind {
                TokenKind::Comment => {}
                TokenKind::HereDocBody(doc) => bodies.push(doc),
                _ => tokens.push(token),
            }
        }
        // Bodies come in the same order as the `<<` operators that have a delimiter. They
        // are matched up here and not while parsing, so that skipping tokens after an error
        // cannot give a later `<<` the wrong body.
        let mut bodies = bodies.into_iter();
        let mut here_docs = BTreeMap::new();
        for (index, pair) in tokens.windows(2).enumerate() {
            if let [op, delimiter] = pair
                && matches!(op.kind, TokenKind::Op(Op::HereDoc | Op::HereDocStripTabs))
                && matches!(delimiter.kind, TokenKind::Word(_))
                && let Some(body) = bodies.next()
            {
                here_docs.insert(index, body);
            }
        }
        Self {
            src,
            tokens,
            here_docs,
            pos: 0,
            end: range.end,
            last_end: range.start,
            linebreak_at: None,
            errors: Vec::new(),
            mode,
            depth,
            stopped: false,
        }
    }

    fn peek(&self) -> Option<&'a Token> {
        self.tokens.get(self.pos).copied()
    }

    fn peek_op(&self) -> Option<Op> {
        match self.peek()?.kind {
            TokenKind::Op(op) => Some(op),
            _ => None,
        }
    }

    fn peek_word(&self) -> Option<(&'a Word, Span)> {
        let token = self.peek()?;
        match &token.kind {
            TokenKind::Word(word) => Some((word, token.span)),
            _ => None,
        }
    }

    /// The reserved word at the cursor. Only call this where a reserved word can count.
    fn reserved(&self) -> Option<ReservedWord> {
        let (_, span) = self.peek_word()?;
        Word::reserved_word(span, self.src)
    }

    /// Where the next token starts, or where the input ends if there is none.
    fn peek_start(&self) -> usize {
        self.peek().map_or(self.end, |token| token.span.start)
    }

    fn advance(&mut self) {
        if let Some(token) = self.peek() {
            self.pos += 1;
            self.last_end = token.span.end;
        }
    }

    /// Skips line endings. Call this exactly where the grammar allows them, because that is
    /// also what makes running out of input there count as unfinished.
    fn skip_newlines(&mut self) {
        while matches!(
            self.peek(),
            Some(Token {
                kind: TokenKind::Newline,
                ..
            })
        ) {
            self.advance();
        }
        self.linebreak_at = Some(self.pos);
    }

    /// Records a problem. In strict mode, this also ends the parse.
    fn push_error(&mut self, error: ParseError) {
        if self.stopped {
            return;
        }
        // An error can make the callers fail at the same token. Only the first says anything.
        let repeated = self
            .errors
            .last()
            .is_some_and(|last| last.span.start == error.span.start);
        if !repeated {
            self.errors.push(error);
        }
        if self.mode == Mode::Strict || error.stops_parsing() {
            self.stopped = true;
            self.pos = self.tokens.len();
        }
    }

    fn report(&mut self, kind: ParseErrorKind, span: Span) {
        // A lone backslash at the very end is really the start of a line continuation, and
        // whatever comes on the next line may well fit: `{ a; } \` and then `>f`.
        let incomplete = span.end == self.end && span.text(self.src) == "\\";
        self.push_error(ParseError {
            kind,
            span,
            incomplete,
        });
    }

    /// Reports that the token at the cursor, or the end of the input, is not `expected`. Two
    /// touching tokens that spell a bash-only operator are reported as that instead.
    fn unexpected(&mut self, expected: Expected) {
        if let Some(token) = self.peek() {
            if let Some((feature, span)) = self.unsupported_at(token) {
                self.report(ParseErrorKind::Unsupported(feature), span);
            } else {
                self.report(ParseErrorKind::Unexpected { expected }, token.span);
            }
        } else {
            self.push_error(ParseError {
                kind: ParseErrorKind::UnexpectedEnd { expected },
                span: Span::new(self.end, self.end),
                incomplete: self.linebreak_at == Some(self.pos),
            });
        }
    }

    /// Consumes `word` if it is at the cursor, and reports a problem if not.
    fn expect_reserved(&mut self, word: ReservedWord) -> bool {
        if self.reserved() == Some(word) {
            self.advance();
            true
        } else {
            self.unexpected(Expected::Reserved(word));
            false
        }
    }

    /// Consumes `)` if it is at the cursor, and reports a problem if not.
    fn expect_right_paren(&mut self) -> bool {
        if self.peek_op() == Some(Op::RightParen) {
            self.advance();
            true
        } else {
            self.unexpected(Expected::RightParen);
            false
        }
    }

    /// Counts one level of nesting. On the level that is too deep, reports the error, which
    /// ends the parse, and the caller must not go deeper.
    fn enter(&mut self, span: Span) -> bool {
        if self.depth >= MAX_DEPTH {
            self.report(ParseErrorKind::NestingTooDeep, span);
            return false;
        }
        self.depth += 1;
        true
    }

    /// True at the end of the input and at what closes a list. Words count only here, where
    /// a command would start, and not as arguments: `echo fi` is an ordinary command.
    fn at_list_end(&self) -> bool {
        let Some(token) = self.peek() else {
            return true;
        };
        match token.kind {
            TokenKind::Op(Op::RightParen | Op::DoubleSemi) => true,
            _ => matches!(
                self.reserved(),
                Some(
                    ReservedWord::Then
                        | ReservedWord::Elif
                        | ReservedWord::Else
                        | ReservedWord::Fi
                        | ReservedWord::Do
                        | ReservedWord::Done
                        | ReservedWord::Esac
                        | ReservedWord::RightBrace
                )
            ),
        }
    }

    fn at_separator(&self) -> bool {
        matches!(
            self.peek().map(|token| &token.kind),
            Some(TokenKind::Newline | TokenKind::Op(Op::Semi | Op::Amp))
        )
    }

    fn skip_separator(&mut self) {
        if self.at_separator() {
            self.advance();
        }
    }

    /// After a problem, skips to the next command in this list.
    fn recover(&mut self) {
        while self.peek().is_some() && !self.at_separator() {
            if matches!(self.peek_op(), Some(Op::RightParen | Op::DoubleSemi)) {
                return;
            }
            self.advance();
        }
        self.skip_separator();
    }

    fn parse_list(&mut self, place: ListPlace) -> List {
        let mut items = Vec::new();
        loop {
            self.skip_newlines();
            if self.at_list_end() {
                if place == ListPlace::Nested || self.peek().is_none() {
                    break;
                }
                self.unexpected(Expected::Command);
                self.advance();
                self.skip_separator();
                continue;
            }
            let before = self.pos;
            let Some(and_or) = self.parse_and_or() else {
                // Nothing was consumed if this token cannot start a command. It has to go,
                // or this loop would never end. A separator is the token that recovery looks
                // for, so it is left for that.
                if self.pos == before && !self.at_separator() {
                    self.advance();
                }
                self.recover();
                continue;
            };
            let mut background = false;
            let mut end = and_or.span.end;
            match self.peek() {
                Some(Token {
                    kind: TokenKind::Op(op @ (Op::Semi | Op::Amp)),
                    span,
                }) => {
                    background = *op == Op::Amp;
                    end = span.end;
                    self.advance();
                }
                Some(Token {
                    kind: TokenKind::Newline,
                    ..
                })
                | None => {}
                _ if self.at_list_end() => {}
                _ => {
                    self.unexpected(Expected::CommandEnd);
                    self.recover();
                }
            }
            let span = Span::new(and_or.span.start, end);
            items.push(ListItem {
                and_or,
                background,
                span,
            });
        }
        let (Some(first), Some(last)) = (items.first(), items.last()) else {
            return List::empty(self.last_end);
        };
        let span = Span::new(first.span.start, last.span.end);
        List { items, span }
    }

    fn parse_and_or(&mut self) -> Option<AndOr> {
        let start = self.peek_start();
        let first = self.parse_pipeline()?;
        let mut rest = Vec::new();
        loop {
            let op = match self.peek_op() {
                Some(Op::AndIf) => AndOrOp::And,
                Some(Op::OrIf) => AndOrOp::Or,
                _ => break,
            };
            self.advance();
            self.skip_newlines();
            let Some(pipeline) = self.parse_pipeline() else {
                break;
            };
            rest.push(AndOrPart { op, pipeline });
        }
        Some(AndOr {
            first,
            rest,
            span: Span::new(start, self.last_end),
        })
    }

    fn parse_pipeline(&mut self) -> Option<Pipeline> {
        let start = self.peek_start();
        let negated = self.reserved() == Some(ReservedWord::Bang);
        if negated {
            self.advance();
        }
        let mut commands = vec![self.parse_command()?];
        while self.peek_op() == Some(Op::Pipe) {
            self.advance();
            self.skip_newlines();
            let Some(command) = self.parse_command() else {
                break;
            };
            commands.push(command);
        }
        Some(Pipeline {
            negated,
            commands,
            span: Span::new(start, self.last_end),
        })
    }
}

/// True for text that can name a variable or a function.
pub(crate) fn is_name(text: &str) -> bool {
    let mut bytes = text.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}
