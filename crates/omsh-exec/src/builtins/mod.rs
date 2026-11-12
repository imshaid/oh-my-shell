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
mod journal;
mod jump;
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
pub(crate) use journal::JournalHook;
pub use journal::{JournalCommand, TRASH};
pub use jump::JumpCommand;
pub(crate) use jump::JumpHook;
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

/// The special builtins (POSIX 2.14).
const SPECIAL: &[&str] = &[
    ":", ".", "break", "continue", "eval", "exec", "exit", "export", "return", "readonly", "set",
    "shift", "trap", "unset",
];

/// The regular builtins.
const REGULAR: &[&str] = &[
    "[",
    "__omsh_trash",
    "alias",
    "bg",
    "cd",
    "echo",
    "command",
    "disown",
    "false",
    "fg",
    "hash",
    "history",
    "j",
    "jobs",
    "kill",
    "omsh",
    "printf",
    "pwd",
    "read",
    "source",
    "test",
    "true",
    "type",
    "umask",
    "unalias",
    "undo",
    "wait",
];

/// Says whether `name` is a builtin, and which kind.
pub(crate) fn classify(name: &[u8]) -> Option<Class> {
    let is = |list: &[&str]| list.iter().any(|known| known.as_bytes() == name);
    if is(SPECIAL) {
        Some(Class::Special)
    } else if is(REGULAR) {
        Some(Class::Regular)
    } else {
        None
    }
}

/// True if `name` is a builtin of the shell.
#[must_use]
pub fn is_builtin(name: &str) -> bool {
    classify(name.as_bytes()).is_some() || name == crate::color::INTERNAL
}

/// The builtins a person can type, sorted: every builtin except the internal ones (names that
/// start with `__`). The completer offers these.
#[must_use]
pub fn builtin_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = SPECIAL
        .iter()
        .chain(REGULAR)
        .copied()
        .filter(|name| !name.starts_with("__"))
        .collect();
    names.sort_unstable();
    names
}

/// One line that says what the builtin `name` does, for the command palette. `None` for a name
/// that is not a builtin a person can type.
#[must_use]
pub fn summary(name: &str) -> Option<&'static str> {
    Some(match name {
        ":" => "Do nothing and succeed.",
        "." | "source" => "Run the commands of a file in this shell.",
        "[" | "test" => "Check a condition about files, text or numbers.",
        "alias" => "Define or list aliases.",
        "bg" => "Continue a stopped job in the background.",
        "break" => "Leave a loop.",
        "cd" => "Change the current folder.",
        "command" => "Run a command, skipping functions and aliases, or describe it.",
        "continue" => "Go on with the next round of a loop.",
        "disown" => "Forget a job; the shell no longer waits for it.",
        "echo" => "Print the arguments.",
        "eval" => "Run the arguments as a command line.",
        "exec" => "Replace the shell by a program.",
        "exit" => "Leave the shell.",
        "export" => "Put variables in the environment of programs.",
        "false" => "Do nothing and fail.",
        "fg" => "Bring a job to the foreground.",
        "hash" => "Remember or forget where programs are.",
        "history" => "List, search or delete stored commands.",
        "j" => "Jump to a folder you have been in before.",
        "jobs" => "List background and stopped jobs.",
        "kill" => "Send a signal to a process or job.",
        "omsh" => "The shell's own commands, such as `omsh ai setup`.",
        "printf" => "Print formatted text.",
        "pwd" => "Print the current folder.",
        "read" => "Read a line into variables.",
        "readonly" => "Make variables read-only.",
        "return" => "Leave a function or a sourced file.",
        "set" => "Set shell options and the positional arguments.",
        "shift" => "Drop the first positional arguments.",
        "trap" => "Run a command when a signal arrives or the shell exits.",
        "true" => "Do nothing and succeed.",
        "type" => "Say how a name would be run.",
        "umask" => "Show or set the mask for new files' permissions.",
        "unalias" => "Remove aliases.",
        "undo" => "Put back files that a deletion moved to the trash.",
        "unset" => "Remove variables or functions.",
        "wait" => "Wait for background jobs to end.",
        _ => return None,
    })
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
        b"j" => jump::jump(shell, args, io),
        b"omsh" => omsh::omsh(shell, args, io),
        b"__omsh_trash" => journal::trash(shell, args, io),
        b"undo" => journal::undo(shell, args, io),
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

#[cfg(test)]
mod name_tests {
    use super::*;

    #[test]
    fn every_listed_name_is_a_builtin_and_internal_ones_are_left_out() {
        let names = builtin_names();
        assert!(names.iter().all(|name| is_builtin(name)));
        assert!(names.contains(&"cd") && names.contains(&"export") && names.contains(&"["));
        assert!(!names.iter().any(|name| name.starts_with("__")));
        assert!(is_builtin("__omsh_trash"), "internal ones still run");
    }

    #[test]
    fn the_list_is_sorted_and_has_no_duplicates() {
        let names = builtin_names();
        assert!(names.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn a_name_that_is_not_a_builtin_is_refused() {
        assert!(classify(b"ls").is_none());
        assert!(classify(b"").is_none());
        assert_eq!(classify(b"export"), Some(Class::Special));
        assert_eq!(classify(b"cd"), Some(Class::Regular));
    }
}

#[cfg(test)]
mod summary_tests {
    use super::*;

    #[test]
    fn every_builtin_a_person_can_type_has_a_one_line_summary() {
        for name in builtin_names() {
            let text = summary(name).unwrap_or_else(|| panic!("no summary for {name}"));
            assert!(
                text.ends_with('.') && !text.contains('\n'),
                "{name}: {text}"
            );
            assert!(
                text.chars().count() <= 70,
                "{name} is too long for a palette row"
            );
        }
        assert_eq!(summary("__omsh_trash"), None);
        assert_eq!(summary("nope"), None);
    }
}
