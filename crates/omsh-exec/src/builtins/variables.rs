//! `export`, `unset`, `set` and `shift`: the builtins that work on variables and on the
//! positional parameters.

use super::{is_name, parse_count, quote};
use crate::exec::{Flow, Unwind};
use crate::io::Io;
use crate::options::{NAMES, Options};
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
        if let Some(value) = value
            && shell.vars.set(&name, value.to_vec()).is_err()
        {
            io.say(format!("oh-my-shell: export: {name}: is read only"));
            return Err(Unwind::Error(ExitStatus::USAGE));
        }
        shell.vars.export(&name);
    }
    Ok(status)
}

/// `readonly [-p] [name[=value]...]`: makes variables read only. With no names, or `-p`, lists
/// them in a form that can be typed back.
pub(super) fn readonly(shell: &mut Shell, args: &[Vec<u8>], io: &Io) -> Flow {
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
        for (name, value) in shell.vars.readonly_sorted() {
            out.extend_from_slice(b"readonly ");
            out.extend_from_slice(name);
            if let Some(value) = value {
                out.push(b'=');
                out.extend_from_slice(&quote(value));
            }
            out.push(b'\n');
        }
        return shell.emit(io, 1, &out);
    }
    for arg in rest {
        let (name, value) = match arg.iter().position(|b| *b == b'=') {
            Some(at) => (&arg[..at], Some(&arg[at + 1..])),
            None => (arg.as_slice(), None),
        };
        if !is_name(name) {
            io.say(format!(
                "oh-my-shell: readonly: {}: not a valid name",
                String::from_utf8_lossy(name)
            ));
            return Err(Unwind::Error(ExitStatus::USAGE));
        }
        let name = String::from_utf8_lossy(name).into_owned();
        if let Some(value) = value
            && shell.vars.set(&name, value.to_vec()).is_err()
        {
            io.say(format!("oh-my-shell: readonly: {name}: is read only"));
            return Err(Unwind::Error(ExitStatus::USAGE));
        }
        shell.vars.make_readonly(&name);
    }
    Ok(ExitStatus::SUCCESS)
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
        } else if shell.vars.unset(&name).is_err() {
            io.say(format!("oh-my-shell: unset: {name}: is read only"));
            return Err(Unwind::Error(ExitStatus::USAGE));
        }
    }
    Ok(status)
}

/// `set [-+eufxC] [-+o name] [--] [arg...]`. Options come first; what is left replaces the
/// positional parameters. With no arguments at all it lists the variables, `set -o` lists the
/// options, and `set +o` lists commands that set them back.
pub(super) fn set(shell: &mut Shell, args: &[Vec<u8>], io: &Io) -> Flow {
    if args.is_empty() {
        let mut out = Vec::new();
        for (name, value, _) in shell.vars.sorted() {
            if let Some(value) = value {
                out.extend_from_slice(name);
                out.push(b'=');
                out.extend_from_slice(&quote(value));
                out.push(b'\n');
            }
        }
        return shell.emit(io, 1, &out);
    }
    let mut options = shell.options;
    let mut i = 0;
    let mut replace_positional = false;
    while i < args.len() {
        let arg = args[i].as_slice();
        if arg == b"--" {
            i += 1;
            replace_positional = true;
            break;
        }
        // A lone `-` ends the options too. (`set -` also turns off `-x` and `-v` in POSIX.)
        if arg == b"-" {
            options.xtrace = false;
            i += 1;
            // `set - a b` replaces the parameters; a lone `set -` keeps them.
            replace_positional = i < args.len();
            break;
        }
        let on = match arg.first() {
            Some(b'-') if arg.len() > 1 => true,
            Some(b'+') if arg.len() > 1 => false,
            _ => {
                replace_positional = true;
                break;
            }
        };
        i += 1;
        for &letter in &arg[1..] {
            if letter == b'o' {
                let name = args.get(i).map(Vec::as_slice);
                i += 1;
                match name {
                    None => return list_options(shell, &options, on, io),
                    Some(name) => {
                        if let Err(message) = set_named(&mut options, name, on) {
                            io.say(format!("oh-my-shell: set: {message}"));
                            return Ok(ExitStatus::USAGE);
                        }
                    }
                }
            } else if !char::from(letter).is_ascii() || !options.set_letter(char::from(letter), on)
            {
                io.say(format!(
                    "oh-my-shell: set: {}{}: option is not supported",
                    if on { '-' } else { '+' },
                    String::from_utf8_lossy(&[letter])
                ));
                return Ok(ExitStatus::USAGE);
            }
        }
    }
    shell.options = options;
    if replace_positional {
        shell.positional = args[i..].to_vec();
    }
    Ok(ExitStatus::SUCCESS)
}

/// `set -o name` and `set +o name`.
fn set_named(options: &mut Options, name: &[u8], on: bool) -> Result<(), String> {
    let shown = String::from_utf8_lossy(name).into_owned();
    if !NAMES.iter().any(|n| n.as_bytes() == name) {
        return Err(format!("{}o {shown}: unknown option", sign(on)));
    }
    if options.set_named(&shown, on) {
        Ok(())
    } else {
        Err(format!("{}o {shown}: option is not supported", sign(on)))
    }
}

fn sign(on: bool) -> char {
    if on { '-' } else { '+' }
}

/// `set -o` (a table) and `set +o` (commands that restore the options).
fn list_options(shell: &Shell, options: &Options, table: bool, io: &Io) -> Flow {
    let mut out = String::new();
    if table {
        out.push_str("Current option settings\n");
    }
    for name in NAMES {
        let on = match *name {
            "interactive" => shell.interactive,
            // The shell reads its commands from standard input.
            "stdin" => true,
            other => options.named(other),
        };
        if table {
            out.push_str(&format!("{name:<16}{}\n", if on { "on" } else { "off" }));
        } else {
            out.push_str(&format!("set {}o {name}\n", sign(on)));
        }
    }
    shell.emit(io, 1, out.as_bytes())
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
