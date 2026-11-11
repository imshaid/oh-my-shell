//! Builtin commands: they run inside the shell, because they change the shell's own state
//! (`cd`, `export`, `exit`, ...) or because starting a program for them would be wasteful.
//!
//! POSIX splits them in two. *Special* builtins are found before functions, and a variable
//! assigned in front of one stays assigned. *Regular* builtins are found after functions, and
//! behave like programs.

mod alias;
mod cd;
mod echo;
mod exec;
mod flow;
mod history;
mod jobs;
mod lookup;
mod omsh;
mod printf;
mod read;
mod source;
mod test;
mod trap;
mod umask;
mod variables;

use nix::errno::Errno;

pub(crate) use history::HistoryHook;
pub use history::{BuiltinOutput, HistoryCommand};
pub use omsh::{Key, OmshCommand, ReadError, Terminal};
pub(crate) use omsh::{OmshHook, terminal_handle};

use crate::exec::{Flow, Unwind};
use crate::io::Io;
use crate::shell::Shell;
use crate::status::ExitStatus;

/// Which kind of builtin a name is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Class {
    Special,
    Regular,
}

/// Says whether `name` is a builtin, and which kind.
pub(crate) fn classify(name: &[u8]) -> Option<Class> {
    Some(match name {
        b":" | b"." | b"break" | b"continue" | b"eval" | b"exec" | b"exit" | b"export"
        | b"return" | b"readonly" | b"set" | b"shift" | b"trap" | b"unset" => Class::Special,
        b"[" | b"alias" | b"bg" | b"cd" | b"echo" | b"command" | b"disown" | b"false" | b"fg"
        | b"hash" | b"history" | b"jobs" | b"kill" | b"omsh" | b"printf" | b"pwd" | b"read"
        | b"source" | b"test" | b"true" | b"type" | b"umask" | b"unalias" | b"wait" => {
            Class::Regular
        }
        _ => return None,
    })
}

/// True if `name` is a builtin of the shell.
#[must_use]
pub fn is_builtin(name: &str) -> bool {
    classify(name.as_bytes()).is_some() || name == crate::color::INTERNAL
}

/// Runs the builtin `name` with `args` (the words after the name). `name` must be one that
/// [`classify`] knows.
pub(crate) fn run(shell: &mut Shell, name: &[u8], args: &[Vec<u8>], io: &Io) -> Flow {
    match name {
        b":" | b"true" => Ok(ExitStatus::SUCCESS),
        b"false" => Ok(ExitStatus(1)),
        b"." | b"source" => source::dot(shell, args, io),
        b"eval" => source::eval(shell, args, io),
        b"exec" => exec::exec(shell, args, io),
        b"exit" => flow::exit(shell, args, io),
        b"return" => flow::return_(shell, args, io),
        b"break" => flow::loop_control(shell, args, io, Unwind::Break),
        b"continue" => flow::loop_control(shell, args, io, Unwind::Continue),
        b"alias" => alias::alias(shell, args, io),
        b"unalias" => alias::unalias(shell, args, io),
        b"cd" => cd::cd(shell, args, io),
        b"pwd" => cd::pwd(shell, args, io),
        b"echo" => echo::echo(shell, args, io),
        b"printf" => printf::printf(shell, args, io),
        b"type" => lookup::type_(shell, args, io),
        b"command" => lookup::command(shell, args, io),
        b"hash" => lookup::hash(shell, args, io),
        b"umask" => umask::umask(shell, args, io),
        b"history" => history::history(shell, args, io),
        b"omsh" => omsh::omsh(shell, args, io),
        b"jobs" => jobs::jobs(shell, args, io),
        b"wait" => jobs::wait(shell, args, io),
        b"fg" => jobs::fg(shell, args, io),
        b"bg" => jobs::bg(shell, args, io),
        b"disown" => jobs::disown(shell, args, io),
        b"kill" => jobs::kill_(shell, args, io),
        b"read" => read::read(shell, args, io),
        b"test" => test::test(shell, args, io, false),
        b"[" => test::test(shell, args, io, true),
        b"export" => variables::export(shell, args, io),
        b"unset" => variables::unset(shell, args, io),
        b"readonly" => variables::readonly(shell, args, io),
        b"trap" => trap::trap(shell, args, io),
        b"set" => variables::set(shell, args, io),
        b"shift" => variables::shift(shell, args, io),
        _ => {
            debug_assert!(false, "not a builtin");
            Ok(ExitStatus::NOT_FOUND)
        }
    }
}

impl Shell {
    /// Writes a builtin's output to descriptor `fd` of `io` and returns the status of the
    /// builtin. A write to a pipe whose reader has gone ends a subshell the way `SIGPIPE`
    /// ends a program: otherwise `while :; do echo y; done | head -1` would never stop.
    pub(crate) fn emit(&self, io: &Io, fd: u32, bytes: &[u8]) -> Flow {
        match io.write_all(fd, bytes) {
            Ok(()) => Ok(ExitStatus::SUCCESS),
            Err(Errno::EPIPE) if self.in_subshell => Err(Unwind::Exit(ExitStatus::from_signal(13))),
            Err(errno) => {
                io.say(format!("oh-my-shell: write error: {}", errno.desc()));
                Ok(ExitStatus(1))
            }
        }
    }
}

/// A non-negative number written as digits.
pub(crate) fn parse_count(text: &[u8]) -> Option<u32> {
    if text.is_empty() || !text.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(text).ok()?.parse().ok()
}

/// True for a name a variable can have.
pub(crate) fn is_name(name: &[u8]) -> bool {
    match name.split_first() {
        Some((first, rest)) => {
            (first.is_ascii_alphabetic() || *first == b'_')
                && rest.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'_')
        }
        None => false,
    }
}

/// `text` in single quotes, the way shells print values so that they can be read back.
pub(crate) fn quote(text: &[u8]) -> Vec<u8> {
    let mut out = vec![b'\''];
    for &b in text {
        if b == b'\'' {
            out.extend_from_slice(b"'\"'\"'");
        } else {
            out.push(b);
        }
    }
    out.push(b'\'');
    out
}
