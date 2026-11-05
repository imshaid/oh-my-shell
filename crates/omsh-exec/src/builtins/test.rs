//! `test` and `[`: conditions on files, strings and numbers.
//!
//! The grammar is POSIX's. Up to four arguments are decided by their count, as the standard
//! says, so that `[ ! ]` and `[ -n ]` (one argument) are tests of a plain string. Longer
//! expressions use `!`, `-a`, `-o` and parentheses.

use std::fs::Metadata;
use std::io::IsTerminal;
use std::os::fd::AsFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt};

use nix::unistd::{AccessFlags, access, getegid, geteuid};

use crate::exec::Flow;
use crate::io::Io;
use crate::shell::Shell;
use crate::status::ExitStatus;

/// Why an expression could not be evaluated. The status is then 2.
struct Error(String);

type Result<T> = std::result::Result<T, Error>;

/// `test args...` and `[ args... ]` (`bracket` says which, so that `]` can be required).
pub(super) fn test(shell: &Shell, args: &[Vec<u8>], io: &Io, bracket: bool) -> Flow {
    let mut args = args;
    if bracket {
        match args.split_last() {
            Some((last, rest)) if last == b"]" => args = rest,
            _ => {
                io.say("oh-my-shell: [: missing ]");
                return Ok(ExitStatus::USAGE);
            }
        }
    }
    let evaluator = Evaluator { shell, io, args };
    match evaluator.run() {
        Ok(true) => Ok(ExitStatus::SUCCESS),
        Ok(false) => Ok(ExitStatus(1)),
        Err(Error(message)) => {
            io.say(format!(
                "oh-my-shell: {}: {message}",
                if bracket { "[" } else { "test" }
            ));
            Ok(ExitStatus::USAGE)
        }
    }
}

struct Evaluator<'a> {
    shell: &'a Shell,
    io: &'a Io,
    args: &'a [Vec<u8>],
}

fn lossy(text: &[u8]) -> String {
    String::from_utf8_lossy(text).into_owned()
}

impl Evaluator<'_> {
    fn run(&self) -> Result<bool> {
        let args = self.args;
        if args.is_empty() {
            return Ok(false);
        }
        let mut pos = 0;
        let value = self.or_expr(&mut pos)?;
        if pos < args.len() {
            return Err(Error(format!("{}: unexpected operator", lossy(&args[pos]))));
        }
        Ok(value)
    }

    /// `expr -o expr`
    fn or_expr(&self, pos: &mut usize) -> Result<bool> {
        let mut value = self.and_expr(pos)?;
        while self.args.get(*pos).is_some_and(|a| a == b"-o") {
            *pos += 1;
            let right = self.and_expr(pos)?;
            value = value || right;
        }
        Ok(value)
    }

    /// `expr -a expr`
    fn and_expr(&self, pos: &mut usize) -> Result<bool> {
        let mut value = self.not_expr(pos)?;
        while self.args.get(*pos).is_some_and(|a| a == b"-a") {
            *pos += 1;
            let right = self.not_expr(pos)?;
            value = value && right;
        }
        Ok(value)
    }

    /// `! expr`. Like dash and bash, `!` takes everything after it (`! a -o b` is `! (a -o b)`).
    /// A `!` that is the last argument, or is followed by a binary operator, is a plain string.
    fn not_expr(&self, pos: &mut usize) -> Result<bool> {
        let args = self.args;
        if args.get(*pos).is_some_and(|a| a == b"!")
            && *pos + 1 < args.len()
            && !(*pos + 2 < args.len() && binary(&args[*pos + 1]))
        {
            *pos += 1;
            return Ok(!self.or_expr(pos)?);
        }
        self.primary(pos)
    }

    fn primary(&self, pos: &mut usize) -> Result<bool> {
        let args = self.args;
        let Some(first) = args.get(*pos) else {
            return Err(Error("argument expected".to_owned()));
        };
        let remaining = args.len() - *pos;
        if first == b"(" && remaining >= 2 && !(remaining >= 3 && binary(&args[*pos + 1])) {
            *pos += 1;
            let value = self.or_expr(pos)?;
            if args.get(*pos).is_some_and(|a| a == b")") {
                *pos += 1;
                return Ok(value);
            }
            return Err(Error("closing paren expected".to_owned()));
        }
        // A binary operator: `a = b`.
        if remaining >= 3 && binary(&args[*pos + 1]) {
            let (left, op, right) = (&args[*pos], &args[*pos + 1], &args[*pos + 2]);
            *pos += 3;
            return self.compare(left, op, right);
        }
        // A unary operator: `-f file`.
        if remaining >= 2 && unary(first) {
            let operand = &args[*pos + 1];
            *pos += 2;
            return self.unary_test(first, operand);
        }
        *pos += 1;
        Ok(!first.is_empty())
    }

    fn compare(&self, left: &[u8], op: &[u8], right: &[u8]) -> Result<bool> {
        Ok(match op {
            b"=" | b"==" => left == right,
            b"!=" => left != right,
            b"<" => left < right,
            b">" => left > right,
            b"-eq" | b"-ne" | b"-lt" | b"-le" | b"-gt" | b"-ge" => {
                let (a, b) = (number(left)?, number(right)?);
                match op {
                    b"-eq" => a == b,
                    b"-ne" => a != b,
                    b"-lt" => a < b,
                    b"-le" => a <= b,
                    b"-gt" => a > b,
                    _ => a >= b,
                }
            }
            b"-nt" => match (self.stat(left), self.stat(right)) {
                (Some(a), Some(b)) => (a.mtime(), a.mtime_nsec()) > (b.mtime(), b.mtime_nsec()),
                (Some(_), None) => true,
                _ => false,
            },
            b"-ot" => match (self.stat(left), self.stat(right)) {
                (Some(a), Some(b)) => (a.mtime(), a.mtime_nsec()) < (b.mtime(), b.mtime_nsec()),
                (None, Some(_)) => true,
                _ => false,
            },
            b"-ef" => match (self.stat(left), self.stat(right)) {
                (Some(a), Some(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
                _ => false,
            },
            _ => return Err(Error(format!("{}: unknown operator", lossy(op)))),
        })
    }

    /// The metadata of a file, following symbolic links.
    fn stat(&self, name: &[u8]) -> Option<Metadata> {
        std::fs::metadata(self.shell.resolve(name)).ok()
    }

    fn unary_test(&self, op: &[u8], operand: &[u8]) -> Result<bool> {
        match op {
            b"-n" => return Ok(!operand.is_empty()),
            b"-z" => return Ok(operand.is_empty()),
            b"-t" => {
                let fd = number(operand)?;
                let Ok(fd) = u32::try_from(fd) else {
                    return Ok(false);
                };
                return Ok(self
                    .io
                    .get(fd)
                    .is_some_and(|slot| slot.as_fd().is_terminal()));
            }
            b"-h" | b"-L" => {
                return Ok(std::fs::symlink_metadata(self.shell.resolve(operand))
                    .is_ok_and(|m| m.file_type().is_symlink()));
            }
            b"-r" | b"-w" | b"-x" => {
                let flag = match op {
                    b"-r" => AccessFlags::R_OK,
                    b"-w" => AccessFlags::W_OK,
                    _ => AccessFlags::X_OK,
                };
                return Ok(access(&self.shell.resolve(operand), flag).is_ok());
            }
            _ => {}
        }
        let Some(meta) = self.stat(operand) else {
            return Ok(false);
        };
        let file_type = meta.file_type();
        let mode = meta.mode();
        Ok(match op {
            b"-e" => true,
            b"-f" => file_type.is_file(),
            b"-d" => file_type.is_dir(),
            b"-b" => file_type.is_block_device(),
            b"-c" => file_type.is_char_device(),
            b"-p" => file_type.is_fifo(),
            b"-S" => file_type.is_socket(),
            b"-s" => meta.len() > 0,
            b"-g" => mode & 0o2000 != 0,
            b"-u" => mode & 0o4000 != 0,
            b"-k" => mode & 0o1000 != 0,
            b"-O" => meta.uid() == geteuid().as_raw(),
            b"-G" => meta.gid() == getegid().as_raw(),
            _ => return Err(Error(format!("{}: unknown operator", lossy(op)))),
        })
    }
}

fn binary(op: &[u8]) -> bool {
    matches!(
        op,
        b"=" | b"=="
            | b"!="
            | b"<"
            | b">"
            | b"-eq"
            | b"-ne"
            | b"-lt"
            | b"-le"
            | b"-gt"
            | b"-ge"
            | b"-nt"
            | b"-ot"
            | b"-ef"
    )
}

fn unary(op: &[u8]) -> bool {
    matches!(
        op,
        b"-b"
            | b"-c"
            | b"-d"
            | b"-e"
            | b"-f"
            | b"-g"
            | b"-h"
            | b"-k"
            | b"-L"
            | b"-n"
            | b"-p"
            | b"-r"
            | b"-S"
            | b"-s"
            | b"-t"
            | b"-u"
            | b"-w"
            | b"-x"
            | b"-z"
            | b"-O"
            | b"-G"
    )
}

/// An integer, with optional leading and trailing blanks and a sign.
fn number(text: &[u8]) -> Result<i64> {
    std::str::from_utf8(text)
        .ok()
        .and_then(|t| t.trim().parse::<i64>().ok())
        .ok_or_else(|| Error(format!("Illegal number: {}", lossy(text))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::capture::Captured;
    use crate::vars::Variables;

    fn code(args: &[&str]) -> u8 {
        let shell = Shell::with_variables(Variables::default()).subshell();
        let captured = Captured::new();
        let args: Vec<Vec<u8>> = args.iter().map(|a| a.as_bytes().to_vec()).collect();
        test(&shell, &args, &captured.io, false).unwrap().code()
    }

    #[test]
    fn the_number_of_arguments_decides_the_small_cases() {
        assert_eq!(code(&[]), 1);
        assert_eq!(code(&["x"]), 0);
        assert_eq!(code(&[""]), 1);
        assert_eq!(code(&["!"]), 0);
        assert_eq!(code(&["-n"]), 0);
        assert_eq!(code(&["!", "x"]), 1);
        assert_eq!(code(&["!", ""]), 0);
        assert_eq!(code(&["a", "=", "a"]), 0);
        assert_eq!(code(&["!", "=", "!"]), 0);
        assert_eq!(code(&["-z", ""]), 0);
        assert_eq!(code(&["!", "-z", ""]), 1);
        assert_eq!(code(&["!", "a", "=", "b"]), 0);
    }

    #[test]
    fn numbers_compare_as_integers() {
        assert_eq!(code(&["10", "-gt", "9"]), 0);
        assert_eq!(code(&["10", ">", "9"]), 1);
        assert_eq!(code(&[" 7", "-eq", "7 "]), 0);
        assert_eq!(code(&["-1", "-lt", "0"]), 0);
        assert_eq!(code(&["x", "-eq", "1"]), 2);
    }

    #[test]
    fn and_or_and_parentheses() {
        assert_eq!(code(&["a", "=", "a", "-a", "b", "!=", "c"]), 0);
        assert_eq!(code(&["a", "=", "b", "-o", "c", "=", "c"]), 0);
        assert_eq!(code(&["(", "a", "=", "b", ")", "-o", "x"]), 0);
        assert_eq!(code(&["!", "(", "a", "=", "a", ")"]), 1);
        assert_eq!(code(&["(", "a", "=", "a"]), 2);
        assert_eq!(code(&["a", "b"]), 2);
    }

    #[test]
    fn files() {
        assert_eq!(code(&["-d", "/tmp"]), 0);
        assert_eq!(code(&["-f", "/tmp"]), 1);
        assert_eq!(code(&["-e", "/nonexistent-omsh"]), 1);
        assert_eq!(code(&["-x", "/bin/sh"]), 0);
        assert_eq!(code(&["/tmp", "-ef", "/tmp/."]), 0);
        assert_eq!(code(&["-h", "/bin/sh"]), 0);
    }

    #[test]
    fn bracket_needs_its_closing_bracket() {
        let shell = Shell::with_variables(Variables::default()).subshell();
        let captured = Captured::new();
        let args = vec![b"a".to_vec()];
        assert_eq!(test(&shell, &args, &captured.io, true).unwrap().code(), 2);
        let args = vec![b"a".to_vec(), b"]".to_vec()];
        assert_eq!(test(&shell, &args, &captured.io, true).unwrap().code(), 0);
    }
}
