//! Builds the token list with its tree of word parts.

use crate::scanner::{Close, Leaf, Open, Sink, WordClass, scan};
use crate::span::Span;
use crate::token::{
    HereDoc, LexError, Lexed, Parameter, PartKind, Token, TokenKind, Word, WordPart,
};

/// Splits `src` into tokens.
///
/// This never fails and never panics, so it also works on half-typed input. A caller that
/// is about to run the line must check [`Lexed::errors`].
#[must_use]
pub fn lex(src: &str) -> Lexed {
    let mut sink = TreeSink::default();
    scan(src, &mut sink);
    Lexed {
        tokens: sink.tokens,
        errors: sink.errors,
    }
}

/// A construct still being built.
struct Frame {
    start: usize,
    parts: Vec<WordPart>,
}

#[derive(Default)]
struct TreeSink {
    tokens: Vec<Token>,
    errors: Vec<LexError>,
    frames: Vec<Frame>,
    /// How many `$(` are open. Their contents are not kept, because the parser lexes the
    /// body again by itself, so while this is above zero only errors are recorded.
    command_subs: u32,
    /// Where the outermost open `$(` starts.
    command_sub_start: usize,
}

impl TreeSink {
    fn push_part(&mut self, kind: PartKind, span: Span) {
        if let Some(frame) = self.frames.last_mut() {
            frame.parts.push(WordPart { kind, span });
        }
    }
}

impl Sink for TreeSink {
    fn simple(&mut self, kind: TokenKind, span: Span) {
        if self.command_subs == 0 {
            self.tokens.push(Token { kind, span });
        }
    }

    fn open(&mut self, open: Open, start: usize) {
        if let Open::CommandSub = open {
            if self.command_subs == 0 {
                self.command_sub_start = start;
            }
            self.command_subs += 1;
        } else if self.command_subs == 0 {
            self.frames.push(Frame {
                start,
                parts: Vec::new(),
            });
        }
    }

    fn leaf(&mut self, leaf: Leaf, span: Span) {
        if self.command_subs > 0 {
            return;
        }
        let kind = match leaf {
            Leaf::Literal => PartKind::Literal,
            Leaf::Escaped => PartKind::Escaped,
            Leaf::LineContinuation => PartKind::LineContinuation,
            Leaf::SingleQuoted { content } => PartKind::SingleQuoted { content },
            Leaf::AnsiCQuoted { content } => PartKind::AnsiCQuoted { content },
            Leaf::Backtick { body } => PartKind::Backtick { body },
        };
        self.push_part(kind, span);
    }

    fn close(&mut self, close: Close, end: usize) {
        if self.command_subs > 0 {
            if let Close::CommandSub { body } = close {
                self.command_subs -= 1;
                if self.command_subs == 0 {
                    let span = Span::new(self.command_sub_start, end);
                    self.push_part(PartKind::CommandSub { body }, span);
                }
            }
            return;
        }
        let Some(Frame { start, parts }) = self.frames.pop() else {
            return;
        };
        let span = Span::new(start, end);
        match close {
            Close::Word {
                class,
                assignment_name,
            } => {
                let kind = if class == WordClass::IoNumber {
                    TokenKind::IoNumber
                } else {
                    TokenKind::Word(Word {
                        parts,
                        assignment_name,
                    })
                };
                self.tokens.push(Token { kind, span });
            }
            Close::DoubleQuoted => self.push_part(PartKind::DoubleQuoted { parts }, span),
            Close::Parameter(info) => self.push_part(
                PartKind::Parameter(Parameter {
                    name: info.name,
                    braced: info.braced,
                    length: info.length,
                    op: info.op,
                    arg: parts,
                }),
                span,
            ),
            Close::Arithmetic { body } => {
                self.push_part(PartKind::Arithmetic { body, parts }, span);
            }
            Close::HereDoc(info) => self.tokens.push(Token {
                kind: TokenKind::HereDocBody(HereDoc {
                    body: info.body,
                    end_marker: info.end_marker,
                    quoted: info.quoted,
                    strip_tabs: info.strip_tabs,
                    parts,
                }),
                span,
            }),
            Close::CommandSub { .. } => {}
        }
    }

    fn error(&mut self, error: LexError) {
        self.errors.push(error);
    }
}
