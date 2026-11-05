//! `echo`, in the style of bash: no escape sequences unless `-e` is given.
//!
//! dash's `echo` always reads escape sequences, which is why scripts use `printf`. People who
//! type `echo '\n'` into an interactive shell expect what bash does.

use crate::exec::Flow;
use crate::io::Io;
use crate::shell::Shell;

pub(super) fn echo(shell: &Shell, args: &[Vec<u8>], io: &Io) -> Flow {
    let mut newline = true;
    let mut escapes = false;
    let mut rest = args;
    while let Some(arg) = rest.first() {
        if !is_option(arg) {
            break;
        }
        for letter in &arg[1..] {
            match letter {
                b'n' => newline = false,
                b'e' => escapes = true,
                _ => escapes = false,
            }
        }
        rest = &rest[1..];
    }

    let mut out = Vec::new();
    for (i, arg) in rest.iter().enumerate() {
        if i > 0 {
            out.push(b' ');
        }
        if escapes {
            if !push_escaped(&mut out, arg) {
                // `\c` ends all output, including the line ending.
                newline = false;
                break;
            }
        } else {
            out.extend_from_slice(arg);
        }
    }
    if newline {
        out.push(b'\n');
    }
    shell.emit(io, 1, &out)
}

/// `-n`, `-e`, `-E` and combinations such as `-ne`.
fn is_option(arg: &[u8]) -> bool {
    arg.len() >= 2 && arg[0] == b'-' && arg[1..].iter().all(|b| matches!(b, b'n' | b'e' | b'E'))
}

/// Adds `text` with its backslash sequences replaced. Returns false at `\c`.
fn push_escaped(out: &mut Vec<u8>, text: &[u8]) -> bool {
    let mut i = 0;
    while i < text.len() {
        let b = text[i];
        i += 1;
        if b != b'\\' || i == text.len() {
            out.push(b);
            continue;
        }
        let c = text[i];
        i += 1;
        match c {
            b'\\' => out.push(b'\\'),
            b'a' => out.push(7),
            b'b' => out.push(8),
            b'c' => return false,
            b'e' | b'E' => out.push(27),
            b'f' => out.push(12),
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'v' => out.push(11),
            b'0'..=b'7' => {
                // `\0nnn` has up to three digits after the zero; `\nnn` up to two more.
                let most = if c == b'0' { 3 } else { 2 };
                let mut value = u32::from(c - b'0');
                if c == b'0' {
                    value = 0;
                }
                let mut taken = 0;
                while taken < most && i < text.len() && matches!(text[i], b'0'..=b'7') {
                    value = value * 8 + u32::from(text[i] - b'0');
                    i += 1;
                    taken += 1;
                }
                out.push((value & 0xff) as u8);
            }
            b'x' => {
                let mut value = 0_u32;
                let mut taken = 0;
                while taken < 2 && i < text.len() && text[i].is_ascii_hexdigit() {
                    value = value * 16 + (text[i] as char).to_digit(16).unwrap_or(0);
                    i += 1;
                    taken += 1;
                }
                if taken == 0 {
                    out.extend_from_slice(b"\\x");
                } else {
                    out.push((value & 0xff) as u8);
                }
            }
            other => {
                out.push(b'\\');
                out.push(other);
            }
        }
    }
    true
}
