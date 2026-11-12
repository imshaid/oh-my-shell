//! `history`: the builtin's place in the shell. What it does lives outside this crate.
//!
//! The executor may not depend on the history crate (the workspace's dependency direction), so
//! the program that owns the shell hands it something that implements [`HistoryCommand`], and
//! the builtin passes its arguments on.

use std::fmt;
use std::io::IsTerminal;
use std::os::fd::AsFd;
use std::sync::Arc;

use crate::exec::Flow;
use crate::io::Io;
use crate::shell::Shell;
use crate::status::ExitStatus;

/// What a `history` command gave back.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BuiltinOutput {
    /// For standard output.
    pub out: Vec<u8>,
    /// For standard error. Whole lines.
    pub err: Vec<u8>,
    pub status: u8,
}

/// Runs the `history` builtin for the shell.
pub trait HistoryCommand: Send + Sync {
    /// `args` are the words after `history`, `cwd` is the shell's working directory, and
    /// `to_terminal` says whether the output goes to a terminal.
    ///
    /// A long listing is not built in memory: it is passed to `sink` in pieces as it is read, and
    /// the listing stops when `sink` returns `false` (the reader of the pipe has gone). Whatever
    /// is in the returned `out` is written after that, as before.
    fn run(
        &self,
        args: &[Vec<u8>],
        cwd: &[u8],
        to_terminal: bool,
        sink: &mut dyn FnMut(&[u8]) -> bool,
    ) -> BuiltinOutput;

    /// Called just before `exec command` replaces the whole shell. The process image is gone
    /// the moment it succeeds, and with it any thread that has not written yet, so a store
    /// that writes in the background must catch up here. The call may fail to replace the
    /// shell (no such program): the shell then goes on. The default does nothing.
    fn before_replace(&self) {}
}

/// The shell's handle on its history command. Shared by every copy of the shell.
#[derive(Clone, Default)]
pub(crate) struct HistoryHook(pub(crate) Option<Arc<dyn HistoryCommand>>);

impl fmt::Debug for HistoryHook {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if self.0.is_some() {
            "HistoryHook(set)"
        } else {
            "HistoryHook(none)"
        })
    }
}

impl Shell {
    /// Gives the shell its `history` builtin. Without one, `history` says that there is no
    /// history in this shell.
    pub fn set_history(&mut self, command: Arc<dyn HistoryCommand>) {
        self.history = HistoryHook(Some(command));
    }
}

pub(super) fn history(shell: &mut Shell, args: &[Vec<u8>], io: &Io) -> Flow {
    let Some(command) = shell.history.0.clone() else {
        io.say("oh-my-shell: history: not available in this shell");
        return Ok(ExitStatus(1));
    };
    let cwd = shell.logical_cwd();
    let to_terminal = io.get(1).is_some_and(|slot| slot.as_fd().is_terminal());
    // The first write that fails (or ends a subshell) stops the listing; its outcome is returned.
    let mut stopped: Option<Flow> = None;
    let output = command.run(
        args,
        cwd.as_os_str().as_encoded_bytes(),
        to_terminal,
        &mut |piece| match shell.emit(io, 1, piece) {
            Ok(status) if status == ExitStatus::SUCCESS => true,
            other => {
                stopped = Some(other);
                false
            }
        },
    );
    if let Some(outcome) = stopped {
        return outcome;
    }
    if !output.err.is_empty() {
        io.say(output.err.strip_suffix(b"\n").unwrap_or(&output.err));
    }
    if !output.out.is_empty() {
        let written = shell.emit(io, 1, &output.out)?;
        if written != ExitStatus::SUCCESS {
            return Ok(written);
        }
    }
    Ok(ExitStatus(output.status))
}
