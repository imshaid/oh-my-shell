//! One command: choosing its kind, simple commands, redirections and function definitions.

use super::{Expected, ParseErrorKind, Parser, is_name};
use crate::ast::{
    Assignment, Command, CommandKind, FunctionDef, HereDoc, PartKind, Redirect, RedirectKind,
    RedirectOp, SimpleCommand, Word, WordPart,
};
use crate::scanner::cook_delimiter;
use crate::span::{Span, strip_line_continuations};
use crate::token::{self, Op, ReservedWord, TokenKind};

/// POSIX's special builtins that are valid names. A function cannot have these names.
const SPECIAL_BUILTINS: [&str; 13] = [
    "break", "continue", "eval", "exec", "exit", "export", "readonly", "return", "set", "shift",
    "times", "trap", "unset",
];

/// Reads a file descriptor number, which has to fit in a C `int`. Anything larger is not a
/// file descriptor but an ordinary word, in both `dash` and `bash`.
fn fd_number(text: &str) -> Option<u32> {
    let number: i32 = strip_line_continuations(text).parse().ok()?;
    u32::try_from(number).ok()
}

impl Parser<'_> {
    /// Parses a command. On a token that cannot start one, reports it and returns nothing,
    /// without consuming it.
    pub(super) fn parse_command(&mut self) -> Option<Command> {
        let Some(token) = self.peek() else {
            self.unexpected(Expected::Command);
            return None;
        };
        match &token.kind {
            TokenKind::Word(word) => match self.reserved() {
                Some(
                    ReservedWord::If
                    | ReservedWord::While
                    | ReservedWord::Until
                    | ReservedWord::For
                    | ReservedWord::Case
                    | ReservedWord::LeftBrace,
                ) => self.parse_compound(),
                Some(_) => {
                    self.unexpected(Expected::Command);
                    None
                }
                None => {
                    let next = self.tokens.get(self.pos + 1).map(|next| &next.kind);
                    let before_paren = matches!(next, Some(TokenKind::Op(Op::LeftParen)));
                    if before_paren && word.assignment_name.is_none() {
                        self.parse_function()
                    } else {
                        Some(self.parse_simple())
                    }
                }
            },
            TokenKind::Op(Op::LeftParen) => self.parse_compound(),
            TokenKind::IoNumber => Some(self.parse_simple()),
            TokenKind::Op(op) if op.is_redirect() => Some(self.parse_simple()),
            _ => {
                self.unexpected(Expected::Command);
                None
            }
        }
    }

    /// Parses `NAME=value` words, command words and redirections in any mix. A word stops
    /// counting as an assignment once a command word has been seen.
    fn parse_simple(&mut self) -> Command {
        let start = self.peek_start();
        let mut assignments = Vec::new();
        let mut words = Vec::new();
        let mut redirects = Vec::new();
        while let Some(token) = self.peek() {
            match &token.kind {
                TokenKind::Word(word) => {
                    self.advance();
                    match word.assignment_name {
                        Some(name) if words.is_empty() => {
                            assignments.push(self.assignment(word, name, token.span));
                        }
                        _ => words.push(self.word(word, token.span)),
                    }
                }
                TokenKind::IoNumber if fd_number(token.span.text(self.src)).is_none() => {
                    self.advance();
                    let text = strip_line_continuations(token.span.text(self.src));
                    words.push(Word {
                        parts: vec![WordPart {
                            kind: PartKind::Literal(text.into_owned()),
                            span: token.span,
                        }],
                        span: token.span,
                    });
                }
                TokenKind::IoNumber => redirects.extend(self.parse_redirect()),
                TokenKind::Op(op) if op.is_redirect() => redirects.extend(self.parse_redirect()),
                _ => break,
            }
        }
        Command {
            kind: CommandKind::Simple(SimpleCommand { assignments, words }),
            redirects,
            span: Span::new(start, self.last_end),
        }
    }

    /// Parses `name ( ) command`. The cursor is on the name, and `(` follows it.
    fn parse_function(&mut self) -> Option<Command> {
        let (_, name_span) = self.peek_word()?;
        if !self.enter(name_span) {
            return None;
        }
        let function = self.parse_function_inner(name_span);
        self.depth -= 1;
        function
    }

    fn parse_function_inner(&mut self, name_span: Span) -> Option<Command> {
        self.advance();
        self.advance();
        if !self.expect_right_paren() {
            return None;
        }
        let name = strip_line_continuations(name_span.text(self.src)).into_owned();
        if !is_name(&name) || SPECIAL_BUILTINS.contains(&name.as_str()) {
            self.report(ParseErrorKind::BadFunctionName, name_span);
        }
        self.skip_newlines();
        let body = self.parse_command()?;
        Some(Command {
            kind: CommandKind::Function(FunctionDef {
                name,
                name_span,
                body: Box::new(body),
            }),
            redirects: Vec::new(),
            span: Span::new(name_span.start, self.last_end),
        })
    }

    /// Parses the redirections after a compound command.
    pub(super) fn parse_trailing_redirects(&mut self) -> Vec<Redirect> {
        let mut redirects = Vec::new();
        while let Some(token) = self.peek() {
            let is_redirect = match token.kind {
                TokenKind::IoNumber => fd_number(token.span.text(self.src)).is_some(),
                TokenKind::Op(op) => op.is_redirect(),
                _ => false,
            };
            if !is_redirect {
                break;
            }
            redirects.extend(self.parse_redirect());
        }
        redirects
    }

    /// Parses a redirection. If its word is missing, the operator is consumed, the problem
    /// is reported, and nothing is returned.
    fn parse_redirect(&mut self) -> Option<Redirect> {
        let start = self.peek_start();
        let token = self.peek()?;
        let fd = if matches!(token.kind, TokenKind::IoNumber) {
            self.advance();
            fd_number(token.span.text(self.src))
        } else {
            None
        };
        let op = self.peek_op()?;
        let op_index = self.pos;
        self.advance();

        let op = match op {
            Op::Less => RedirectOp::Input,
            Op::Great => RedirectOp::Output,
            Op::DoubleGreat => RedirectOp::Append,
            Op::LessGreat => RedirectOp::ReadWrite,
            Op::GreatPipe => RedirectOp::Clobber,
            Op::LessAmp => RedirectOp::DupInput,
            Op::GreatAmp => RedirectOp::DupOutput,
            Op::AmpGreat => RedirectOp::OutputAndError,
            Op::AmpDoubleGreat => RedirectOp::AppendAndError,
            Op::HereString => RedirectOp::HereString,
            Op::HereDoc | Op::HereDocStripTabs => {
                return self.parse_here_doc(start, fd, op_index);
            }
            _ => return None,
        };
        let Some((word, span)) = self.peek_word() else {
            self.unexpected(Expected::RedirectTarget);
            return None;
        };
        self.advance();
        Some(Redirect {
            fd,
            kind: RedirectKind::Target {
                op,
                word: self.word(word, span),
            },
            span: Span::new(start, span.end),
        })
    }

    /// Parses the rest of `<<` or `<<-`, whose operator at `op_index` is already consumed.
    fn parse_here_doc(
        &mut self,
        start: usize,
        fd: Option<u32>,
        op_index: usize,
    ) -> Option<Redirect> {
        let Some((_, delimiter)) = self.peek_word() else {
            self.unexpected(Expected::HereDocDelimiter);
            return None;
        };
        self.advance();
        // The lexer gives every `<<` that has a delimiter a body.
        let doc = self.here_docs.get(&op_index).copied()?;
        let doc = HereDoc {
            delimiter: cook_delimiter(delimiter.text(self.src)).0,
            quoted: doc.quoted,
            strip_tabs: doc.strip_tabs,
            // `dash` treats the inside of a here-document like the inside of double quotes
            // when it comes to backslashes in backticks.
            body: self.parts(&doc.parts, true),
            body_span: doc.body,
        };
        Some(Redirect {
            fd,
            kind: RedirectKind::HereDoc(Box::new(doc)),
            span: Span::new(start, delimiter.end),
        })
    }

    /// Builds an assignment from a word that starts with `NAME=`. `name` is the name's span.
    fn assignment(&mut self, word: &token::Word, name: Span, span: Span) -> Assignment {
        // Only line continuations can come between the name and the `=`.
        let value_start = Span::new(name.end, span.end)
            .text(self.src)
            .find('=')
            .map_or(span.end, |offset| name.end + offset + 1);
        Assignment {
            name: strip_line_continuations(name.text(self.src)).into_owned(),
            name_span: name,
            value: self.word_from(word, value_start, span.end),
            span,
        }
    }
}
