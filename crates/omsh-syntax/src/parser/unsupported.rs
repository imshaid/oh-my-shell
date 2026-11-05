//! Bash-only syntax that §6.2 rules out, and how the parser notices it.
//!
//! All checks run inside the normal parsing paths, so strict mode stops at the first one and
//! tolerant mode lists them all. Each check looks at tokens or tree nodes that are already
//! there and never scans the text again, so none of them can make parsing slower than linear.
//!
//! `time` is deliberately not here: `dash` treats it as an ordinary command, and so do we.

use super::error::ParseErrorKind;
use super::{Parser, is_name};
use crate::ast::{CommandKind, List, PartKind, RedirectKind, RedirectOp, Word, WordPart};
use crate::span::{Span, strip_line_continuations};
use crate::token::{self, Op, ParamOp, Token, TokenKind};

/// The bash features that Oh My Shell does not support (§6.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unsupported {
    /// `[[ ... ]]`
    DoubleBracket,
    /// `<(...)` and `>(...)`
    ProcessSubstitution,
    /// `{a,b}` and `{1..3}`
    BraceExpansion,
    /// `a=(1 2)`, `a+=(3)`, `a[0]=x` and `${a[0]}`
    Array,
    /// `select i in ...; do ...; done`
    Select,
    /// `coproc`
    Coprocess,
    /// `shopt`
    Shopt,
    /// `${x/a/b}`
    PatternSubstitution,
    /// `${x:1:2}`
    Substring,
    /// `${x^^}` and `${x,,}`
    CaseModification,
    /// `${!x}`
    Indirection,
    /// `${x@Q}`
    Transformation,
    /// `((i++))`
    ArithmeticCommand,
    /// `function f { ...; }`
    FunctionKeyword,
    /// `a |& b`
    PipeStderr,
    /// `;&` and `;;&` at the end of a `case` branch
    CaseFallthrough,
    /// `a+=b`
    AppendAssignment,
    /// `$(<file)`
    FileContents,
}

impl Unsupported {
    /// Every feature, for tests that check each one has a message.
    pub const ALL: [Self; 18] = [
        Self::DoubleBracket,
        Self::ProcessSubstitution,
        Self::BraceExpansion,
        Self::Array,
        Self::Select,
        Self::Coprocess,
        Self::Shopt,
        Self::PatternSubstitution,
        Self::Substring,
        Self::CaseModification,
        Self::Indirection,
        Self::Transformation,
        Self::ArithmeticCommand,
        Self::FunctionKeyword,
        Self::PipeStderr,
        Self::CaseFallthrough,
        Self::AppendAssignment,
        Self::FileContents,
    ];

    /// A noun phrase naming the feature, which reads right before "is not supported".
    #[must_use]
    pub const fn what(self) -> &'static str {
        match self {
            Self::DoubleBracket => "the `[[ ... ]]` test",
            Self::ProcessSubstitution => "process substitution",
            Self::BraceExpansion => "brace expansion",
            Self::Array => "array syntax",
            Self::Select => "the `select` loop",
            Self::Coprocess => "the `coproc` keyword",
            Self::Shopt => "the `shopt` command",
            Self::PatternSubstitution => "the `${var/pattern/text}` replacement",
            Self::Substring => "the `${var:offset:length}` substring form",
            Self::CaseModification => "the `${var^}` and `${var,}` case change",
            Self::Indirection => "the `${!var}` indirect expansion",
            Self::Transformation => "the `${var@op}` transformation",
            Self::ArithmeticCommand => "the `(( ... ))` arithmetic command",
            Self::FunctionKeyword => "the `function` keyword",
            Self::PipeStderr => "the `|&` pipe",
            Self::CaseFallthrough => "the `;&` and `;;&` ending of a `case` branch",
            Self::AppendAssignment => "the `name+=value` assignment",
            Self::FileContents => "the `$(<file)` shortcut",
        }
    }

    /// What to write instead.
    #[must_use]
    pub const fn help(self) -> &'static str {
        match self {
            Self::DoubleBracket => "use `[ ... ]` or `test`, and `case` for patterns",
            Self::ProcessSubstitution => "write the output to a temporary file, or use a pipe",
            Self::BraceExpansion => {
                "write the words out, or use a `for` loop; quote the braces to keep them as text"
            }
            Self::Array => "use separate variables, or the positional parameters with `set --`",
            Self::Select => "use a `while` loop with `read`",
            Self::Coprocess => "use a pipe, or a named pipe made with `mkfifo`",
            Self::Shopt => "Oh My Shell has no shell options to switch; remove the command",
            Self::PatternSubstitution => "use `sed`, or `${var#prefix}` and `${var%suffix}`",
            Self::Substring => "use `cut` or `expr substr`",
            Self::CaseModification => "use `tr` to change the case of text",
            Self::Indirection => "use `eval` to read a variable named by another variable",
            Self::Transformation => "use the plain `${var}`",
            Self::ArithmeticCommand => {
                "use `: $((...))` to run an expression, or `[ $((...)) -ne 0 ]` to test it"
            }
            Self::FunctionKeyword => "write `name() { ...; }` instead",
            Self::PipeStderr => "write `2>&1 |` instead",
            Self::CaseFallthrough => "end each branch with `;;`",
            Self::AppendAssignment => "write `name=\"${name}value\"` instead",
            Self::FileContents => "use `$(cat file)` instead",
        }
    }
}

impl Parser<'_> {
    /// Checks the first word of a simple command, which `span` covers. The word has been
    /// consumed already. A word in quotes never matches, because its text includes the quotes.
    pub(super) fn check_command_name(&mut self, span: Span) {
        let text = strip_line_continuations(span.text(self.src));
        let feature = match text.as_ref() {
            "[[" => Some(Unsupported::DoubleBracket),
            "function" => Some(Unsupported::FunctionKeyword),
            "select" => Some(Unsupported::Select),
            "coproc" => Some(Unsupported::Coprocess),
            "shopt" => Some(Unsupported::Shopt),
            // `a+=(1)` is an array. `unexpected` sees the `(` and says so.
            _ if self.next_is_adjacent_paren(span) => None,
            _ if is_array_element_assignment(&text) => Some(Unsupported::Array),
            _ if is_append_assignment(&text) => Some(Unsupported::AppendAssignment),
            _ => None,
        };
        if let Some(feature) = feature {
            self.report(ParseErrorKind::Unsupported(feature), span);
        }
    }

    /// True if the token at the cursor is a `(` that touches `span`.
    pub(super) fn next_is_adjacent_paren(&self, span: Span) -> bool {
        self.peek().is_some_and(|next| {
            next.span.start == span.end && matches!(next.kind, TokenKind::Op(Op::LeftParen))
        })
    }

    /// Looks at a token that does not fit, together with the one before it. Two operators
    /// that touch can be one bash operator: `|&`, `;&`, `<(` and `>(`, and so can `a=(`.
    /// Returns the feature and the span of both tokens. Operators that are apart, as in
    /// `a | & b`, are an ordinary mistake.
    pub(super) fn unsupported_at(&self, token: &Token) -> Option<(Unsupported, Span)> {
        // `a > >(b)`: the redirection has no target yet, and the `>` that stands in its place
        // is directly followed by a `(`.
        if let (TokenKind::Op(Op::Less | Op::Great), Some(next)) =
            (&token.kind, self.tokens.get(self.pos + 1))
            && next.span.start == token.span.end
            && matches!(next.kind, TokenKind::Op(Op::LeftParen))
        {
            let span = Span::new(token.span.start, next.span.end);
            return Some((Unsupported::ProcessSubstitution, span));
        }
        let previous = self.pos.checked_sub(1).and_then(|i| self.tokens.get(i))?;
        if previous.span.end != token.span.start {
            return None;
        }
        let feature = match (&previous.kind, &token.kind) {
            (TokenKind::Op(Op::Pipe), TokenKind::Op(Op::Amp)) => Unsupported::PipeStderr,
            (TokenKind::Op(Op::Semi | Op::DoubleSemi), TokenKind::Op(Op::Amp)) => {
                Unsupported::CaseFallthrough
            }
            (TokenKind::Op(Op::Less | Op::Great), TokenKind::Op(Op::LeftParen)) => {
                Unsupported::ProcessSubstitution
            }
            (TokenKind::Word(_), TokenKind::Op(Op::LeftParen))
                if strip_line_continuations(previous.span.text(self.src)).ends_with('=') =>
            {
                Unsupported::Array
            }
            _ => return None,
        };
        Some((feature, Span::new(previous.span.start, token.span.end)))
    }

    /// Checks a subshell for being `((...))`, which bash reads as arithmetic. Its rule is
    /// that the two opening parentheses touch and the two closing ones touch, so `( (a))` and
    /// `((a) )` stay subshells. `open` is the outer `(`, and the outer `)` has just been
    /// consumed.
    pub(super) fn check_arithmetic_command(&mut self, open: Span, list: &List) {
        let Some(close) = self.pos.checked_sub(1).and_then(|i| self.tokens.get(i)) else {
            return;
        };
        let [item] = list.items.as_slice() else {
            return;
        };
        let [command] = item.and_or.first.commands.as_slice() else {
            return;
        };
        let is_arithmetic = matches!(command.kind, CommandKind::Subshell(_))
            && item.and_or.rest.is_empty()
            && command.redirects.is_empty()
            && command.span.start == open.end
            && command.span.end == close.span.start;
        if is_arithmetic {
            let span = Span::new(open.start, close.span.end);
            self.report(
                ParseErrorKind::Unsupported(Unsupported::ArithmeticCommand),
                span,
            );
        }
    }

    /// Checks a `${...}` for being malformed or bash-only. `span` covers all of it.
    pub(super) fn check_parameter(&mut self, parameter: &token::Parameter, span: Span) {
        let name = strip_line_continuations(parameter.name.text(self.src));
        let kind = if name.is_empty() {
            ParseErrorKind::BadSubstitution
        } else if let Some((ParamOp::Unsupported, op_span)) = parameter.op {
            // The span of an unsupported operator can be empty, so look at the text itself.
            let first = self
                .src
                .get(op_span.start..)
                .and_then(|rest| rest.chars().next());
            match first {
                Some('[') => ParseErrorKind::Unsupported(Unsupported::Array),
                // `${#x:-y}` is not valid in any shell.
                _ if parameter.length => ParseErrorKind::BadSubstitution,
                _ if name == "!" => ParseErrorKind::Unsupported(Unsupported::Indirection),
                Some('/') => ParseErrorKind::Unsupported(Unsupported::PatternSubstitution),
                Some(':') => ParseErrorKind::Unsupported(Unsupported::Substring),
                Some('^' | ',') => ParseErrorKind::Unsupported(Unsupported::CaseModification),
                Some('@') => ParseErrorKind::Unsupported(Unsupported::Transformation),
                _ => ParseErrorKind::BadSubstitution,
            }
        } else {
            return;
        };
        self.report(kind, span);
    }

    /// Checks a word of a command, which the shell would brace-expand. Assignment values are
    /// not checked, because bash does not expand braces there either.
    pub(super) fn check_braces(&mut self, word: &Word) {
        if has_brace_expansion(&word.parts) {
            self.report(
                ParseErrorKind::Unsupported(Unsupported::BraceExpansion),
                word.span,
            );
        }
    }

    /// Checks the body of a command substitution for being `<file` and nothing else.
    pub(super) fn check_file_contents(&mut self, list: &List, span: Span) {
        let [item] = list.items.as_slice() else {
            return;
        };
        let [command] = item.and_or.first.commands.as_slice() else {
            return;
        };
        let CommandKind::Simple(simple) = &command.kind else {
            return;
        };
        let [redirect] = command.redirects.as_slice() else {
            return;
        };
        let only_a_redirect = item.and_or.rest.is_empty()
            && !item.and_or.first.negated
            && !item.background
            && simple.assignments.is_empty()
            && simple.words.is_empty()
            && redirect.fd.is_none()
            && matches!(
                redirect.kind,
                RedirectKind::Target {
                    op: RedirectOp::Input,
                    ..
                }
            );
        if only_a_redirect {
            self.report(ParseErrorKind::Unsupported(Unsupported::FileContents), span);
        }
    }
}

/// True for `name[...]=...` and `name[...]+=...`, bash's array element assignments.
fn is_array_element_assignment(text: &str) -> bool {
    let Some(open) = text.find('[') else {
        return false;
    };
    let name = text.get(..open).unwrap_or("");
    let rest = text.get(open..).unwrap_or("");
    is_name(name) && (rest.contains("]=") || rest.contains("]+="))
}

/// True for `name+=...`.
fn is_append_assignment(text: &str) -> bool {
    text.split_once("+=").is_some_and(|(name, _)| is_name(name))
}

/// One `{` that has not been closed yet.
struct Brace {
    /// True once a `,` has been seen directly inside it.
    comma: bool,
    /// The plain text inside it, for telling `{1..3}` from `{a}`.
    text: String,
    /// False once anything but plain text has been seen inside it.
    plain: bool,
}

impl Brace {
    const fn new() -> Self {
        Self {
            comma: false,
            text: String::new(),
            plain: true,
        }
    }
}

/// True if an unquoted `{...}` in the word has a top-level comma or is a sequence. Escaped
/// characters, quoted text and expansions count as ordinary text and never as a brace or comma.
/// An unclosed `{` does nothing, as in bash.
fn has_brace_expansion(parts: &[WordPart]) -> bool {
    let has_brace = parts
        .iter()
        .any(|part| matches!(&part.kind, PartKind::Literal(text) if text.contains('{')));
    if !has_brace {
        return false;
    }
    let mut open: Vec<Brace> = Vec::new();
    for part in parts {
        let PartKind::Literal(text) = &part.kind else {
            if let Some(top) = open.last_mut() {
                top.plain = false;
            }
            continue;
        };
        for c in text.chars() {
            match c {
                '{' => open.push(Brace::new()),
                '}' => {
                    if let Some(closed) = open.pop() {
                        if closed.comma || (closed.plain && is_sequence(&closed.text)) {
                            return true;
                        }
                        if let Some(parent) = open.last_mut() {
                            parent.plain = false;
                        }
                    }
                }
                _ => {
                    if let Some(top) = open.last_mut() {
                        top.comma |= c == ',';
                        top.text.push(c);
                    }
                }
            }
        }
    }
    false
}

/// True for `1..3`, `a..e` and either with a step as in `1..9..2`.
fn is_sequence(text: &str) -> bool {
    let mut pieces = text.split("..");
    let (Some(from), Some(to)) = (pieces.next(), pieces.next()) else {
        return false;
    };
    let step = pieces.next();
    if pieces.next().is_some() {
        return false;
    }
    let ends = (is_integer(from) && is_integer(to)) || (is_letter(from) && is_letter(to));
    ends && step.is_none_or(is_integer)
}

fn is_integer(text: &str) -> bool {
    let digits = text.strip_prefix(['-', '+']).unwrap_or(text);
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

fn is_letter(text: &str) -> bool {
    matches!(text.as_bytes(), [b] if b.is_ascii_alphabetic())
}
