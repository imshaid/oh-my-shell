//! `umask [-S] [mode]`: the permissions that new files do not get.
//!
//! The file creation mask belongs to the whole process, which has other threads running in a
//! pipeline. The main shell sets the real mask. A copy of the shell only records the new value
//! (see `Shell::umask_override`), and uses it for the programs it starts and the files it
//! creates.

use nix::sys::stat::{Mode, umask as set_process_umask};

use crate::exec::Flow;
use crate::io::Io;
use crate::shell::Shell;
use crate::status::ExitStatus;

impl Shell {
    /// The mask in effect for this shell.
    pub(crate) fn current_umask(&self) -> u32 {
        self.umask_override.unwrap_or_else(process_umask)
    }

    fn set_umask(&mut self, mask: u32) {
        if self.in_subshell {
            self.umask_override = Some(mask);
        } else {
            // Setting the mask cannot fail. It returns the old one, which is not needed.
            let _ = set_process_umask(Mode::from_bits_truncate(mask));
        }
    }
}

/// The mask of this process, read from `/proc`. (The usual way, `umask(0)` and then putting
/// the old value back, would let a file made by another thread in between get the wrong
/// permissions.)
fn process_umask() -> u32 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find_map(|line| line.strip_prefix("Umask:"))
                .and_then(|value| u32::from_str_radix(value.trim(), 8).ok())
        })
        .unwrap_or(0o022)
}

/// `umask`.
pub(super) fn umask(shell: &mut Shell, args: &[Vec<u8>], io: &Io) -> Flow {
    let mut symbolic = false;
    let mut rest = args;
    while let Some(arg) = rest.first() {
        match arg.as_slice() {
            b"-S" => symbolic = true,
            b"--" => {
                rest = &rest[1..];
                break;
            }
            other if other.len() > 1 && other[0] == b'-' => {
                io.say(format!(
                    "oh-my-shell: umask: Illegal option {}",
                    String::from_utf8_lossy(other)
                ));
                return Ok(ExitStatus::USAGE);
            }
            _ => break,
        }
        rest = &rest[1..];
    }
    let current = shell.current_umask();
    let Some(mode) = rest.first() else {
        let line = if symbolic {
            format!("{}\n", describe(current))
        } else {
            format!("{current:04o}\n")
        };
        return shell.emit(io, 1, line.as_bytes());
    };
    match parse_mode(mode, current) {
        Ok(mask) => {
            shell.set_umask(mask);
            Ok(ExitStatus::SUCCESS)
        }
        Err(message) => {
            io.say(format!("oh-my-shell: umask: {message}"));
            Ok(ExitStatus::USAGE)
        }
    }
}

/// `u=rwx,g=rx,o=` for the permissions that the mask leaves.
fn describe(mask: u32) -> String {
    let allowed = !mask & 0o777;
    let part = |shift: u32| -> String {
        let bits = (allowed >> shift) & 7;
        [(4, 'r'), (2, 'w'), (1, 'x')]
            .iter()
            .filter(|(bit, _)| bits & bit != 0)
            .map(|(_, letter)| *letter)
            .collect()
    };
    format!("u={},g={},o={}", part(6), part(3), part(0))
}

/// Reads the argument: octal digits, or a symbolic mode that changes the permissions the mask
/// currently leaves (`u=rwx,g=rx,o=`, `g+w`, `o-rwx`). Returns the new mask.
fn parse_mode(text: &[u8], current: u32) -> Result<u32, String> {
    let shown = String::from_utf8_lossy(text).into_owned();
    if text.first().is_some_and(u8::is_ascii_digit) {
        return std::str::from_utf8(text)
            .ok()
            .and_then(|t| u32::from_str_radix(t, 8).ok())
            .filter(|m| *m <= 0o777)
            .ok_or_else(|| format!("Illegal number: {shown}"));
    }
    let illegal = || format!("Illegal mode: {shown}");
    let mut allowed = !current & 0o777;
    for clause in text.split(|b| *b == b',') {
        let op_at = clause
            .iter()
            .position(|b| matches!(b, b'=' | b'+' | b'-'))
            .ok_or_else(illegal)?;
        let mut who = 0_u32;
        for letter in &clause[..op_at] {
            who |= match letter {
                b'u' => 0o700,
                b'g' => 0o070,
                b'o' => 0o007,
                b'a' => 0o777,
                _ => return Err(illegal()),
            };
        }
        if who == 0 {
            who = 0o777;
        }
        let mut bits = 0_u32;
        for letter in &clause[op_at + 1..] {
            bits |= match letter {
                b'r' => 0o444,
                b'w' => 0o222,
                b'x' => 0o111,
                _ => return Err(illegal()),
            };
        }
        bits &= who;
        match clause[op_at] {
            b'=' => allowed = (allowed & !who) | bits,
            b'+' => allowed |= bits,
            _ => allowed &= !bits,
        }
    }
    Ok(!allowed & 0o777)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn octal_and_symbolic_modes() {
        assert_eq!(parse_mode(b"027", 0o022), Ok(0o027));
        assert_eq!(parse_mode(b"0", 0o022), Ok(0));
        assert_eq!(parse_mode(b"u=rwx,g=rx,o=", 0o022), Ok(0o027));
        assert_eq!(parse_mode(b"g+w", 0o027), Ok(0o007));
        assert_eq!(parse_mode(b"a=", 0o022), Ok(0o777));
        assert_eq!(parse_mode(b"o-rwx", 0o002), Ok(0o007));
        assert!(parse_mode(b"999", 0).is_err());
        assert!(parse_mode(b"0777", 0).is_ok());
        assert!(parse_mode(b"1000", 0).is_err());
        assert!(parse_mode(b"q=r", 0).is_err());
        assert!(parse_mode(b"u", 0).is_err());
    }

    #[test]
    fn the_symbolic_form_lists_what_the_mask_leaves() {
        assert_eq!(describe(0o022), "u=rwx,g=rx,o=rx");
        assert_eq!(describe(0o777), "u=,g=,o=");
        assert_eq!(describe(0), "u=rwx,g=rwx,o=rwx");
    }
}
