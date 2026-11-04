//! The cheap path for syntax highlighting: flat colored spans and no tree.
//!
//! The editor calls this on every keystroke, on text that is usually unfinished. It keeps
//! one list of spans, plus a small stack for the constructs currently open.

use crate::scanner::{Close, Leaf, Open, Sink, WordClass, scan_range};
use crate::span::Span;
use crate::token::{LexError, TokenKind};

/// The roles a piece of the command line can have.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HighlightKind {
    /// The word naming the command to run.
    Command,
    /// A reserved word in a position where it counts.
    Keyword,
    /// An argument, pattern or redirection target.
    Argument,
    /// `FOO=` in front of a command.
    Assignment,
    /// `|`, `&&`, `;`, `(` and the like.
    Operator,
    /// `>`, `2>&`, `<<` and the like.
    Redirect,
    /// The digits in `2>file`.
    IoNumber,
    /// A comment.
    Comment,
    /// `'...'` or `$'...'`.
    SingleQuoted,
    /// `"..."`, except for the expansions inside.
    DoubleQuoted,
    /// A backslash escape or line continuation.
    Escape,
    /// A parameter expansion, except for the word inside `${x:-word}`.
    Variable,
    /// The `$(`, `)` and blanks of a substitution, or a whole `` `...` ``. Commands inside
    /// `$(...)` get their own spans.
    CommandSub,
    /// Arithmetic, except for the expansions inside.
    Arithmetic,
    /// The word after `<<`, or the line that closes a here-document.
    HereDocDelimiter,
    /// The text of a here-document.
    HereDocBody,
}

/// One colored span.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HighlightSpan {
    /// The piece's role.
    pub kind: HighlightKind,
    /// Where it is.
    pub span: Span,
}

/// The result of [`lex_for_highlight`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Highlighted {
    /// Spans in source order, never overlapping. Blanks between tokens have no span.
    pub spans: Vec<HighlightSpan>,
    /// Problems, so that the editor can mark them on top of the spans.
    pub errors: Vec<LexError>,
}

/// Finds the highlighting for `src` without building a token list or an AST.
///
/// This never fails and never panics, so half-typed input is fine.
#[must_use]
pub fn lex_for_highlight(src: &str) -> Highlighted {
    let mut sink = PaintSink::default();
    scan_range(src, Span::new(0, src.len()), &mut sink);
    Highlighted {
        spans: sink
            .pieces
            .into_iter()
            .map(|piece| HighlightSpan {
                kind: piece.kind.unwrap_or(HighlightKind::Argument),
                span: piece.span,
            })
            .collect(),
        errors: sink.errors,
    }
}

/// A span whose kind is `None` while its word is still open, because a word's role is
/// only known once the whole word has been read.
struct Piece {
    kind: Option<HighlightKind>,
    span: Span,
}

/// An open construct.
struct Frame {
    /// The kind of plain text directly inside this construct.
    kind: Option<HighlightKind>,
    /// The number of pieces when the construct opened.
    first_piece: usize,
}

#[derive(Default)]
struct PaintSink {
    pieces: Vec<Piece>,
    errors: Vec<LexError>,
    frames: Vec<Frame>,
    /// Everything before this position has been painted or skipped.
    cursor: usize,
}

impl PaintSink {
    fn paint(&mut self, start: usize, end: usize, kind: Option<HighlightKind>) {
        if start >= end {
            return;
        }
        if let Some(last) = self.pieces.last_mut()
            && last.kind == kind
            && last.span.end == start
        {
            last.span.end = end;
            return;
        }
        self.pieces.push(Piece {
            kind,
            span: Span::new(start, end),
        });
    }

    /// Paints everything from the cursor up to `pos` as the innermost construct's plain
    /// text. At the top level, what lies between tokens is only blanks.
    fn advance_to(&mut self, pos: usize) {
        if let Some(kind) = self.frames.last().map(|frame| frame.kind) {
            self.paint(self.cursor, pos, kind);
        }
        self.cursor = pos;
    }

    /// Gives the word's own pieces their final kind. The pieces of words nested inside
    /// this one have a kind by now, so only this word's pieces still have none.
    fn finish_word(&mut self, first: usize, kind: HighlightKind, assignment: Option<Span>) {
        if let Some(name) = assignment {
            self.split_assignment(first, name);
        }
        for piece in self.pieces.iter_mut().skip(first) {
            if piece.kind.is_none() {
                piece.kind = Some(kind);
            }
        }
    }

    /// Marks the `FOO=` at the start of a word. If a line continuation splits it, this
    /// leaves the word alone, and the word is colored as a plain argument.
    fn split_assignment(&mut self, first: usize, name: Span) {
        let equals_end = name.end + 1;
        let Some(piece) = self.pieces.get_mut(first) else {
            return;
        };
        if piece.kind.is_some() || piece.span.start != name.start || piece.span.end < equals_end {
            return;
        }
        let rest_end = piece.span.end;
        piece.kind = Some(HighlightKind::Assignment);
        piece.span.end = equals_end;
        if equals_end < rest_end {
            self.pieces.insert(
                first + 1,
                Piece {
                    kind: None,
                    span: Span::new(equals_end, rest_end),
                },
            );
        }
    }
}

impl Sink for PaintSink {
    fn simple(&mut self, kind: TokenKind, span: Span) {
        self.advance_to(span.start);
        let paint_kind = match kind {
            TokenKind::Op(op) if op.is_redirect() => HighlightKind::Redirect,
            TokenKind::Op(_) => HighlightKind::Operator,
            TokenKind::Comment => HighlightKind::Comment,
            // A line ending has no color of its own. Leaving the cursor before it lets
            // an enclosing `$(...)` color it, like any other blank inside.
            _ => return,
        };
        self.paint(span.start, span.end, Some(paint_kind));
        self.cursor = span.end;
    }

    fn open(&mut self, open: Open, start: usize) {
        self.advance_to(start);
        let kind = match open {
            Open::Word => None,
            Open::DoubleQuoted => Some(HighlightKind::DoubleQuoted),
            Open::Parameter => Some(HighlightKind::Variable),
            Open::CommandSub => Some(HighlightKind::CommandSub),
            Open::Arithmetic => Some(HighlightKind::Arithmetic),
            Open::HereDoc => Some(HighlightKind::HereDocBody),
        };
        self.frames.push(Frame {
            kind,
            first_piece: self.pieces.len(),
        });
    }

    fn leaf(&mut self, leaf: Leaf, span: Span) {
        self.advance_to(span.start);
        let kind = match leaf {
            Leaf::Literal => self.frames.last().and_then(|frame| frame.kind),
            Leaf::Escaped | Leaf::LineContinuation => Some(HighlightKind::Escape),
            Leaf::SingleQuoted { .. } | Leaf::AnsiCQuoted { .. } => {
                Some(HighlightKind::SingleQuoted)
            }
            Leaf::Backtick { .. } => Some(HighlightKind::CommandSub),
        };
        self.paint(span.start, span.end, kind);
        self.cursor = span.end;
    }

    fn close(&mut self, close: Close, end: usize) {
        if let Close::HereDoc(info) = close
            && let Some(marker) = info.end_marker
        {
            self.advance_to(marker.start);
            self.paint(marker.start, end, Some(HighlightKind::HereDocDelimiter));
            self.cursor = end;
        } else {
            self.advance_to(end);
        }
        let Some(frame) = self.frames.pop() else {
            return;
        };
        if let Close::Word {
            class,
            assignment_name,
        } = close
        {
            let kind = match class {
                WordClass::Plain | WordClass::Assignment => HighlightKind::Argument,
                WordClass::Command => HighlightKind::Command,
                WordClass::Keyword => HighlightKind::Keyword,
                WordClass::IoNumber => HighlightKind::IoNumber,
                WordClass::HereDocDelimiter => HighlightKind::HereDocDelimiter,
            };
            let assignment = assignment_name.filter(|_| class == WordClass::Assignment);
            self.finish_word(frame.first_piece, kind, assignment);
        }
    }

    fn error(&mut self, error: LexError) {
        self.errors.push(error);
    }
}
