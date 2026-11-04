//! Rewriting every span in a tree.
//!
//! The functions take each struct apart with a pattern that names every field. If a field
//! is added later, this file stops compiling until the new field is handled.

use super::{
    AndOr, AndOrPart, Assignment, CaseArm, CaseCommand, Command, CommandKind, ForCommand,
    FunctionDef, HereDoc, IfBranch, IfCommand, List, ListItem, LoopCommand, Parameter, PartKind,
    Pipeline, Redirect, RedirectKind, SimpleCommand, Word, WordPart,
};
use crate::span::Span;

type MapSpan<'a> = &'a mut dyn FnMut(Span) -> Span;

impl List {
    /// Replaces every span in the tree with what `f` returns for it. The parser uses this
    /// to turn positions inside a backtick body into positions in the source. It also lets
    /// a caller inspect every span of a tree.
    pub fn map_spans(&mut self, f: MapSpan<'_>) {
        let Self { items, span } = self;
        *span = f(*span);
        for item in items {
            map_item(item, f);
        }
    }
}

fn map_item(item: &mut ListItem, f: MapSpan<'_>) {
    let ListItem {
        and_or,
        background: _,
        span,
    } = item;
    *span = f(*span);
    let AndOr { first, rest, span } = and_or;
    *span = f(*span);
    map_pipeline(first, f);
    for AndOrPart { op: _, pipeline } in rest {
        map_pipeline(pipeline, f);
    }
}

fn map_pipeline(pipeline: &mut Pipeline, f: MapSpan<'_>) {
    let Pipeline {
        negated: _,
        commands,
        span,
    } = pipeline;
    *span = f(*span);
    for command in commands {
        map_command(command, f);
    }
}

fn map_command(command: &mut Command, f: MapSpan<'_>) {
    let Command {
        kind,
        redirects,
        span,
    } = command;
    *span = f(*span);
    match kind {
        CommandKind::Simple(SimpleCommand { assignments, words }) => {
            for assignment in assignments {
                map_assignment(assignment, f);
            }
            map_words(words, f);
        }
        CommandKind::Subshell(list) | CommandKind::Group(list) => list.map_spans(f),
        CommandKind::If(IfCommand {
            branches,
            else_body,
        }) => {
            for IfBranch {
                condition,
                body,
                span,
            } in branches
            {
                *span = f(*span);
                condition.map_spans(f);
                body.map_spans(f);
            }
            if let Some(body) = else_body {
                body.map_spans(f);
            }
        }
        CommandKind::While(LoopCommand { condition, body })
        | CommandKind::Until(LoopCommand { condition, body }) => {
            condition.map_spans(f);
            body.map_spans(f);
        }
        CommandKind::For(ForCommand {
            name: _,
            name_span,
            words,
            body,
        }) => {
            *name_span = f(*name_span);
            if let Some(words) = words {
                map_words(words, f);
            }
            body.map_spans(f);
        }
        CommandKind::Case(CaseCommand { word, arms }) => {
            map_word(word, f);
            for CaseArm {
                patterns,
                body,
                span,
            } in arms
            {
                *span = f(*span);
                map_words(patterns, f);
                body.map_spans(f);
            }
        }
        CommandKind::Function(FunctionDef {
            name: _,
            name_span,
            body,
        }) => {
            *name_span = f(*name_span);
            map_command(body, f);
        }
    }
    for redirect in redirects {
        map_redirect(redirect, f);
    }
}

fn map_assignment(assignment: &mut Assignment, f: MapSpan<'_>) {
    let Assignment {
        name: _,
        name_span,
        value,
        span,
    } = assignment;
    *name_span = f(*name_span);
    *span = f(*span);
    map_word(value, f);
}

fn map_redirect(redirect: &mut Redirect, f: MapSpan<'_>) {
    let Redirect { fd: _, kind, span } = redirect;
    *span = f(*span);
    match kind {
        RedirectKind::Target { op: _, word } => map_word(word, f),
        RedirectKind::HereDoc(doc) => {
            let HereDoc {
                delimiter: _,
                quoted: _,
                strip_tabs: _,
                body,
                body_span,
            } = &mut **doc;
            *body_span = f(*body_span);
            map_parts(body, f);
        }
    }
}

fn map_words(words: &mut [Word], f: MapSpan<'_>) {
    for word in words {
        map_word(word, f);
    }
}

fn map_word(word: &mut Word, f: MapSpan<'_>) {
    let Word { parts, span } = word;
    *span = f(*span);
    map_parts(parts, f);
}

fn map_parts(parts: &mut [WordPart], f: MapSpan<'_>) {
    for WordPart { kind, span } in parts {
        *span = f(*span);
        match kind {
            PartKind::Literal(_)
            | PartKind::Escaped(_)
            | PartKind::SingleQuoted(_)
            | PartKind::AnsiCQuoted(_) => {}
            PartKind::DoubleQuoted(parts) | PartKind::Arithmetic(parts) => map_parts(parts, f),
            PartKind::Parameter(parameter) => {
                let Parameter {
                    name: _,
                    name_span,
                    length: _,
                    op,
                    arg,
                } = &mut **parameter;
                *name_span = f(*name_span);
                if let Some((_, span)) = op {
                    *span = f(*span);
                }
                map_parts(arg, f);
            }
            PartKind::CommandSub(list) => list.map_spans(f),
        }
    }
}
