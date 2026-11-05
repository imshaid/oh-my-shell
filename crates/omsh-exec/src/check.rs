//! Checking a parsed line before anything in it runs.
//!
//! The parser accepts the whole language, but the executor does not yet. Running the first
//! half of a line and then refusing the rest would be worse than refusing all of it, so the
//! whole line (including every `$(...)` inside it) is looked at first.

use omsh_syntax::Span;
use omsh_syntax::ast::{Command, CommandKind, List, PartKind, Pipeline, Word, WordPart};

/// Something the shell cannot run yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NotYet {
    /// What it is, as a noun phrase: "pipelines" in "pipelines are not implemented yet".
    pub what: &'static str,
    /// Where it is in the line.
    pub span: Span,
}

impl NotYet {
    pub(crate) const fn new(what: &'static str, span: Span) -> Self {
        Self { what, span }
    }
}

/// Looks at every command in `list`, and in the lists inside its words.
pub(crate) fn check_list(list: &List) -> Result<(), NotYet> {
    for item in &list.items {
        if item.background {
            return Err(NotYet::new("background jobs", item.span));
        }
        let and_or = &item.and_or;
        if let Some(part) = and_or.rest.first() {
            return Err(NotYet::new("`&&` and `||`", part.pipeline.span));
        }
        check_pipeline(&and_or.first)?;
    }
    Ok(())
}

fn check_pipeline(pipeline: &Pipeline) -> Result<(), NotYet> {
    if pipeline.negated {
        return Err(NotYet::new("`!`", pipeline.span));
    }
    let [command] = pipeline.commands.as_slice() else {
        return Err(NotYet::new("pipelines", pipeline.span));
    };
    check_command(command)
}

fn check_command(command: &Command) -> Result<(), NotYet> {
    if let Some(redirect) = command.redirects.first() {
        return Err(NotYet::new("redirections", redirect.span));
    }
    let span = command.span;
    let simple = match &command.kind {
        CommandKind::Simple(simple) => simple,
        CommandKind::Subshell(_) => return Err(NotYet::new("subshells", span)),
        CommandKind::Group(_) => return Err(NotYet::new("command groups", span)),
        CommandKind::If(_) => return Err(NotYet::new("`if` commands", span)),
        CommandKind::While(_) | CommandKind::Until(_) | CommandKind::For(_) => {
            return Err(NotYet::new("loops", span));
        }
        CommandKind::Case(_) => return Err(NotYet::new("`case` commands", span)),
        CommandKind::Function(_) => return Err(NotYet::new("function definitions", span)),
    };
    for assignment in &simple.assignments {
        check_word(&assignment.value)?;
    }
    simple.words.iter().try_for_each(check_word)
}

fn check_word(word: &Word) -> Result<(), NotYet> {
    check_parts(&word.parts)
}

fn check_parts(parts: &[WordPart]) -> Result<(), NotYet> {
    for part in parts {
        match &part.kind {
            PartKind::CommandSub(list) => check_list(list)?,
            PartKind::DoubleQuoted(inner) | PartKind::Arithmetic(inner) => check_parts(inner)?,
            PartKind::Parameter(parameter) => check_parts(&parameter.arg)?,
            PartKind::Literal(_)
            | PartKind::Escaped(_)
            | PartKind::SingleQuoted(_)
            | PartKind::AnsiCQuoted(_) => {}
        }
    }
    Ok(())
}
