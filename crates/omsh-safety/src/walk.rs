//! Walks a syntax tree and applies the rules to every command in it.

use omsh_syntax::ast::{
    Command, CommandKind, List, Pipeline, Redirect, RedirectKind, SimpleCommand,
};
use omsh_syntax::{Span, parse_tolerant};

use crate::command::{Invocation, invocation};
use crate::context::Context;
use crate::level::Level;
use crate::paths;
use crate::rules::{Env, Hit, Matcher, RULES, Rule};
use crate::word::{Arg, collect_subs};

/// How deep `eval` and `sh -c` strings are followed. A person does not nest more.
const MAX_NESTING: usize = 8;

/// One thing the analyzer found.
#[derive(Clone, Debug)]
pub struct Finding {
    /// The rule that found it.
    pub rule: &'static Rule,
    /// The level, after `sudo` raised it.
    pub level: Level,
    /// Why, in plain words.
    pub reason: String,
    /// What the command acts on, as written, when the rule knows.
    pub target: Option<String>,
    /// The command, in the text that was analyzed. For a command found inside an `eval` or
    /// `sh -c` string it is the span of the `eval` or `sh` command.
    pub span: Span,
}

/// Everything found in one line.
#[derive(Clone, Debug, Default)]
pub struct Analysis {
    /// The findings, in the order the commands appear.
    pub findings: Vec<Finding>,
}

impl Analysis {
    /// The highest level of any finding; `Safe` if there is none.
    #[must_use]
    pub fn level(&self) -> Level {
        self.findings
            .iter()
            .map(|finding| finding.level)
            .max()
            .unwrap_or(Level::Safe)
    }

    /// The findings at `level` or above.
    pub fn at_least(&self, level: Level) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(move |finding| finding.level >= level)
    }
}

/// Looks at the commands in `list`, which was parsed from `src`, and judges each of them.
/// Nothing runs and nothing is written.
#[must_use]
pub fn analyze(list: &List, src: &str, ctx: &Context<'_>) -> Analysis {
    let mut walker = Walker {
        ctx,
        cwd: ctx.cwd_text(),
        home: ctx.home_text(),
        findings: Vec::new(),
        depth: 0,
    };
    walker.list(list, &Frame { src, outer: None });
    Analysis {
        findings: walker.findings,
    }
}

/// The text a tree came from, and where that text sits in the line the person typed (for the
/// string of an `eval`).
struct Frame<'s> {
    src: &'s str,
    outer: Option<Span>,
}

struct Walker<'c, 'p> {
    ctx: &'c Context<'p>,
    /// The working directory at this point of the line, as far as it is known.
    cwd: Option<String>,
    /// What `~` means at this point. `None` once the line assigns `HOME`.
    home: Option<String>,
    findings: Vec<Finding>,
    depth: usize,
}

impl Walker<'_, '_> {
    fn list(&mut self, list: &List, frame: &Frame<'_>) {
        for item in &list.items {
            self.pipeline(&item.and_or.first, frame);
            for part in &item.and_or.rest {
                self.pipeline(&part.pipeline, frame);
            }
        }
    }

    fn pipeline(&mut self, pipeline: &Pipeline, frame: &Frame<'_>) {
        let many = pipeline.commands.len() > 1;
        let saved = self.cwd.clone();
        let stages: Vec<Option<Invocation<'_>>> = pipeline
            .commands
            .iter()
            .map(|command| self.command(command, frame))
            .collect();
        if many {
            // Every stage is a subshell: a `cd` in one does not reach the line.
            self.cwd = saved;
            let hits: Vec<(&'static Rule, Hit)> = RULES
                .iter()
                .filter_map(|rule| match rule.matcher {
                    Matcher::Pipeline(matcher) => matcher(&stages).map(|hit| (rule, hit)),
                    _ => None,
                })
                .collect();
            let sudo = stages
                .iter()
                .flatten()
                .map(|inv| inv.sudo)
                .max()
                .unwrap_or(0);
            for (rule, hit) in hits {
                self.record(rule, hit, sudo, pipeline.span, frame);
            }
        }
    }

    /// Walks one command. Returns what a simple command runs, for the rules over pipelines.
    fn command<'a>(&mut self, command: &'a Command, frame: &Frame<'_>) -> Option<Invocation<'a>> {
        for redirect in &command.redirects {
            self.redirect(redirect, command.span, frame);
        }
        match &command.kind {
            CommandKind::Simple(simple) => return self.simple(command.span, simple, frame),
            CommandKind::Subshell(list) => {
                let saved = self.cwd.clone();
                self.list(list, frame);
                self.cwd = saved;
            }
            CommandKind::Group(list) => self.list(list, frame),
            CommandKind::If(branch) => {
                let before = self.cwd.clone();
                for b in &branch.branches {
                    self.list(&b.condition, frame);
                    self.list(&b.body, frame);
                }
                if let Some(body) = &branch.else_body {
                    self.list(body, frame);
                }
                self.settle(&before);
            }
            CommandKind::While(lp) | CommandKind::Until(lp) => {
                let before = self.cwd.clone();
                self.list(&lp.condition, frame);
                self.list(&lp.body, frame);
                self.settle(&before);
            }
            CommandKind::For(lp) => {
                let before = self.cwd.clone();
                for word in lp.words.iter().flatten() {
                    let arg = Arg::from_word(word, frame.src, self.home.as_deref());
                    self.subs(&arg, frame);
                }
                self.list(&lp.body, frame);
                self.settle(&before);
            }
            CommandKind::Case(case) => {
                let before = self.cwd.clone();
                let arg = Arg::from_word(&case.word, frame.src, self.home.as_deref());
                self.subs(&arg, frame);
                for arm in &case.arms {
                    self.list(&arm.body, frame);
                }
                self.settle(&before);
            }
            CommandKind::Function(def) => {
                let hits: Vec<(&'static Rule, Hit)> = RULES
                    .iter()
                    .filter_map(|rule| match rule.matcher {
                        Matcher::Function(matcher) => matcher(def).map(|hit| (rule, hit)),
                        _ => None,
                    })
                    .collect();
                for (rule, hit) in hits {
                    self.record(rule, hit, 0, command.span, frame);
                }
                // The body runs later, wherever the shell is then: a `cd` in it does not
                // count for the line.
                let saved = self.cwd.clone();
                self.command(&def.body, frame);
                self.cwd = saved;
            }
        }
        None
    }

    /// After a command that may or may not have run its body: if it changed the working
    /// directory, it is no longer known.
    fn settle(&mut self, before: &Option<String>) {
        if self.cwd != *before {
            self.cwd = None;
        }
    }

    fn redirect(&mut self, redirect: &Redirect, command: Span, frame: &Frame<'_>) {
        match &redirect.kind {
            RedirectKind::Target { op, word } => {
                let arg = Arg::from_word(word, frame.src, self.home.as_deref());
                self.subs(&arg, frame);
                let hits: Vec<(&'static Rule, Hit)> = {
                    let env = self.env();
                    RULES
                        .iter()
                        .filter_map(|rule| match rule.matcher {
                            Matcher::Redirect(matcher) => {
                                matcher(*op, &arg, &env).map(|hit| (rule, hit))
                            }
                            _ => None,
                        })
                        .collect()
                };
                for (rule, hit) in hits {
                    self.record(rule, hit, 0, command, frame);
                }
            }
            RedirectKind::HereDoc(doc) => {
                if !doc.quoted {
                    let mut subs = Vec::new();
                    collect_subs(&doc.body, &mut subs);
                    for list in subs {
                        let saved = self.cwd.clone();
                        self.list(list, frame);
                        self.cwd = saved;
                    }
                }
            }
        }
    }

    fn simple<'a>(
        &mut self,
        span: Span,
        simple: &'a SimpleCommand,
        frame: &Frame<'_>,
    ) -> Option<Invocation<'a>> {
        for assignment in &simple.assignments {
            let value = Arg::from_word(&assignment.value, frame.src, self.home.as_deref());
            self.subs(&value, frame);
            if assignment.name == "HOME" {
                self.home = None;
            }
        }
        let words: Vec<Arg<'a>> = simple
            .words
            .iter()
            .map(|word| Arg::from_word(word, frame.src, self.home.as_deref()))
            .collect();
        for word in &words {
            self.subs(word, frame);
        }
        let inv = invocation(span, words)?;
        if inv.only_asks_for_help() {
            return Some(inv);
        }
        let hits: Vec<(&'static Rule, Hit)> = {
            let env = self.env();
            RULES
                .iter()
                .filter_map(|rule| match rule.matcher {
                    Matcher::Command(matcher) => matcher(&inv, &env).map(|hit| (rule, hit)),
                    _ => None,
                })
                .collect()
        };
        for (rule, hit) in hits {
            self.record(rule, hit, inv.sudo, span, frame);
        }
        self.effects(&inv, span, frame);
        Some(inv)
    }

    /// What a command does to the walk itself: `cd` moves, `eval` and `sh -c` hold more code.
    fn effects(&mut self, inv: &Invocation<'_>, span: Span, frame: &Frame<'_>) {
        match inv.name.as_str() {
            "cd" => self.change_directory(inv),
            "pushd" | "popd" => self.cwd = None,
            "eval" => {
                if let Some(script) = static_script(inv.args.iter()) {
                    // `eval` runs in this shell: a `cd` in it counts.
                    self.nested(&script, span, frame, true);
                }
            }
            "sh" | "bash" | "dash" | "zsh" | "ksh" | "ash" => {
                let options = inv.options();
                if options.has_short('c')
                    && let Some(script) = options.operands.first()
                    && !script.unknown
                {
                    self.nested(&script.text, span, frame, false);
                }
            }
            _ => {}
        }
    }

    fn change_directory(&mut self, inv: &Invocation<'_>) {
        let options = inv.options();
        self.cwd = match options.operands.first() {
            None => self.home.clone(),
            Some(arg) if arg.unknown || arg.text == "-" => None,
            Some(arg) => match self.cwd.as_deref() {
                Some(base) => Some(paths::resolve(base, &arg.text)),
                None if arg.text.starts_with('/') => Some(paths::resolve("/", &arg.text)),
                None => None,
            },
        };
    }

    /// Analyzes code that is held in a string.
    fn nested(&mut self, script: &str, at: Span, frame: &Frame<'_>, keep_directory: bool) {
        if self.depth >= MAX_NESTING {
            return;
        }
        let parsed = parse_tolerant(script);
        let inner = Frame {
            src: script,
            outer: Some(frame.outer.unwrap_or(at)),
        };
        let saved = self.cwd.clone();
        self.depth += 1;
        self.list(&parsed.list, &inner);
        self.depth -= 1;
        if !keep_directory {
            self.cwd = saved;
        }
    }

    /// Walks the command substitutions inside a word, in their own subshell.
    fn subs(&mut self, arg: &Arg<'_>, frame: &Frame<'_>) {
        for list in &arg.subs {
            let saved = self.cwd.clone();
            self.list(list, frame);
            self.cwd = saved;
        }
    }

    fn env(&self) -> Env<'_> {
        Env {
            cwd: self.cwd.as_deref(),
            home: self.home.as_deref(),
            probe: self.ctx.probe,
        }
    }

    fn record(&mut self, rule: &'static Rule, hit: Hit, sudo: u8, span: Span, frame: &Frame<'_>) {
        debug_assert!(
            hit.level <= rule.level,
            "rule {} gave {:?}, above its own level {:?}",
            rule.id,
            hit.level,
            rule.level
        );
        let mut level = hit.level.min(rule.level);
        let mut reason = rule.reason.to_owned();
        if let Some(note) = hit.note {
            reason.push(' ');
            reason.push_str(note);
        }
        if sudo > 0 && level.raise() != level {
            level = level.raise();
            reason.push_str(" It runs with sudo.");
        }
        let span = frame.outer.unwrap_or(span);
        let known = self.findings.iter().any(|finding| {
            std::ptr::eq(finding.rule, rule) && finding.span == span && finding.target == hit.target
        });
        if !known {
            self.findings.push(Finding {
                rule,
                level,
                reason,
                target: hit.target,
                span,
            });
        }
    }
}

/// The words of an `eval` joined by spaces, if each is the same on every run.
fn static_script<'i, 'a: 'i>(args: impl Iterator<Item = &'i Arg<'a>>) -> Option<String> {
    let mut script = String::new();
    for (i, arg) in args.enumerate() {
        if arg.unknown {
            return None;
        }
        if i > 0 {
            script.push(' ');
        }
        script.push_str(&arg.text);
    }
    Some(script)
}
