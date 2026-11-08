//! Checking a parsed line before anything in it runs.
//!
//! The parser accepts the whole language, but the executor may not run all of it yet. Running
//! the first half of a line and then refusing the rest would be worse than refusing all of it,
//! so the whole line (including every `$(...)`, function body and compound command inside it)
//! is looked at first. Today the executor runs everything the parser accepts (background jobs
//! came with S09a), so nothing is refused; the check stays for the next gap, and its removal
//! is a known gap in PROGRESS.md.

use omsh_syntax::Span;
use omsh_syntax::ast::{
    Command, CommandKind, List, PartKind, Pipeline, Redirect, RedirectKind, Word, WordPart,
};

/// Something the shell cannot run yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NotYet {
    /// What it is, as a noun phrase: "background jobs" in "background jobs cannot run yet".
    pub what: &'static str,
    /// Where it is in the line.
    pub span: Span,
}

impl NotYet {
    #[expect(
        dead_code,
        reason = "nothing is refused today; the constructor is for the next unfinished construct"
    )]
    pub(crate) const fn new(what: &'static str, span: Span) -> Self {
        Self { what, span }
    }
}

/// Looks at every command in `list`, and in the lists inside its words and compound commands.
pub(crate) fn check_list(list: &List) -> Result<(), NotYet> {
    for item in &list.items {
        let and_or = &item.and_or;
        check_pipeline(&and_or.first)?;
        for part in &and_or.rest {
            check_pipeline(&part.pipeline)?;
        }
    }
    Ok(())
}

fn check_pipeline(pipeline: &Pipeline) -> Result<(), NotYet> {
    pipeline.commands.iter().try_for_each(check_command)
}

fn check_command(command: &Command) -> Result<(), NotYet> {
    command.redirects.iter().try_for_each(check_redirect)?;
    match &command.kind {
        CommandKind::Simple(simple) => {
            for assignment in &simple.assignments {
                check_word(&assignment.value)?;
            }
            simple.words.iter().try_for_each(check_word)
        }
        CommandKind::Subshell(list) | CommandKind::Group(list) => check_list(list),
        CommandKind::If(command) => {
            for branch in &command.branches {
                check_list(&branch.condition)?;
                check_list(&branch.body)?;
            }
            command.else_body.as_ref().map_or(Ok(()), check_list)
        }
        CommandKind::While(command) | CommandKind::Until(command) => {
            check_list(&command.condition)?;
            check_list(&command.body)
        }
        CommandKind::For(command) => {
            if let Some(words) = &command.words {
                words.iter().try_for_each(check_word)?;
            }
            check_list(&command.body)
        }
        CommandKind::Case(command) => {
            check_word(&command.word)?;
            for arm in &command.arms {
                arm.patterns.iter().try_for_each(check_word)?;
                check_list(&arm.body)?;
            }
            Ok(())
        }
        CommandKind::Function(function) => check_command(&function.body),
    }
}

fn check_redirect(redirect: &Redirect) -> Result<(), NotYet> {
    match &redirect.kind {
        RedirectKind::Target { word, .. } => check_word(word),
        RedirectKind::HereDoc(doc) => check_parts(&doc.body),
    }
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
