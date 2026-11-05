//! `trap`: commands that run when the shell receives a signal or exits.
//!
//! A signal can arrive at any moment, and almost nothing is safe to do inside its handler. So
//! the handler only sets a bit (see `signals.rs`), and the executor runs the commands of the
//! signals that arrived between two commands: [`Shell::run_pending_traps`].

use std::collections::BTreeMap;

use crate::exec::{Flow, Unwind, boundary};
use crate::io::Io;
use crate::shell::Shell;
use crate::signals;
use crate::status::ExitStatus;

/// The signals `trap` accepts: name, number. `EXIT` (0) is not a signal: it is the end of
/// the shell. The others that need job control (`CHLD`, `TSTP`, `TTIN`, `TTOU`) come with
/// S08 and S09.
pub(crate) const SIGNALS: &[(&str, i32)] = &[
    ("EXIT", 0),
    ("HUP", 1),
    ("INT", 2),
    ("QUIT", 3),
    ("USR1", 10),
    ("USR2", 12),
    ("ALRM", 14),
    ("TERM", 15),
];

/// What to do on a signal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    /// `trap '' SIG`.
    Ignore,
    /// `trap 'command' SIG`.
    Command(String),
}

/// The traps that are set. A signal that is not here has its default behavior.
#[derive(Clone, Debug, Default)]
pub(crate) struct Traps {
    actions: BTreeMap<i32, Action>,
}

impl Traps {
    pub(crate) fn set(&mut self, signal: i32, action: Option<Action>) {
        match action {
            Some(action) => {
                self.actions.insert(signal, action);
            }
            None => {
                self.actions.remove(&signal);
            }
        }
    }

    pub(crate) fn get(&self, signal: i32) -> Option<&Action> {
        self.actions.get(&signal)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (i32, &Action)> {
        self.actions
            .iter()
            .map(|(signal, action)| (*signal, action))
    }

    /// What a subshell starts with: the commands are forgotten, ignored signals stay ignored.
    pub(crate) fn for_subshell(&self) -> Self {
        Self {
            actions: self
                .actions
                .iter()
                .filter(|(_, action)| **action == Action::Ignore)
                .map(|(signal, action)| (*signal, action.clone()))
                .collect(),
        }
    }

    /// Bit `n` is set for each signal `n` that is ignored by a trap.
    pub(crate) fn ignored_mask(&self) -> u64 {
        self.actions
            .iter()
            .filter(|(signal, action)| **action == Action::Ignore && **signal > 0)
            .fold(0, |mask, (signal, _)| mask | (1_u64 << signal))
    }
}

/// The name of a signal as `trap` shows it.
pub(crate) fn signal_name(signal: i32) -> String {
    SIGNALS
        .iter()
        .find(|(_, number)| *number == signal)
        .map_or_else(|| signal.to_string(), |(name, _)| (*name).to_owned())
}

impl Shell {
    /// Runs the commands of the signals that arrived since the last check. Only the main shell
    /// does this: a signal belongs to the whole process, and a copy of the shell (a thread)
    /// must not take it away from the main shell. `$?` is the same after a trap as before.
    pub(crate) fn run_pending_traps(&mut self, io: &Io) -> Result<(), Unwind> {
        if self.in_subshell || self.running_trap || !signals::trap_pending() {
            return Ok(());
        }
        let arrived = signals::take_pending();
        for signal in 1..64 {
            if arrived & (1_u64 << signal) == 0 {
                continue;
            }
            let Some(Action::Command(text)) = self.traps.get(signal).cloned() else {
                continue;
            };
            self.running_trap = true;
            let saved = self.status;
            let flow = self.run_text(&text, "trap", io);
            self.running_trap = false;
            self.status = saved;
            match flow {
                // A `return` or `break` in the command ends nothing outside of it.
                Err(Unwind::Return(_) | Unwind::Break(_) | Unwind::Continue(_)) | Ok(_) => {}
                Err(other) => return Err(other),
            }
        }
        Ok(())
    }

    /// Runs the `EXIT` trap, once. Call it when the shell is about to end with `status`.
    /// Returns the status the shell should end with: the given one, unless the command calls
    /// `exit`.
    pub fn run_exit_trap(&mut self, status: ExitStatus, io: &Io) -> ExitStatus {
        let Some(Action::Command(text)) = self.traps.get(0).cloned() else {
            return status;
        };
        self.traps.set(0, None);
        self.status = status;
        match self.run_text(&text, "trap", io) {
            Err(Unwind::Exit(code)) => code,
            _ => status,
        }
    }

    /// The end of a copy of the shell (a subshell, a pipeline stage, a `$(...)`): its `EXIT`
    /// trap runs, and the status it ends with comes out.
    pub(crate) fn finish_copy(&mut self, flow: Flow, io: &Io) -> ExitStatus {
        let status = boundary(flow);
        self.run_exit_trap(status, io)
    }
}
