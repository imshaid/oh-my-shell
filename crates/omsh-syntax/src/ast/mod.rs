//! The syntax tree that the parser builds.
//!
//! Every node records where it came from as a [`Span`] into the text that was parsed.
//!
//! The tree also owns its text, instead of reading it back through spans, for two reasons.
//! A function definition outlives the line it was typed on, so the tree must stay usable
//! after the source is gone. And a span cannot always give the text a node means: in
//! `` `echo a\$` `` the word is `a$`, while the source says `a\$`.

mod spans;

pub use crate::token::ParamOp;

use crate::span::Span;

/// A list of commands: a whole input, or the body of a compound command or substitution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct List {
    /// The commands, in order.
    pub items: Vec<ListItem>,
    /// From the first item to the last. Empty, and placed where the list would start, if
    /// there are no items.
    pub span: Span,
}

impl List {
    /// A list with no items.
    pub(crate) const fn empty(at: usize) -> Self {
        Self {
            items: Vec::new(),
            span: Span::new(at, at),
        }
    }
}

/// One entry of a [`List`], with the `;` or `&` that ends it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListItem {
    /// What to run.
    pub and_or: AndOr,
    /// True if it ends in `&`, so that the shell does not wait for it.
    pub background: bool,
    /// Where it is, including its `;` or `&`.
    pub span: Span,
}

/// Pipelines joined by `&&` and `||`, which run from left to right with equal priority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AndOr {
    /// The pipeline that always runs.
    pub first: Pipeline,
    /// The pipelines after it, each with the operator before it.
    pub rest: Vec<AndOrPart>,
    /// Where it is.
    pub span: Span,
}

/// A pipeline after `&&` or `||`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AndOrPart {
    /// The operator before the pipeline.
    pub op: AndOrOp,
    /// The pipeline.
    pub pipeline: Pipeline,
}

/// The operators between pipelines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AndOrOp {
    /// `&&`
    And,
    /// `||`
    Or,
}

/// Commands joined by `|`, with an optional `!` in front.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pipeline {
    /// True if it starts with `!`, which inverts its exit status.
    pub negated: bool,
    /// The commands, from left to right. Never empty.
    pub commands: Vec<Command>,
    /// Where it is, including any `!`.
    pub span: Span,
}

/// One command and its redirections.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    /// What kind of command it is.
    pub kind: CommandKind,
    /// The redirections, in the order they were written. They apply in that order.
    pub redirects: Vec<Redirect>,
    /// Where it is, including its redirections, but not any here-document body.
    pub span: Span,
}

/// The kinds of command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandKind {
    /// `FOO=bar cmd arg`
    Simple(SimpleCommand),
    /// `( list )`
    Subshell(List),
    /// `{ list; }`
    Group(List),
    /// `if list; then list; fi`
    If(IfCommand),
    /// `while list; do list; done`
    While(LoopCommand),
    /// `until list; do list; done`
    Until(LoopCommand),
    /// `for name in words; do list; done`
    For(ForCommand),
    /// `case word in pattern) list;; esac`
    Case(CaseCommand),
    /// `name() command`
    Function(FunctionDef),
}

/// Assignments and words. Both can be empty: `>file` is a simple command too.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SimpleCommand {
    /// The `NAME=value` words before the command name.
    pub assignments: Vec<Assignment>,
    /// The command name and its arguments.
    pub words: Vec<Word>,
}

/// `NAME=value` in front of a command, or by itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Assignment {
    /// The variable's name.
    pub name: String,
    /// Where the name is.
    pub name_span: Span,
    /// The value. It has no parts for `NAME=`.
    pub value: Word,
    /// Where the whole assignment is.
    pub span: Span,
}

/// An `if` command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IfCommand {
    /// The `if` branch and then each `elif` branch. Never empty.
    pub branches: Vec<IfBranch>,
    /// The body after `else`.
    pub else_body: Option<List>,
}

/// A condition and the body it guards.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IfBranch {
    /// The list between `if` or `elif` and `then`.
    pub condition: List,
    /// The list after `then`.
    pub body: List,
    /// From `if` or `elif` to the end of the body.
    pub span: Span,
}

/// A `while` or `until` command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoopCommand {
    /// The list before `do`.
    pub condition: List,
    /// The list between `do` and `done`.
    pub body: List,
}

/// A `for` command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForCommand {
    /// The loop variable's name.
    pub name: String,
    /// Where the name is.
    pub name_span: Span,
    /// The words after `in`. Absent if there is no `in`, which loops over `"$@"`.
    pub words: Option<Vec<Word>>,
    /// The list between `do` and `done`.
    pub body: List,
}

/// A `case` command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaseCommand {
    /// The word to match.
    pub word: Word,
    /// The patterns and their bodies, in order.
    pub arms: Vec<CaseArm>,
}

/// One `pattern) list ;;` of a `case` command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaseArm {
    /// The patterns separated by `|`.
    pub patterns: Vec<Word>,
    /// What to run on a match. It can be empty.
    pub body: List,
    /// From the first pattern, or the `(` before it, to the end of the body.
    pub span: Span,
}

/// `name() command`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FunctionDef {
    /// The function's name.
    pub name: String,
    /// Where the name is.
    pub name_span: Span,
    /// The function's body. A redirection written after the body belongs to the body, and
    /// applies each time the function is called.
    pub body: Box<Command>,
}

/// A redirection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Redirect {
    /// The file descriptor written in front of the operator: the `2` in `2>f`.
    pub fd: Option<u32>,
    /// What it does.
    pub kind: RedirectKind,
    /// Where it is, from the file descriptor to the target. A here-document's body is
    /// elsewhere; see [`HereDoc::body_span`].
    pub span: Span,
}

/// What a redirection does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RedirectKind {
    /// An operator followed by a word.
    Target {
        /// The operator.
        op: RedirectOp,
        /// The word after it.
        word: Word,
    },
    /// `<<` or `<<-` and the text that goes with it.
    HereDoc(Box<HereDoc>),
}

/// The redirection operators that take a word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RedirectOp {
    /// `<`
    Input,
    /// `>`
    Output,
    /// `>>`
    Append,
    /// `<>`
    ReadWrite,
    /// `>|`
    Clobber,
    /// `<&`
    DupInput,
    /// `>&`
    DupOutput,
    /// `&>`
    OutputAndError,
    /// `&>>`
    AppendAndError,
    /// `<<<`
    HereString,
}

/// A here-document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HereDoc {
    /// The closing delimiter, with its quotes removed.
    pub delimiter: String,
    /// True if any part of the delimiter was quoted, which turns off expansion.
    pub quoted: bool,
    /// True for `<<-`. The tabs it strips are already gone from `body`.
    pub strip_tabs: bool,
    /// The body's pieces.
    pub body: Vec<WordPart>,
    /// Where the body is. It comes after the line holding the `<<`, so it lies outside the
    /// spans of the redirection and the command.
    pub body_span: Span,
}

/// A word and the pieces it is made of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Word {
    /// The pieces, in order.
    pub parts: Vec<WordPart>,
    /// Where it is.
    pub span: Span,
}

/// One piece of a word.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WordPart {
    /// What the piece is.
    pub kind: PartKind,
    /// Where it is, including any quotes or `$` around it.
    pub span: Span,
}

/// The kinds of word piece. Line continuations are gone by this point, so `a\<newline>b` is
/// one [`PartKind::Literal`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PartKind {
    /// Text with no special meaning to the parser. Tildes and glob characters are still in
    /// it, because whether they are special depends on where expansion finds them. It holds
    /// a backslash only inside double quotes, here-documents and arithmetic, where one that
    /// escapes nothing stays as text. In an unquoted word, a backslash is always
    /// [`PartKind::Escaped`].
    Literal(String),
    /// A character that a backslash made literal.
    Escaped(char),
    /// `'...'`, holding the text between the quotes.
    SingleQuoted(String),
    /// `$'...'`, holding the text between the quotes. Escape sequences are decoded later.
    AnsiCQuoted(String),
    /// `"..."`, holding the pieces between the quotes.
    DoubleQuoted(Vec<WordPart>),
    /// `$NAME`, `${NAME}`, `${#NAME}` or `${NAME:-word}` and similar.
    Parameter(Box<Parameter>),
    /// `$(...)` or `` `...` ``, holding the commands inside.
    CommandSub(List),
    /// `$((...))`, holding the pieces between the parentheses.
    Arithmetic(Vec<WordPart>),
}

/// A parameter expansion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parameter {
    /// The parameter's name, number or special character. Empty for a malformed `${}`.
    pub name: String,
    /// Where the name is.
    pub name_span: Span,
    /// True for `${#NAME}`, the length of the value.
    pub length: bool,
    /// The operator and where it is. Absent for plain `$NAME` and `${NAME}`.
    pub op: Option<(ParamOp, Span)>,
    /// The pieces of the word after the operator.
    pub arg: Vec<WordPart>,
}
