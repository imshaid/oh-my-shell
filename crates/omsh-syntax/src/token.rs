//! The lexer's output types.

use crate::span::{Span, strip_line_continuations};

/// The result of [`lex`](crate::lex()). Lexing always finishes; problems are listed in `errors`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lexed {
    /// Every token in source order. Blanks are not tokens; comments are.
    pub tokens: Vec<Token>,
    /// Every problem found, in the order the scanner finished with it.
    pub errors: Vec<LexError>,
}

impl Lexed {
    /// True if more typed text could fix the problems, so an interactive shell should
    /// read another line instead of reporting an error.
    #[must_use]
    pub fn is_incomplete(&self) -> bool {
        !self.errors.is_empty() && self.errors.iter().all(|e| e.kind.is_incomplete())
    }
}

/// One token with its position in the source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    /// What the token is.
    pub kind: TokenKind,
    /// Where it is.
    pub span: Span,
}

/// The kinds of token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TokenKind {
    /// A word. Reserved words such as `if` and `{` are words too, because only the parser
    /// knows the position that makes them reserved. See [`Word::reserved_word`].
    Word(Word),
    /// The digits in `2>file`: only digits, directly followed by `<` or `>`. The span can
    /// contain line continuations; see [`strip_line_continuations`].
    IoNumber,
    /// An operator.
    Op(Op),
    /// An unquoted line ending.
    Newline,
    /// A comment, from `#` up to but not including the line ending.
    Comment,
    /// The body of a here-document, emitted just after the line ending that starts it.
    /// Bodies come in the same order as their `<<` operators.
    HereDocBody(HereDoc),
}

/// The operators.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    /// `|`
    Pipe,
    /// `&&`
    AndIf,
    /// `||`
    OrIf,
    /// `;`
    Semi,
    /// `;;`
    DoubleSemi,
    /// `&`
    Amp,
    /// `(`
    LeftParen,
    /// `)`
    RightParen,
    /// `<`
    Less,
    /// `>`
    Great,
    /// `>>`
    DoubleGreat,
    /// `<&`
    LessAmp,
    /// `>&`
    GreatAmp,
    /// `&>`
    AmpGreat,
    /// `&>>`
    AmpDoubleGreat,
    /// `<<`
    HereDoc,
    /// `<<-`
    HereDocStripTabs,
    /// `<<<`
    HereString,
    /// `<>`
    LessGreat,
    /// `>|`
    GreatPipe,
}

impl Op {
    /// True for the operators that redirect a file descriptor.
    #[must_use]
    pub const fn is_redirect(self) -> bool {
        matches!(
            self,
            Self::Less
                | Self::Great
                | Self::DoubleGreat
                | Self::LessAmp
                | Self::GreatAmp
                | Self::AmpGreat
                | Self::AmpDoubleGreat
                | Self::HereDoc
                | Self::HereDocStripTabs
                | Self::HereString
                | Self::LessGreat
                | Self::GreatPipe
        )
    }
}

/// A word and the pieces it is made of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Word {
    /// The pieces, in order. Together they cover the whole word.
    pub parts: Vec<WordPart>,
    /// The `FOO` in `FOO=bar`, when the word starts with an unquoted name and `=`.
    /// Only the parser knows whether the word really is an assignment. The span can
    /// contain line continuations; see [`strip_line_continuations`].
    pub assignment_name: Option<Span>,
}

impl Word {
    /// Returns the reserved word this word spells, if any. `span` is the word's token span.
    ///
    /// A quoted word such as `"if"` never matches, because its text includes the quotes.
    #[must_use]
    pub fn reserved_word(span: Span, src: &str) -> Option<ReservedWord> {
        ReservedWord::from_text(&strip_line_continuations(span.text(src)))
    }
}

/// One piece of a word, a double-quoted string, a here-document or similar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WordPart {
    /// What the piece is.
    pub kind: PartKind,
    /// Where it is, including any quotes or `$` around it.
    pub span: Span,
}

/// The kinds of word piece.
///
/// Tildes and glob characters stay inside [`PartKind::Literal`]. Whether they are special
/// depends on their position and on neighbouring pieces, which expansion decides.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PartKind {
    /// Text with no special meaning here.
    Literal,
    /// A backslash and the character it escapes.
    Escaped,
    /// A backslash and a line ending, which together mean nothing.
    LineContinuation,
    /// `'...'`.
    SingleQuoted {
        /// The text between the quotes.
        content: Span,
    },
    /// `$'...'`. Escape sequences in it are decoded later.
    AnsiCQuoted {
        /// The text between the quotes, undecoded.
        content: Span,
    },
    /// `"..."`.
    DoubleQuoted {
        /// The pieces between the quotes.
        parts: Vec<WordPart>,
    },
    /// `$NAME`, `${NAME}`, `${#NAME}` or `${NAME:-word}` and similar.
    Parameter(Parameter),
    /// `$(...)`.
    CommandSub {
        /// The text between the parentheses, which the parser lexes again by itself.
        body: Span,
    },
    /// `` `...` ``.
    Backtick {
        /// The text between the backticks, with its backslashes not yet removed.
        body: Span,
    },
    /// `$((...))`.
    Arithmetic {
        /// The text between the parentheses.
        body: Span,
        /// The pieces of that text.
        parts: Vec<WordPart>,
    },
}

/// A parameter expansion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parameter {
    /// The parameter's name, number or special character. Empty for a malformed `${}`.
    /// The span can contain line continuations; see [`strip_line_continuations`].
    pub name: Span,
    /// True for the `${...}` spelling.
    pub braced: bool,
    /// True for `${#NAME}`, the length of the value.
    pub length: bool,
    /// The operator and where it is. Absent for plain `$NAME` and `${NAME}`.
    pub op: Option<(ParamOp, Span)>,
    /// The pieces of the word after the operator.
    pub arg: Vec<WordPart>,
}

/// The operators inside `${...}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamOp {
    /// `:-`
    ColonDash,
    /// `:=`
    ColonEquals,
    /// `:?`
    ColonQuestion,
    /// `:+`
    ColonPlus,
    /// `-`
    Dash,
    /// `=`
    Equals,
    /// `?`
    Question,
    /// `+`
    Plus,
    /// `#`
    Hash,
    /// `##`
    DoubleHash,
    /// `%`
    Percent,
    /// `%%`
    DoublePercent,
    /// Any other form, such as `${x//a/b}`, `${x:1:2}` or `${#x:-y}`. The lexer still
    /// finds the closing brace, so that a later stage can name the whole expression.
    Unsupported,
}

/// A here-document body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HereDoc {
    /// The body text, without the line holding the closing delimiter.
    pub body: Span,
    /// The line holding the closing delimiter, without its line ending. Absent if the
    /// input ended first.
    pub end_marker: Option<Span>,
    /// True if any part of the delimiter was quoted, which turns off expansion.
    pub quoted: bool,
    /// True for `<<-`. The tabs it strips are already left out of `parts`.
    pub strip_tabs: bool,
    /// The pieces of the body.
    pub parts: Vec<WordPart>,
}

/// A problem the lexer found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LexError {
    /// What went wrong.
    pub kind: LexErrorKind,
    /// The construct that was left open, or where the problem is.
    pub span: Span,
}

/// The kinds of lexer problem.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LexErrorKind {
    /// `'` with no closing quote.
    UnterminatedSingleQuote,
    /// `$'` with no closing quote.
    UnterminatedAnsiCQuote,
    /// `"` with no closing quote.
    UnterminatedDoubleQuote,
    /// `` ` `` with no closing backtick.
    UnterminatedBacktick,
    /// `$(` with no closing parenthesis.
    UnterminatedCommandSub,
    /// `$((` with no closing `))`.
    UnterminatedArithmetic,
    /// `${` with no closing brace.
    UnterminatedParameter,
    /// A here-document whose closing delimiter never came. A shell reading from a terminal
    /// should read another line. With `-c` or a script, `/bin/sh` ends the body at the end
    /// of the input, so such callers may choose to treat this as no error.
    UnterminatedHereDoc,
    /// The input ends in a backslash or a line continuation. A shell reading from a
    /// terminal should read another line. With `-c` or a script, `/bin/sh` runs the line
    /// as written, so such callers may choose to treat this as no error.
    TrailingBackslash,
    /// Quotes and substitutions nested too deeply. Lexing stops at this point.
    NestingTooDeep,
}

impl LexErrorKind {
    /// True if more input could fix this problem.
    #[must_use]
    pub const fn is_incomplete(self) -> bool {
        !matches!(self, Self::NestingTooDeep)
    }
}

/// The shell's reserved words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReservedWord {
    /// `if`
    If,
    /// `then`
    Then,
    /// `elif`
    Elif,
    /// `else`
    Else,
    /// `fi`
    Fi,
    /// `while`
    While,
    /// `until`
    Until,
    /// `for`
    For,
    /// `in`
    In,
    /// `do`
    Do,
    /// `done`
    Done,
    /// `case`
    Case,
    /// `esac`
    Esac,
    /// `{`
    LeftBrace,
    /// `}`
    RightBrace,
    /// `!`
    Bang,
}

impl ReservedWord {
    /// Returns the reserved word spelled exactly by `text`.
    #[must_use]
    pub fn from_text(text: &str) -> Option<Self> {
        Some(match text {
            "if" => Self::If,
            "then" => Self::Then,
            "elif" => Self::Elif,
            "else" => Self::Else,
            "fi" => Self::Fi,
            "while" => Self::While,
            "until" => Self::Until,
            "for" => Self::For,
            "in" => Self::In,
            "do" => Self::Do,
            "done" => Self::Done,
            "case" => Self::Case,
            "esac" => Self::Esac,
            "{" => Self::LeftBrace,
            "}" => Self::RightBrace,
            "!" => Self::Bang,
            _ => return None,
        })
    }
}
