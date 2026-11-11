//! `undo` and the internal `__omsh_trash`: the builtins' place in the shell (S18).
//!
//! Like `history` and `omsh`, they are only a place here. The executor may not depend on the
//! safety crate (the workspace's dependency direction), so the program that owns the shell
//! hands it something that implements [`JournalCommand`].
//!
//! `__omsh_trash` is what an `rm` becomes when it goes through the trash: the shell replaces
//! the word `rm` by it in the parsed line, and expansion then gives it exactly the words `rm`
//! would have got. Without a journal it deletes **nothing** and says so: it never falls back
//! to a real `rm`.

use std::fmt;
use std::sync::Arc;

use crate::builtins::omsh::{Terminal, terminal_handle};
use crate::exec::{Flow, Unwind};
use crate::io::Io;
use crate::shell::Shell;
use crate::status::ExitStatus;

/// The name of the internal builtin that an `rm` is replaced by.
pub const TRASH: &str = "__omsh_trash";

/// What the two builtins do. Implemented by the program that owns the shell.
pub trait JournalCommand: Send + Sync {
    /// `__omsh_trash`: moves the files named by `args` (the words of an `rm`) to the trash.
    /// `cwd` is the shell's working directory. Returns the exit status.
    fn trash(&self, args: &[Vec<u8>], cwd: &[u8], terminal: &mut dyn Terminal) -> u8;
    /// `undo [--list | ID]`. Returns the exit status.
    fn undo(&self, args: &[Vec<u8>], cwd: &[u8], terminal: &mut dyn Terminal) -> u8;
}

/// The shell's handle on its journal command. Shared by every copy of the shell.
#[derive(Clone, Default)]
pub(crate) struct JournalHook(pub(crate) Option<Arc<dyn JournalCommand>>);

impl fmt::Debug for JournalHook {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if self.0.is_some() {
            "JournalHook(set)"
        } else {
            "JournalHook(none)"
        })
    }
}

impl Shell {
    /// Gives the shell its trash and `undo`. Without one, `undo` says it is not available and
    /// `__omsh_trash` deletes nothing.
    pub fn set_journal(&mut self, command: Arc<dyn JournalCommand>) {
        self.journal = JournalHook(Some(command));
    }

    /// True if this shell has a trash to send files to.
    #[must_use]
    pub fn has_journal(&self) -> bool {
        self.journal.0.is_some()
    }
}

pub(super) fn trash(shell: &mut Shell, args: &[Vec<u8>], io: &Io) -> Flow {
    let Some(command) = shell.journal.0.clone() else {
        io.say("oh-my-shell: the trash is not available in this shell; nothing was deleted");
        return Ok(ExitStatus(1));
    };
    run(shell, io, |cwd, terminal| {
        command.trash(args, cwd, terminal)
    })
}

pub(super) fn undo(shell: &mut Shell, args: &[Vec<u8>], io: &Io) -> Flow {
    let Some(command) = shell.journal.0.clone() else {
        io.say("oh-my-shell: undo: not available in this shell");
        return Ok(ExitStatus(1));
    };
    run(shell, io, |cwd, terminal| command.undo(args, cwd, terminal))
}

fn run(shell: &Shell, io: &Io, call: impl FnOnce(&[u8], &mut dyn Terminal) -> u8) -> Flow {
    let cwd = shell.logical_cwd();
    let status = call(
        cwd.as_os_str().as_encoded_bytes(),
        &mut terminal_handle(shell, io),
    );
    // Ctrl+C during the command ends the line, as it does for `read`.
    if shell.interrupted() {
        return Err(Unwind::Interrupted);
    }
    Ok(ExitStatus(status))
}
