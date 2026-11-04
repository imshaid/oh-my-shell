//! The scanner: the one place that knows the shell's lexical rules.
//!
//! It walks the source once and reports what it finds to a [`Sink`]. [`crate::lex()`] uses
//! a sink that builds a tree of word parts, and [`crate::lex_for_highlight`] uses one that
//! paints flat colored spans. Because both share this code, they cannot disagree about
//! where a word or quote ends.
//!
//! The scanner works on bytes. Everything it looks for is ASCII, and UTF-8 guarantees
//! that ASCII bytes never appear inside a longer character, so every position where it
//! ends a span is a character boundary.

mod events;
mod expansion;
mod heredoc;
mod position;

pub(crate) use events::{Close, Leaf, Open, Sink, WordClass};
use heredoc::{PendingHereDoc, cook_delimiter};
use position::{ListState, assignment_name_len};

use crate::span::{Span, strip_line_continuations};
use crate::token::{LexError, LexErrorKind, Op, TokenKind};

/// How deeply quotes and substitutions may nest. Each level uses stack space, and running
/// out of stack would kill the shell, so hostile or accidental input has to stop here.
const MAX_NESTING: u32 = 64;

/// Scans all of `src` and reports it to `sink`.
pub(crate) fn scan<S: Sink>(src: &str, sink: &mut S) {
    let mut scanner = Scanner {
        src,
        bytes: src.as_bytes(),
        pos: 0,
        limit: src.len(),
        depth: 0,
        aborted: false,
        sink,
    };
    scanner.scan_list(false);
}

/// Where the part scanner is, which decides what is special and what ends the scan.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ctx {
    /// An unquoted word. Ends at a blank or an operator character.
    Word,
    /// Between double quotes. Ends at `"`.
    DoubleQuote,
    /// The word after an operator in `${...}`. Ends at `}`.
    ParamArg {
        /// The expansion itself is inside double quotes or similar.
        in_dq: bool,
        /// Single quotes quote here. Inside double quotes they do so only after `#` and
        /// `%`: `"${x:-it's}"` is fine, while `"${x#'a'}"` strips `a`.
        single_quotes: bool,
    },
    /// A here-document body. Ends at the limit.
    HereDoc { strip_tabs: bool, expand: bool },
    /// Between `$((` and `))`.
    Arithmetic,
}

impl Ctx {
    /// Whether a backslash escapes the byte after it.
    fn escapes(self, next: u8) -> bool {
        let dq_rule = matches!(next, b'$' | b'`' | b'\\');
        match self {
            Self::Word | Self::ParamArg { in_dq: false, .. } => true,
            Self::DoubleQuote => dq_rule || next == b'"',
            Self::ParamArg { in_dq: true, .. } => dq_rule || matches!(next, b'"' | b'}'),
            Self::HereDoc { .. } | Self::Arithmetic => dq_rule,
        }
    }

    fn single_quotes(self) -> bool {
        match self {
            Self::Word => true,
            Self::ParamArg { single_quotes, .. } => single_quotes,
            Self::DoubleQuote | Self::HereDoc { .. } | Self::Arithmetic => false,
        }
    }

    /// Whether `$'...'` is ANSI-C quoting here. Inside double quotes it is plain text.
    fn ansi_c_quotes(self) -> bool {
        matches!(self, Self::Word | Self::ParamArg { in_dq: false, .. })
    }

    fn double_quotes(self) -> bool {
        !matches!(self, Self::HereDoc { .. })
    }

    /// Whether a parameter expansion here counts as being inside double quotes.
    fn in_dq(self) -> bool {
        !matches!(self, Self::Word | Self::ParamArg { in_dq: false, .. })
    }
}

/// True for the bytes that end an unquoted word.
const fn is_word_end(b: u8) -> bool {
    matches!(
        b,
        b' ' | b'\t' | b'\n' | b';' | b'&' | b'|' | b'<' | b'>' | b'(' | b')'
    )
}

const fn is_name_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

const fn is_name_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// True for the characters that are parameters by themselves.
const fn is_special_param(b: u8) -> bool {
    matches!(b, b'?' | b'$' | b'!' | b'#' | b'@' | b'*' | b'-')
}

/// Finds the end of the name in a raw word that starts with `NAME=`, allowing for line
/// continuations inside the name.
fn raw_assignment_name_end(raw: &str) -> Option<usize> {
    let name_len = assignment_name_len(&strip_line_continuations(raw))?;
    let mut seen = 0;
    let mut pos = 0;
    let bytes = raw.as_bytes();
    while seen < name_len {
        if bytes.get(pos) == Some(&b'\\') {
            pos += 2;
        } else {
            pos += 1;
            seen += 1;
        }
    }
    // Continuations between the name and `=` are not part of the name.
    Some(pos)
}

struct Scanner<'a, S> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    /// Scanning stops here. Lower than the input's end only inside a here-document body.
    limit: usize,
    depth: u32,
    /// The nesting limit was hit. Scanning has jumped to the end of the input, and the
    /// constructs still open close quietly as the recursion unwinds.
    aborted: bool,
    sink: &'a mut S,
}

impl<S: Sink> Scanner<'_, S> {
    fn at(&self, pos: usize) -> Option<u8> {
        if pos < self.limit {
            self.bytes.get(pos).copied()
        } else {
            None
        }
    }

    fn peek(&self) -> Option<u8> {
        self.at(self.pos)
    }

    /// Moves `pos` past any line continuations, which count as nothing inside a token.
    fn skip_continuations(&self, mut pos: usize) -> usize {
        while self.at(pos) == Some(b'\\') && self.at(pos + 1) == Some(b'\n') {
            pos += 2;
        }
        pos
    }

    /// If the next byte at `pos`, ignoring continuations, is `b`, returns the position
    /// after it.
    fn follows(&self, pos: usize, b: u8) -> Option<usize> {
        let pos = self.skip_continuations(pos);
        (self.at(pos) == Some(b)).then_some(pos + 1)
    }

    /// The length of the character at `pos`.
    fn char_len(&self, pos: usize) -> usize {
        self.src
            .get(pos..)
            .and_then(|rest| rest.chars().next())
            .map_or(1, char::len_utf8)
    }

    fn error(&mut self, kind: LexErrorKind, start: usize, end: usize) {
        if !self.aborted {
            self.sink.error(LexError {
                kind,
                span: Span::new(start, end),
            });
        }
    }

    /// Counts one level of nesting. On the level that is too deep, reports the error and
    /// stops scanning; the caller must then not open its construct.
    fn enter(&mut self, start: usize) -> bool {
        if self.depth >= MAX_NESTING {
            self.error(LexErrorKind::NestingTooDeep, start, start + 1);
            self.aborted = true;
            self.pos = self.src.len();
            return false;
        }
        self.depth += 1;
        true
    }

    fn flush_literal(&mut self, start: usize) {
        if start < self.pos {
            self.sink.leaf(Leaf::Literal, Span::new(start, self.pos));
        }
    }

    /// Reports a continuation that ends the input, which means more lines are needed.
    fn check_trailing_continuation(&mut self, start: usize) {
        if start + 2 >= self.src.len() {
            self.error(LexErrorKind::TrailingBackslash, start, start + 2);
        }
    }

    fn skip_blanks(&mut self) {
        loop {
            match self.peek() {
                Some(b' ' | b'\t') => self.pos += 1,
                Some(b'\\') if self.at(self.pos + 1) == Some(b'\n') => {
                    self.check_trailing_continuation(self.pos);
                    self.pos += 2;
                }
                _ => return,
            }
        }
    }

    /// Scans a command list. Inside a substitution it stops at the `)` that closes it,
    /// without consuming it, and returns true.
    fn scan_list(&mut self, in_command_sub: bool) -> bool {
        let mut state = ListState::new();
        loop {
            self.skip_blanks();
            let Some(b) = self.peek() else {
                self.abandon_heredocs(&mut state);
                return false;
            };
            match b {
                b'\n' => {
                    let start = self.pos;
                    self.pos += 1;
                    self.sink
                        .simple(TokenKind::Newline, Span::new(start, self.pos));
                    state.after_newline();
                    for doc in std::mem::take(&mut state.pending) {
                        self.scan_heredoc(&doc);
                    }
                }
                b'#' => self.scan_comment(),
                b')' => {
                    let unclaimed = state.close_paren();
                    if unclaimed && in_command_sub {
                        self.abandon_heredocs(&mut state);
                        return true;
                    }
                    let start = self.pos;
                    self.pos += 1;
                    self.sink
                        .simple(TokenKind::Op(Op::RightParen), Span::new(start, self.pos));
                }
                b'(' | b'|' | b'&' | b';' | b'<' | b'>' => self.scan_operator(&mut state),
                _ => self.scan_word(&mut state),
            }
        }
    }

    fn scan_comment(&mut self) {
        let start = self.pos;
        while self.peek().is_some_and(|b| b != b'\n') {
            self.pos += 1;
        }
        self.sink
            .simple(TokenKind::Comment, Span::new(start, self.pos));
    }

    fn scan_operator(&mut self, state: &mut ListState) {
        let start = self.pos;
        let next = start + 1;
        let (op, end) = match self.bytes.get(start).copied() {
            Some(b'|') => match self.follows(next, b'|') {
                Some(end) => (Op::OrIf, end),
                None => (Op::Pipe, next),
            },
            Some(b'&') => {
                if let Some(end) = self.follows(next, b'&') {
                    (Op::AndIf, end)
                } else if let Some(end) = self.follows(next, b'>') {
                    match self.follows(end, b'>') {
                        Some(end) => (Op::AmpDoubleGreat, end),
                        None => (Op::AmpGreat, end),
                    }
                } else {
                    (Op::Amp, next)
                }
            }
            Some(b';') => match self.follows(next, b';') {
                Some(end) => (Op::DoubleSemi, end),
                None => (Op::Semi, next),
            },
            Some(b'<') => {
                if let Some(end) = self.follows(next, b'<') {
                    if let Some(end) = self.follows(end, b'<') {
                        (Op::HereString, end)
                    } else if let Some(end) = self.follows(end, b'-') {
                        (Op::HereDocStripTabs, end)
                    } else {
                        (Op::HereDoc, end)
                    }
                } else if let Some(end) = self.follows(next, b'&') {
                    (Op::LessAmp, end)
                } else if let Some(end) = self.follows(next, b'>') {
                    (Op::LessGreat, end)
                } else {
                    (Op::Less, next)
                }
            }
            Some(b'>') => {
                if let Some(end) = self.follows(next, b'>') {
                    (Op::DoubleGreat, end)
                } else if let Some(end) = self.follows(next, b'&') {
                    (Op::GreatAmp, end)
                } else if let Some(end) = self.follows(next, b'|') {
                    (Op::GreatPipe, end)
                } else {
                    (Op::Great, next)
                }
            }
            _ => (Op::LeftParen, next),
        };
        self.pos = end;
        self.sink.simple(TokenKind::Op(op), Span::new(start, end));
        state.after_operator(op);
    }

    fn scan_word(&mut self, state: &mut ListState) {
        let start = self.pos;
        self.sink.open(Open::Word, start);
        self.scan_parts(Ctx::Word);
        let end = self.pos;

        let raw = Span::new(start, end).text(self.src);
        let text = strip_line_continuations(raw);
        let before_redirect = matches!(self.peek(), Some(b'<' | b'>'));
        let class = state.classify(&text, before_redirect);
        if class == WordClass::HereDocDelimiter {
            let (delimiter, quoted) = cook_delimiter(raw);
            state.pending.push(PendingHereDoc {
                delimiter,
                quoted,
                strip_tabs: state.want_delimiter.take().unwrap_or(false),
                opener: Span::new(start, end),
            });
        }
        let assignment_name = raw_assignment_name_end(raw).map(|len| Span::new(start, start + len));
        self.sink.close(
            Close::Word {
                class,
                assignment_name,
            },
            end,
        );
    }

    /// Scans the pieces of a word or word-like region. Returns true if it stopped at the
    /// byte that ends `ctx`, which it leaves unconsumed, and false if the input ran out.
    fn scan_parts(&mut self, ctx: Ctx) -> bool {
        let mut literal = self.pos;
        let mut parens = 0_u32;
        if let Ctx::HereDoc {
            strip_tabs: true, ..
        } = ctx
        {
            self.skip_tabs();
            literal = self.pos;
        }
        let closed = loop {
            let Some(b) = self.peek() else {
                break false;
            };
            match ctx {
                Ctx::Word if is_word_end(b) => break true,
                Ctx::DoubleQuote if b == b'"' => break true,
                Ctx::ParamArg { .. } if b == b'}' => break true,
                Ctx::Arithmetic if b == b'(' => {
                    parens += 1;
                    self.pos += 1;
                    continue;
                }
                Ctx::Arithmetic if b == b')' => {
                    if parens == 0 && self.follows(self.pos + 1, b')').is_some() {
                        break true;
                    }
                    parens = parens.saturating_sub(1);
                    self.pos += 1;
                    continue;
                }
                Ctx::HereDoc { strip_tabs, .. } if b == b'\n' => {
                    self.pos += 1;
                    if strip_tabs {
                        self.flush_literal(literal);
                        self.skip_tabs();
                        literal = self.pos;
                    }
                    continue;
                }
                Ctx::HereDoc { expand: false, .. } => {
                    self.pos += 1;
                    continue;
                }
                _ => {}
            }
            match b {
                b'\\' => literal = self.scan_backslash(ctx, literal),
                b'\'' if ctx.single_quotes() => {
                    self.flush_literal(literal);
                    self.scan_single_quoted(self.pos, self.pos + 1, Quote::Single);
                    literal = self.pos;
                }
                b'"' if ctx.double_quotes() => {
                    self.flush_literal(literal);
                    self.scan_double_quoted();
                    literal = self.pos;
                }
                b'$' if self.dollar_starts_expansion(ctx) => {
                    self.flush_literal(literal);
                    self.scan_dollar(ctx);
                    literal = self.pos;
                }
                b'`' => {
                    self.flush_literal(literal);
                    self.scan_backtick();
                    literal = self.pos;
                }
                _ => self.pos += 1,
            }
        };
        self.flush_literal(literal);
        closed
    }

    fn skip_tabs(&mut self) {
        while self.peek() == Some(b'\t') {
            self.pos += 1;
        }
    }

    /// Handles a backslash at `pos` and returns where the current literal now starts.
    fn scan_backslash(&mut self, ctx: Ctx, literal: usize) -> usize {
        let start = self.pos;
        match self.at(start + 1) {
            Some(b'\n') => {
                self.flush_literal(literal);
                self.pos = start + 2;
                self.sink
                    .leaf(Leaf::LineContinuation, Span::new(start, self.pos));
                self.check_trailing_continuation(start);
                self.pos
            }
            Some(next) if ctx.escapes(next) => {
                self.flush_literal(literal);
                self.pos = start + 1 + self.char_len(start + 1);
                self.sink.leaf(Leaf::Escaped, Span::new(start, self.pos));
                self.pos
            }
            Some(_) => {
                self.pos += 1;
                literal
            }
            None => {
                self.pos += 1;
                if ctx == Ctx::Word {
                    self.error(LexErrorKind::TrailingBackslash, start, self.pos);
                }
                literal
            }
        }
    }

    /// Scans `'...'` or `$'...'`. `start` is the first byte of the construct and
    /// `content` the first byte after its opening quote.
    fn scan_single_quoted(&mut self, start: usize, content: usize, quote: Quote) {
        self.pos = content;
        let closed = loop {
            match self.peek() {
                None => break false,
                Some(b'\'') => break true,
                Some(b'\\') if quote == Quote::AnsiC => {
                    self.pos += 1;
                    if self.peek().is_some() {
                        self.pos += self.char_len(self.pos);
                    }
                }
                Some(_) => self.pos += 1,
            }
        };
        let content = Span::new(content, self.pos);
        if closed {
            self.pos += 1;
        } else {
            let kind = match quote {
                Quote::Single => LexErrorKind::UnterminatedSingleQuote,
                Quote::AnsiC => LexErrorKind::UnterminatedAnsiCQuote,
            };
            self.error(kind, start, content.start);
        }
        let leaf = match quote {
            Quote::Single => Leaf::SingleQuoted { content },
            Quote::AnsiC => Leaf::AnsiCQuoted { content },
        };
        self.sink.leaf(leaf, Span::new(start, self.pos));
    }

    fn scan_double_quoted(&mut self) {
        let start = self.pos;
        if !self.enter(start) {
            return;
        }
        self.sink.open(Open::DoubleQuoted, start);
        self.pos += 1;
        if self.scan_parts(Ctx::DoubleQuote) {
            self.pos += 1;
        } else {
            self.error(LexErrorKind::UnterminatedDoubleQuote, start, start + 1);
        }
        self.sink.close(Close::DoubleQuoted, self.pos);
        self.depth -= 1;
    }

    fn scan_backtick(&mut self) {
        let start = self.pos;
        self.pos += 1;
        let body_start = self.pos;
        let closed = loop {
            match self.peek() {
                None => break false,
                Some(b'`') => break true,
                Some(b'\\') => {
                    self.pos += 1;
                    if self.peek().is_some() {
                        self.pos += self.char_len(self.pos);
                    }
                }
                Some(_) => self.pos += 1,
            }
        };
        let body = Span::new(body_start, self.pos);
        if closed {
            self.pos += 1;
        } else {
            self.error(LexErrorKind::UnterminatedBacktick, start, body_start);
        }
        self.sink
            .leaf(Leaf::Backtick { body }, Span::new(start, self.pos));
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Quote {
    Single,
    AnsiC,
}
