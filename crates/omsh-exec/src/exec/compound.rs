//! Compound commands: `( )`, `{ }`, `if`, `while`, `until`, `for`, `case`, and function
//! definitions.

use std::sync::Arc;
use std::thread;

use omsh_syntax::ast::{CaseCommand, CommandKind, ForCommand, IfCommand, List, LoopCommand};

use super::pipeline::STAGE_STACK;
use super::{Flow, Unwind};
use crate::expand::Expander;
use crate::io::Io;
use crate::jobs::Job;
use crate::shell::Shell;
use crate::status::ExitStatus;

impl Shell {
    /// Runs a command that is not a simple command. Its redirections are already in `io`.
    pub(super) fn run_compound(&mut self, kind: &CommandKind, io: &Io) -> Flow {
        match kind {
            CommandKind::Simple(_) => {
                debug_assert!(false, "simple commands do not come here");
                Ok(self.status)
            }
            CommandKind::Subshell(list)
                if self.options.monitor && !self.no_new_jobs && self.job.is_none() =>
            {
                self.run_subshell_job(list, io)
            }
            CommandKind::Subshell(list) => {
                let mut copy = self.subshell();
                let flow = copy.run_list(list, io);
                Ok(copy.finish_copy(flow, io))
            }
            CommandKind::Group(list) => self.run_list(list, io),
            CommandKind::If(command) => self.run_if(command, io),
            CommandKind::While(command) => self.run_loop(command, false, io),
            CommandKind::Until(command) => self.run_loop(command, true, io),
            CommandKind::For(command) => self.run_for(command, io),
            CommandKind::Case(command) => self.run_case(command, io),
            CommandKind::Function(function) => {
                self.functions
                    .insert(function.name.clone(), Arc::new(function.clone()));
                Ok(ExitStatus::SUCCESS)
            }
        }
    }

    /// `( list )` typed at the prompt of a shell with job control: one job, whose shell code
    /// runs on a thread, as the stages of a pipeline do. Ctrl+Z stops the whole subshell and
    /// `fg` continues it where it was, as in bash.
    fn run_subshell_job(&mut self, list: &List, io: &Io) -> Flow {
        let job = Job::foreground(self.jobs.bell(), 1, false);
        let mut copy = self.subshell();
        copy.job = Some(Arc::clone(&job));
        copy.task = 0;
        let list = list.clone();
        let table = io.clone();
        let task = job.task(0);
        let spawned = thread::Builder::new()
            .stack_size(STAGE_STACK)
            .spawn(move || {
                let flow = copy.run_list(&list, &table);
                let status = copy.finish_copy(flow, &table);
                drop(table);
                task.done(status);
            });
        if let Err(error) = spawned {
            io.say(format!("oh-my-shell: cannot start a subshell: {error}"));
            return Ok(ExitStatus(1));
        }
        let text = self
            .current_span
            .map_or_else(|| "(subshell)".to_owned(), |span| self.span_text(span));
        self.finish_or_stop(&job, &text, io)
    }

    fn run_if(&mut self, command: &IfCommand, io: &Io) -> Flow {
        for branch in &command.branches {
            let condition =
                self.ignoring_errexit(true, |shell| shell.run_list(&branch.condition, io))?;
            if condition == ExitStatus::SUCCESS {
                return self.run_list(&branch.body, io);
            }
        }
        match &command.else_body {
            Some(body) => self.run_list(body, io),
            // No branch was taken: the status is 0.
            None => Ok(ExitStatus::SUCCESS),
        }
    }

    fn run_loop(&mut self, command: &LoopCommand, until: bool, io: &Io) -> Flow {
        self.loop_depth += 1;
        let result = self.loop_passes(command, until, io);
        self.loop_depth -= 1;
        self.leave_loop(result)
    }

    /// A stop (Ctrl+Z) leaves every loop it is in. The outermost loop ends it: from there the
    /// line goes on with the status of the stop.
    fn leave_loop(&self, result: Flow) -> Flow {
        match result {
            Err(Unwind::Stopped(status)) if self.loop_depth == 0 => Ok(status),
            other => other,
        }
    }

    fn loop_passes(&mut self, command: &LoopCommand, until: bool, io: &Io) -> Flow {
        let mut status = ExitStatus::SUCCESS;
        loop {
            if self.interrupted() {
                return Err(Unwind::Interrupted);
            }
            let condition = self
                .ignoring_errexit(true, |shell| shell.run_list(&command.condition, io))?
                == ExitStatus::SUCCESS;
            if condition == until {
                return Ok(status);
            }
            if !self.loop_body(&command.body, io, &mut status)? {
                return Ok(status);
            }
        }
    }

    /// Runs the body of a loop once. Returns false if the loop should end (`break`).
    fn loop_body(&mut self, body: &List, io: &Io, status: &mut ExitStatus) -> Result<bool, Unwind> {
        match self.run_list(body, io) {
            Ok(last) => {
                *status = last;
                Ok(true)
            }
            // `break 2` leaves this loop and one more.
            Err(Unwind::Break(n)) if n > 1 => Err(Unwind::Break(n - 1)),
            Err(Unwind::Break(_)) => Ok(false),
            Err(Unwind::Continue(n)) if n > 1 => Err(Unwind::Continue(n - 1)),
            Err(Unwind::Continue(_)) => Ok(true),
            Err(other) => Err(other),
        }
    }

    fn run_for(&mut self, command: &ForCommand, io: &Io) -> Flow {
        let items = match &command.words {
            Some(words) => self.expand_words(words, io)?,
            None => self.positional.clone(),
        };
        self.loop_depth += 1;
        let result = self.for_passes(command, items, io);
        self.loop_depth -= 1;
        self.leave_loop(result)
    }

    fn for_passes(&mut self, command: &ForCommand, items: Vec<Vec<u8>>, io: &Io) -> Flow {
        let mut status = ExitStatus::SUCCESS;
        for item in items {
            if self.interrupted() {
                return Err(Unwind::Interrupted);
            }
            if self.vars.set(&command.name, item).is_err() {
                io.say(format!("oh-my-shell: {}: is read only", command.name));
                return Err(Unwind::Error(ExitStatus::USAGE));
            }
            if !self.loop_body(&command.body, io, &mut status)? {
                break;
            }
        }
        Ok(status)
    }

    fn run_case(&mut self, command: &CaseCommand, io: &Io) -> Flow {
        let word = Expander { shell: self, io }.target(&command.word);
        let word = word.map_err(|e| self.expand_failed(&e, io))?;
        for arm in &command.arms {
            for pattern in &arm.patterns {
                let pattern = Expander { shell: self, io }.pattern(&pattern.parts);
                let pattern = pattern.map_err(|e| self.expand_failed(&e, io))?;
                if pattern.matches(&word) {
                    return self.run_list(&arm.body, io);
                }
            }
        }
        Ok(ExitStatus::SUCCESS)
    }
}
