//! Compound commands: `( )`, `{ }`, `if`, `while`, `until`, `for`, `case`, and function
//! definitions.

use std::sync::Arc;

use omsh_syntax::ast::{CaseCommand, CommandKind, ForCommand, IfCommand, List, LoopCommand};

use super::{Flow, Unwind, boundary};
use crate::expand::Expander;
use crate::io::Io;
use crate::shell::Shell;
use crate::signals;
use crate::status::ExitStatus;

impl Shell {
    /// Runs a command that is not a simple command. Its redirections are already in `io`.
    pub(super) fn run_compound(&mut self, kind: &CommandKind, io: &Io) -> Flow {
        match kind {
            CommandKind::Simple(_) => {
                debug_assert!(false, "simple commands do not come here");
                Ok(self.status)
            }
            CommandKind::Subshell(list) => {
                let mut copy = self.subshell();
                Ok(boundary(copy.run_list(list, io)))
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

    fn run_if(&mut self, command: &IfCommand, io: &Io) -> Flow {
        for branch in &command.branches {
            if self.run_list(&branch.condition, io)? == ExitStatus::SUCCESS {
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
        result
    }

    fn loop_passes(&mut self, command: &LoopCommand, until: bool, io: &Io) -> Flow {
        let mut status = ExitStatus::SUCCESS;
        loop {
            if signals::interrupted() {
                return Err(Unwind::Interrupted);
            }
            let condition = self.run_list(&command.condition, io)? == ExitStatus::SUCCESS;
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
        result
    }

    fn for_passes(&mut self, command: &ForCommand, items: Vec<Vec<u8>>, io: &Io) -> Flow {
        let mut status = ExitStatus::SUCCESS;
        for item in items {
            if signals::interrupted() {
                return Err(Unwind::Interrupted);
            }
            self.vars.set(&command.name, item);
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
