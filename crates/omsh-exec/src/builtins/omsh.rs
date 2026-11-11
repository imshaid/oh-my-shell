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
use nix::sys::termios::{
    LocalFlags, SetArg, SpecialCharacterIndices, Termios, tcgetattr, tcsetattr,
};
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

/// One key press, as a single-key menu sees it (blueprint §20.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Enter,
    /// The Esc key alone. An escape sequence (an arrow key) is not this: it is dropped.
    Escape,
    Char(char),
}

impl Key {
    /// The key a typed line stands for, when the input is not a terminal and keys cannot be
    /// read one at a time: an empty line is Enter, otherwise its first character.
    #[must_use]
    pub fn from_line(line: &[u8]) -> Self {
        let text = String::from_utf8_lossy(line);
        match text.trim().chars().next() {
            None => Self::Enter,
            Some('\u{1b}') => Self::Escape,
            Some(c) => Self::Char(c),
        }
    }
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
    /// Prints `prompt` to standard error and waits for one key, without waiting for Enter, so
    /// that a menu can react to Esc and to letters (§20.4). Esc is told apart from an escape
    /// sequence by waiting at most 25 ms for a following byte. When input is not a terminal
    /// the answer is a line, as in [`read_line`](Self::read_line).
    ///
    /// # Errors
    ///
    /// [`ReadError`] on Ctrl+C, end of input (also Ctrl+D), or a read error.
    fn read_key(&mut self, prompt: &str) -> Result<Key, ReadError> {
        let mut line = Vec::new();
        self.read_line(prompt, false, &mut line)?;
        Ok(Key::from_line(&line))
    }
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

/// A [`Terminal`] over `io`, for code outside the `omsh` command that needs to ask a question.
pub(crate) fn terminal_handle<'a>(shell: &'a Shell, io: &'a Io) -> impl Terminal + 'a {
    Handle { shell, io }
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

    fn read_key(&mut self, prompt: &str) -> Result<Key, ReadError> {
        let Some(slot) = self.io.get(0) else {
            return Err(ReadError::Failed);
        };
        let input = slot.as_fd();
        if !input.is_terminal() {
            let mut line = Vec::new();
            self.read_line(prompt, false, &mut line)?;
            return Ok(Key::from_line(&line));
        }
        if !prompt.is_empty() {
            self.write(2, prompt);
        }
        // Line mode and echo come back when this goes out of scope, on every way out.
        let mode = KeyMode::new(input);
        let key = decode_key(&mut |wait| self.read_byte(input, wait));
        drop(mode);
        // Echo was off: show the key, and leave the cursor on a new line.
        match key {
            Ok(Key::Char(c)) => self.write(2, &format!("{c}\n")),
            Ok(Key::Enter | Key::Escape) | Err(ReadError::EndOfInput) => self.write(2, "\n"),
            Err(ReadError::Interrupted) => self.write(2, "^C\n"),
            Err(ReadError::Failed) => {}
        }
        key
    }

    fn interrupted(&self) -> bool {
        self.shell.interrupted()
    }
}

/// How long to wait for a byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wait {
    /// As long as it takes (Ctrl+C is still noticed).
    Forever,
    /// At most this many milliseconds; `None` comes back if nothing arrived.
    Millis(u16),
}

/// After Esc, how long to wait for the rest of an escape sequence before calling it the Esc
/// key (§20.4).
const ESC_WAIT_MS: u16 = 25;

/// Reads one key from a source of bytes. Escape sequences (arrow keys, Alt+key) and keys that
/// mean nothing here are dropped and the next key is read.
fn decode_key(
    read: &mut dyn FnMut(Wait) -> Result<Option<u8>, ReadError>,
) -> Result<Key, ReadError> {
    loop {
        let Some(first) = read(Wait::Forever)? else {
            continue;
        };
        match first {
            b'\r' | b'\n' => return Ok(Key::Enter),
            // Ctrl+D.
            0x04 => return Err(ReadError::EndOfInput),
            0x1b => {
                if read(Wait::Millis(ESC_WAIT_MS))?.is_none() {
                    return Ok(Key::Escape);
                }
                // A sequence: drop what follows until the line is quiet.
                while read(Wait::Millis(ESC_WAIT_MS))?.is_some() {}
            }
            0x20..=0x7e => return Ok(Key::Char(char::from(first))),
            0xc0..=0xf7 => {
                let extra = match first {
                    0xc0..=0xdf => 1,
                    0xe0..=0xef => 2,
                    _ => 3,
                };
                let mut bytes = vec![first];
                for _ in 0..extra {
                    match read(Wait::Millis(ESC_WAIT_MS))? {
                        Some(next) => bytes.push(next),
                        None => break,
                    }
                }
                if let Some(c) = std::str::from_utf8(&bytes)
                    .ok()
                    .and_then(|text| text.chars().next())
                {
                    return Ok(Key::Char(c));
                }
            }
            // Other control characters and stray bytes mean nothing.
            _ => {}
        }
    }
}

/// Reads one key at a time: line mode (`ICANON`) and echo off, `ISIG` left on so that Ctrl+C
/// still interrupts. Put back when dropped.
struct KeyMode<'a> {
    fd: BorrowedFd<'a>,
    saved: Termios,
}

impl<'a> KeyMode<'a> {
    /// `None` when the terminal cannot be changed.
    fn new(fd: BorrowedFd<'a>) -> Option<Self> {
        let saved = tcgetattr(fd).ok()?;
        let mut keys = saved.clone();
        keys.local_flags
            .remove(LocalFlags::ICANON | LocalFlags::ECHO);
        keys.control_chars[SpecialCharacterIndices::VMIN as usize] = 1;
        keys.control_chars[SpecialCharacterIndices::VTIME as usize] = 0;
        tcsetattr(fd, SetArg::TCSANOW, &keys).ok()?;
        Some(Self { fd, saved })
    }
}

impl Drop for KeyMode<'_> {
    fn drop(&mut self) {
        let _ = tcsetattr(self.fd, SetArg::TCSANOW, &self.saved);
    }
}

impl Handle<'_> {
    /// One byte from `input`. `Ok(None)` when `wait` ran out. Ctrl+C is noticed in steps.
    fn read_byte(&self, input: BorrowedFd<'_>, wait: Wait) -> Result<Option<u8>, ReadError> {
        let mut left = match wait {
            Wait::Forever => None,
            Wait::Millis(ms) => Some(ms),
        };
        let mut byte = [0_u8; 1];
        loop {
            if self.shell.interrupted() {
                return Err(ReadError::Interrupted);
            }
            let step = left.map_or(WAIT_MS, |ms| ms.min(WAIT_MS));
            let mut fds = [PollFd::new(input, PollFlags::POLLIN)];
            let ready = match poll(&mut fds, PollTimeout::from(step)) {
                Ok(0) => false,
                Err(Errno::EINTR) => continue,
                Ok(_) | Err(_) => true,
            };
            if ready {
                match read_fd(input, &mut byte) {
                    Ok(0) => return Err(ReadError::EndOfInput),
                    Ok(_) => return Ok(Some(byte[0])),
                    Err(Errno::EINTR | Errno::EAGAIN) => continue,
                    Err(_) => return Err(ReadError::Failed),
                }
            }
            if let Some(ms) = left {
                if ms <= step {
                    return Ok(None);
                }
                left = Some(ms - step);
            }
        }
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

#[cfg(test)]
mod key_tests {
    use super::*;

    /// Reads keys from scripted bytes. A `None` in the script is a wait that ran out.
    fn keys(script: &[Option<u8>]) -> Vec<Result<Key, ReadError>> {
        let mut bytes = script.iter().copied();
        let mut out = Vec::new();
        let mut read = |_wait: Wait| match bytes.next() {
            Some(byte) => Ok(byte),
            None => Err(ReadError::EndOfInput),
        };
        loop {
            let key = decode_key(&mut read);
            let end = key == Err(ReadError::EndOfInput);
            out.push(key);
            if end {
                return out;
            }
        }
    }

    fn bytes(text: &[u8]) -> Vec<Option<u8>> {
        text.iter().map(|b| Some(*b)).collect()
    }

    #[test]
    fn letters_and_enter_are_keys() {
        let got = keys(&bytes(b"y\re\n "));
        assert_eq!(got[0], Ok(Key::Char('y')));
        assert_eq!(got[1], Ok(Key::Enter));
        assert_eq!(got[2], Ok(Key::Char('e')));
        assert_eq!(got[3], Ok(Key::Enter));
        assert_eq!(got[4], Ok(Key::Char(' ')));
    }

    #[test]
    fn esc_alone_is_escape() {
        // Esc, then the wait for a following byte runs out.
        let got = keys(&[Some(0x1b), None, Some(b'q')]);
        assert_eq!(got[0], Ok(Key::Escape));
        assert_eq!(got[1], Ok(Key::Char('q')));
    }

    #[test]
    fn an_arrow_key_is_not_escape() {
        // Esc [ A (up), then the line goes quiet, then `n`.
        let got = keys(&[Some(0x1b), Some(b'['), Some(b'A'), None, Some(b'n')]);
        assert_eq!(got[0], Ok(Key::Char('n')), "{got:?}");
    }

    #[test]
    fn ctrl_d_is_the_end_of_input() {
        assert_eq!(keys(&[Some(4)])[0], Err(ReadError::EndOfInput));
    }

    #[test]
    fn other_control_keys_mean_nothing() {
        let got = keys(&[Some(0x01), Some(0x7f), Some(0x00), Some(b'y')]);
        assert_eq!(got[0], Ok(Key::Char('y')));
    }

    #[test]
    fn a_bangla_letter_is_one_key() {
        let got = keys(&bytes("ক".as_bytes()));
        assert_eq!(got[0], Ok(Key::Char('ক')));
        // A broken sequence is dropped, the next key still works.
        let got = keys(&[Some(0xe0), Some(b'a'), Some(b'b'), Some(b'y')]);
        assert!(got.contains(&Ok(Key::Char('y'))), "{got:?}");
    }

    #[test]
    fn a_typed_line_stands_for_a_key_when_input_is_not_a_terminal() {
        assert_eq!(Key::from_line(b"\n"), Key::Enter);
        assert_eq!(Key::from_line(b""), Key::Enter);
        assert_eq!(Key::from_line(b"  \n"), Key::Enter);
        assert_eq!(Key::from_line(b"yes\n"), Key::Char('y'));
        assert_eq!(Key::from_line(b"e\n"), Key::Char('e'));
        assert_eq!(Key::from_line(b"\x1b\n"), Key::Escape);
    }
}
