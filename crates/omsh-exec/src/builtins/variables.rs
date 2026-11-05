//! `export`, `unset`, `set` and `shift`: the builtins that work on variables and on the
//! positional parameters.

use super::{is_name, parse_count, quote};
use crate::exec::Flow;
use crate::io::Io;
use crate::shell::Shell;
use crate::status::ExitStatus;

/// `export [-p] [name[=value]...]`.
pub(super) fn export(shell: &mut Shell, args: &[Vec<u8>], io: &Io) -> Flow {
    let mut rest = args;
    let mut list = false;
    while let Some(arg) = rest.first() {
        match arg.as_slice() {
            b"-p" => list = true,
            b"--" => {
                rest = &rest[1..];
                break;
            }
            _ => break,
        }
        rest = &rest[1..];
    }
    if list || rest.is_empty() {
        let mut out = Vec::new();
        for (name, value, exported) in shell.vars.sorted() {
            if !exported {
                continue;
            }
            out.extend_from_slice(b"export ");
            out.extend_from_slice(name);
            if let Some(value) = value {
                out.push(b'=');
                out.extend_from_slice(&quote(value));
            }
            out.push(b'\n');
        }
        return shell.emit(io, 1, &out);
    }

    let mut status = ExitStatus::SUCCESS;
    for arg in rest {
        let (name, value) = match arg.iter().position(|b| *b == b'=') {
            Some(at) => (&arg[..at], Some(&arg[at + 1..])),
            None => (arg.as_slice(), None),
        };
        if !is_name(name) {
            io.say(format!(
                "oh-my-shell: export: {}: not a valid name",
                String::from_utf8_lossy(name)
            ));
            status = ExitStatus(1);
            continue;
        }
        // `is_name` only accepts ASCII, so this is valid UTF-8.
        let name = String::from_utf8_lossy(name).into_owned();
        if let Some(value) = value {
            shell.vars.set(&name, value.to_vec());
        }
        shell.vars.export(&name);
    }
    Ok(status)
}

/// `unset [-v|-f] name...`.
pub(super) fn unset(shell: &mut Shell, args: &[Vec<u8>], io: &Io) -> Flow {
    let mut functions = false;
    let mut rest = args;
    while let Some(arg) = rest.first() {
        match arg.as_slice() {
            b"-f" => functions = true,
            b"-v" => functions = false,
            b"--" => {
                rest = &rest[1..];
                break;
            }
            _ => break,
        }
        rest = &rest[1..];
    }
    let mut status = ExitStatus::SUCCESS;
    for arg in rest {
        if !is_name(arg) {
            io.say(format!(
                "oh-my-shell: unset: {}: not a valid name",
                String::from_utf8_lossy(arg)
            ));
            status = ExitStatus(1);
            continue;
        }
        let name = String::from_utf8_lossy(arg).into_owned();
        if functions {
            shell.functions.remove(&name);
        } else {
            shell.vars.unset(&name);
        }
    }
    Ok(status)
}

/// `set [--] [arg...]`. With arguments it replaces the positional parameters. With none it
/// lists the variables. Options such as `-e` come in S07b.
pub(super) fn set(shell: &mut Shell, args: &[Vec<u8>], io: &Io) -> Flow {
    match args.first().map(Vec::as_slice) {
        None => {
            let mut out = Vec::new();
            for (name, value, _) in shell.vars.sorted() {
                if let Some(value) = value {
                    out.extend_from_slice(name);
                    out.push(b'=');
                    out.extend_from_slice(&quote(value));
                    out.push(b'\n');
                }
            }
            shell.emit(io, 1, &out)
        }
        Some(b"--") => {
            shell.positional = args[1..].to_vec();
            Ok(ExitStatus::SUCCESS)
        }
        Some(first) if first.len() > 1 && matches!(first[0], b'-' | b'+') => {
            io.say(format!(
                "oh-my-shell: set: {}: options are not supported yet",
                String::from_utf8_lossy(first)
            ));
            Ok(ExitStatus::USAGE)
        }
        Some(_) => {
            shell.positional = args.to_vec();
            Ok(ExitStatus::SUCCESS)
        }
    }
}

/// `shift [n]`.
pub(super) fn shift(shell: &mut Shell, args: &[Vec<u8>], io: &Io) -> Flow {
    let count = match args.first() {
        None => 1,
        Some(arg) => match parse_count(arg) {
            Some(n) => n as usize,
            None => {
                io.say(format!(
                    "oh-my-shell: shift: Illegal number: {}",
                    String::from_utf8_lossy(arg)
                ));
                return Ok(ExitStatus::USAGE);
            }
        },
    };
    if count > shell.positional.len() {
        io.say("oh-my-shell: shift: can't shift that many");
        return Ok(ExitStatus(1));
    }
    shell.positional.drain(..count);
    Ok(ExitStatus::SUCCESS)
}
