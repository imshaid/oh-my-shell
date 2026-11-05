//! Turns the lexer's words into tree words, which means parsing every substitution in them.

use super::{LexErrors, ParseError, ParseErrorKind, Parser, parse_range};
use crate::ast::{self, List, Parameter, PartKind, Word};
use crate::span::{Span, strip_line_continuations};
use crate::token::{self, LexErrorKind};

/// Adds text to the literal piece at the end of `out`, or starts a new one. This is what
/// puts `a\<newline>b` back together.
fn push_literal(out: &mut Vec<ast::WordPart>, text: &str, span: Span) {
    if let Some(ast::WordPart {
        kind: PartKind::Literal(last),
        span: last_span,
    }) = out.last_mut()
    {
        last.push_str(text);
        last_span.end = span.end;
    } else {
        out.push(ast::WordPart {
            kind: PartKind::Literal(text.to_owned()),
            span,
        });
    }
}

impl Parser<'_> {
    pub(super) fn word(&mut self, word: &token::Word, span: Span) -> Word {
        Word {
            parts: self.parts(&word.parts, false),
            span,
        }
    }

    /// Builds a word from the part of `word` that lies in `start..end`. `start` may fall
    /// inside a literal piece, but only there.
    pub(super) fn word_from(&mut self, word: &token::Word, start: usize, end: usize) -> Word {
        let mut parts = Vec::new();
        for part in &word.parts {
            if part.span.end <= start {
                continue;
            }
            if part.span.start < start {
                self.literal(Span::new(start, part.span.end), false, &mut parts);
            } else {
                self.part(part, false, &mut parts);
            }
        }
        Word {
            parts,
            span: Span::new(start, end),
        }
    }

    /// Converts pieces. `in_dq` says whether they count as being inside double quotes, which
    /// changes which backslashes a backtick body loses.
    pub(super) fn parts(&mut self, parts: &[token::WordPart], in_dq: bool) -> Vec<ast::WordPart> {
        let mut out = Vec::with_capacity(parts.len());
        for part in parts {
            self.part(part, in_dq, &mut out);
        }
        out
    }

    /// Adds the literal text at `span`. In an unquoted word, the lexer has made every
    /// backslash an escape, except for one at the very end of the input, which means itself.
    /// It is turned into an escape here too, so that an unquoted literal never holds a
    /// backslash, which is what expansion relies on.
    fn literal(&self, span: Span, in_dq: bool, out: &mut Vec<ast::WordPart>) {
        let text = span.text(self.src);
        match text.strip_suffix('\\') {
            Some(rest) if !in_dq => {
                let backslash = span.end - 1;
                if !rest.is_empty() {
                    push_literal(out, rest, Span::new(span.start, backslash));
                }
                out.push(ast::WordPart {
                    kind: PartKind::Escaped('\\'),
                    span: Span::new(backslash, span.end),
                });
            }
            _ => push_literal(out, text, span),
        }
    }

    fn nested_parts(
        &mut self,
        parts: &[token::WordPart],
        in_dq: bool,
        span: Span,
    ) -> Vec<ast::WordPart> {
        if !self.enter(span) {
            return Vec::new();
        }
        let out = self.parts(parts, in_dq);
        self.depth -= 1;
        out
    }

    fn part(&mut self, part: &token::WordPart, in_dq: bool, out: &mut Vec<ast::WordPart>) {
        let span = part.span;
        let kind = match &part.kind {
            token::PartKind::Literal => {
                self.literal(span, in_dq, out);
                return;
            }
            token::PartKind::LineContinuation => return,
            token::PartKind::Escaped => {
                // The piece is a backslash and the character after it.
                match span.text(self.src).chars().nth(1) {
                    Some(c) => PartKind::Escaped(c),
                    None => return,
                }
            }
            token::PartKind::SingleQuoted { content } => {
                PartKind::SingleQuoted(content.text(self.src).to_owned())
            }
            token::PartKind::AnsiCQuoted { content } => {
                PartKind::AnsiCQuoted(content.text(self.src).to_owned())
            }
            token::PartKind::DoubleQuoted { parts } => {
                PartKind::DoubleQuoted(self.nested_parts(parts, true, span))
            }
            token::PartKind::Parameter(parameter) => {
                self.check_parameter(parameter, span);
                PartKind::Parameter(Box::new(Parameter {
                    name: strip_line_continuations(parameter.name.text(self.src)).into_owned(),
                    name_span: parameter.name,
                    length: parameter.length,
                    op: parameter.op,
                    arg: self.nested_parts(&parameter.arg, in_dq, span),
                }))
            }
            // `dash` treats arithmetic like the inside of double quotes when it comes to
            // backslashes in backticks.
            token::PartKind::Arithmetic { parts, .. } => {
                PartKind::Arithmetic(self.nested_parts(parts, true, span))
            }
            token::PartKind::CommandSub { body } => {
                PartKind::CommandSub(self.command_sub(span, *body))
            }
            token::PartKind::Backtick { body } => {
                PartKind::CommandSub(self.backtick(span, *body, in_dq))
            }
        };
        out.push(ast::WordPart { kind, span });
    }

    /// Parses the commands in `$(...)`. The lexer walked this text as part of the outer
    /// input, so it has already reported any lexer errors here.
    fn command_sub(&mut self, span: Span, body: Span) -> List {
        if self.stopped || !self.enter(span) {
            return List::empty(body.start);
        }
        let (list, errors) = parse_range(
            self.src,
            body,
            self.mode,
            self.depth,
            LexErrors::AlreadyReported,
        );
        self.depth -= 1;
        self.adopt_errors(errors, span.end > body.end);
        self.check_file_contents(&list, span);
        list
    }

    /// Parses the commands in `` `...` ``. The lexer never looked inside those, so errors
    /// here are all new. The body is parsed as a separate text with one level of backslashes
    /// removed, and then its spans are moved back to the source.
    fn backtick(&mut self, span: Span, body: Span, in_dq: bool) -> List {
        if self.stopped || !self.enter(span) {
            return List::empty(body.start);
        }
        let cooked = CookedBacktick::new(body, body.text(self.src), in_dq);
        let whole = Span::new(0, cooked.text.len());
        let (mut list, mut errors) = parse_range(
            &cooked.text,
            whole,
            self.mode,
            self.depth,
            LexErrors::Report,
        );
        self.depth -= 1;
        list.map_spans(&mut |span| cooked.to_source(span));
        for error in &mut errors {
            error.span = cooked.to_source(error.span);
        }
        // A closed body is a whole script, and a script may end in the middle of these:
        // `/bin/sh` ends the here-document there, and keeps the backslash as text.
        let closed = span.end > body.end;
        errors.retain(|error| {
            !(closed
                && matches!(
                    error.kind,
                    ParseErrorKind::Lex(
                        LexErrorKind::UnterminatedHereDoc | LexErrorKind::TrailingBackslash
                    )
                ))
        });
        self.adopt_errors(errors, closed);
        self.check_file_contents(&list, span);
        list
    }

    /// Takes over the errors of a nested parse. If the substitution is closed, text typed
    /// later cannot reach inside it. If it is open, the lexer has already reported that, so
    /// the nested errors that only mean "unfinished" add nothing.
    fn adopt_errors(&mut self, errors: Vec<ParseError>, closed: bool) {
        for mut error in errors {
            if closed {
                error.incomplete = false;
            } else if error.incomplete {
                continue;
            }
            self.push_error(error);
        }
    }
}

/// A backtick body with one level of backslashes removed, and a way back to the source.
struct CookedBacktick {
    text: String,
    /// For each byte of `text`, its position in the source.
    origin: Vec<usize>,
    /// Where the body starts in the source.
    start: usize,
}

impl CookedBacktick {
    /// A backslash disappears before `$`, `` ` `` and `\`, and also before `"` if the
    /// backticks are inside double quotes. A backslash and a line ending disappear together,
    /// even inside single quotes, because all of this happens before the body is lexed.
    fn new(body: Span, source: &str, in_dq: bool) -> Self {
        let bytes = source.as_bytes();
        let mut cooked = Self {
            text: String::with_capacity(source.len()),
            origin: Vec::with_capacity(source.len()),
            start: body.start,
        };
        let mut copied = 0;
        let mut pos = 0;
        while let Some(&byte) = bytes.get(pos) {
            let next = bytes.get(pos + 1).copied();
            let dropped = match next {
                _ if byte != b'\\' => 0,
                Some(b'\n') => 2,
                Some(b'$' | b'`' | b'\\') => 1,
                Some(b'"') if in_dq => 1,
                _ => 0,
            };
            if dropped == 0 {
                pos += 1;
                continue;
            }
            cooked.copy(source, body.start, copied, pos);
            copied = pos + dropped;
            // The character a backslash protects is copied as it is and never looked at
            // again, so `\\$x` is a backslash and then an expansion.
            pos += 2;
        }
        cooked.copy(source, body.start, copied, source.len());
        cooked
    }

    fn copy(&mut self, source: &str, base: usize, from: usize, to: usize) {
        self.text.push_str(source.get(from..to).unwrap_or(""));
        self.origin.extend((from..to).map(|offset| base + offset));
    }

    /// The position in the source just after the byte of `text` before `pos`.
    fn after_byte_before(&self, pos: usize) -> usize {
        pos.checked_sub(1)
            .and_then(|last| self.origin.get(last))
            .map_or(self.start, |origin| origin + 1)
    }

    /// Moves a span from `text` to the source. Its end, and all of an empty span, stay next to
    /// the byte before them, so that a removed backslash right after the span stays outside.
    fn to_source(&self, span: Span) -> Span {
        let end = self.after_byte_before(span.end);
        let start = if span.start < span.end {
            self.origin.get(span.start).copied().unwrap_or(end)
        } else {
            end
        };
        Span::new(start, end)
    }
}
