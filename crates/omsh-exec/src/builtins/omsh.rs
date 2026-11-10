//! `omsh`: the shell's own command, `omsh ai setup` and friends.
//!
//! Like `history`, the builtin is only a place in the shell. The executor may not depend on
//! the AI crates (the workspace's dependency direction), so the program that owns the shell
//! hands it something that implements [`OmshCommand`], and the builtin passes its arguments on
//! together with a [`Terminal`]: a small handle for talking to the person at the keyboard
//! (print, ask a question, ask for a secret without echo) that also knows about Ctrl+C.

use std::fmt;
use std::io::IsTerminal;
use std::os::fd::{AsFd, BorrowedFd};
use std::sync::Arc;

use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::sys::termios::{LocalFlags, SetArg, Termios, tcgetattr, tcsetattr};
use nix::unistd::read as read_fd;

use crate::exec::{Flow, Unwind};
use crate::io::Io;
use crate::shell::Shell;
use crate::status::ExitStatus;

/// Why a line could not be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadError {
    /// Ctrl+C.
    Interrupted,
    /// The input ended before a line did (Ctrl+D, or a closed pipe).
    EndOfInput,
    /// There is no standard input, or reading it failed.
    Failed,
}

/// What a command of `omsh` can do with the keyboard and the screen.
pub trait Terminal {
    /// Whether standard input is a terminal: a wizard needs a person to answer it.
    fn input_is_terminal(&self) -> bool;
    /// Writes to standard output.
    fn out(&mut self, text: &str);
    /// Writes to standard error. Questions and progress go here, as with `read -p`.
    fn err(&mut self, text: &str);
    /// Prints `prompt` to standard error and reads one line from standard input into `line`
    /// (cleared first), without its line ending. With `hidden` nothing is echoed while it is
    /// typed, for secrets; a line ending is then printed so the next output starts on a new
    /// line. The caller owns `line`, so a secret can be wiped by whoever holds it.
    ///
    /// # Errors
    ///
    /// [`ReadError`] on Ctrl+C, end of input, or a read error.
    fn read_line(
        &mut self,
        prompt: &str,
        hidden: bool,
        line: &mut Vec<u8>,
    ) -> Result<(), ReadError>;
    /// Whether Ctrl+C was pressed. Checked while waiting for the network.
    fn interrupted(&self) -> bool;
}

/// What the `omsh` builtin runs. Implemented by the program that owns the shell.
pub trait OmshCommand: Send + Sync {
    /// `args` are the words after `omsh`. Returns the exit status.
    fn run(&self, args: &[Vec<u8>], terminal: &mut dyn Terminal) -> u8;
}

/// The shell's handle on its `omsh` command. Shared by every copy of the shell.
#[derive(Clone, Default)]
pub(crate) struct OmshHook(pub(crate) Option<Arc<dyn OmshCommand>>);

impl fmt::Debug for OmshHook {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if self.0.is_some() {
            "OmshHook(set)"
        } else {
            "OmshHook(none)"
        })
    }
}

impl Shell {
    /// Gives the shell its `omsh` builtin. Without one, `omsh` says it is not available.
    pub fn set_omsh(&mut self, command: Arc<dyn OmshCommand>) {
        self.omsh = OmshHook(Some(command));
    }
}

/// How long to wait for input before looking at the Ctrl+C flag again (see `read.rs`).
const WAIT_MS: u16 = 100;

pub(super) fn omsh(shell: &mut Shell, args: &[Vec<u8>], io: &Io) -> Flow {
    let Some(command) = shell.omsh.0.clone() else {
        io.say("oh-my-shell: omsh: not available in this shell");
        return Ok(ExitStatus(1));
    };
    let status = command.run(args, &mut Handle { shell, io });
    // Ctrl+C during the command ends the line, as it does for `read`.
    if shell.interrupted() {
        return Err(Unwind::Interrupted);
    }
    Ok(ExitStatus(status))
}

/// The [`Terminal`] of one `omsh` command: the shell's table of files for that command.
struct Handle<'a> {
    shell: &'a Shell,
    io: &'a Io,
}

impl Handle<'_> {
    fn write(&self, fd: u32, text: &str) {
        // A closed output is not the command's problem to report.
        let _ = self.io.write_all(fd, text.as_bytes());
    }
}

impl Terminal for Handle<'_> {
    fn input_is_terminal(&self) -> bool {
        self.io
            .get(0)
            .is_some_and(|slot| slot.as_fd().is_terminal())
    }

    fn out(&mut self, text: &str) {
        self.write(1, text);
    }

    fn err(&mut self, text: &str) {
        self.write(2, text);
    }

    fn read_line(
        &mut self,
        prompt: &str,
        hidden: bool,
        line: &mut Vec<u8>,
    ) -> Result<(), ReadError> {
        line.clear();
        let Some(slot) = self.io.get(0) else {
            return Err(ReadError::Failed);
        };
        let input = slot.as_fd();
        if !prompt.is_empty() {
            self.write(2, prompt);
        }
        // Echo comes back when this goes out of scope, on every way out.
        let echo_guard = if hidden { EchoOff::new(input) } else { None };
        let result = self.read_bytes(input, line);
        if echo_guard.is_some() {
            // The person's Enter was not shown, so the cursor is still on the prompt's line.
            self.write(2, "\n");
        }
        drop(echo_guard);
        result
    }

    fn interrupted(&self) -> bool {
        self.shell.interrupted()
    }
}

impl Handle<'_> {
    /// Reads up to a line ending, one byte at a time, so nothing after the line is taken from
    /// a program that reads the same input later.
    fn read_bytes(&self, input: BorrowedFd<'_>, line: &mut Vec<u8>) -> Result<(), ReadError> {
        let mut byte = [0_u8; 1];
        loop {
            if self.shell.interrupted() {
                return Err(ReadError::Interrupted);
            }
            let mut fds = [PollFd::new(input, PollFlags::POLLIN)];
            match poll(&mut fds, PollTimeout::from(WAIT_MS)) {
                Ok(0) | Err(Errno::EINTR) => continue,
                // An error here shows up again in `read`.
                Ok(_) | Err(_) => {}
            }
            match read_fd(input, &mut byte) {
                Ok(0) => {
                    // A last line with no line ending still counts.
                    return if line.is_empty() {
                        Err(ReadError::EndOfInput)
                    } else {
                        Ok(())
                    };
                }
                Ok(_) if byte[0] == b'\n' => return Ok(()),
                Ok(_) => line.push(byte[0]),
                Err(Errno::EINTR | Errno::EAGAIN) => {}
                Err(_) => return Err(ReadError::Failed),
            }
        }
    }
}

/// Turns terminal echo off, and back on when dropped.
struct EchoOff<'a> {
    fd: BorrowedFd<'a>,
    saved: Termios,
}

impl<'a> EchoOff<'a> {
    /// `None` when `fd` is not a terminal (a pipe has no echo to turn off).
    fn new(fd: BorrowedFd<'a>) -> Option<Self> {
        let saved = tcgetattr(fd).ok()?;
        let mut quiet = saved.clone();
        quiet.local_flags.remove(LocalFlags::ECHO);
        tcsetattr(fd, SetArg::TCSANOW, &quiet).ok()?;
        Some(Self { fd, saved })
    }
}

impl Drop for EchoOff<'_> {
    fn drop(&mut self) {
        let _ = tcsetattr(self.fd, SetArg::TCSANOW, &self.saved);
    }
}
