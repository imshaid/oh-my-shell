//! `alias` and `unalias`.

use super::quote;
use crate::alias::is_alias_name;
use crate::exec::Flow;
use crate::io::Io;
use crate::shell::Shell;
use crate::status::ExitStatus;

/// `alias [name[=value]...]`: with no arguments, lists every alias. `name` prints one, and
/// `name=value` defines one.
pub(super) fn alias(shell: &mut Shell, args: &[Vec<u8>], io: &Io) -> Flow {
    let args = match args.first().map(Vec::as_slice) {
        Some(b"--") => &args[1..],
        _ => args,
    };
    if args.is_empty() {
        let mut out = Vec::new();
        for (name, value) in &shell.aliases {
            line(&mut out, name, value);
        }
        return shell.emit(io, 1, &out);
    }
    let mut status = ExitStatus::SUCCESS;
    let mut out = Vec::new();
    for arg in args {
        let text = String::from_utf8_lossy(arg).into_owned();
        match text.split_once('=') {
            Some((name, value)) => {
                if is_alias_name(name) {
                    shell.aliases.insert(name.to_owned(), value.to_owned());
                } else {
                    io.say(format!("oh-my-shell: alias: {name}: invalid alias name"));
                    status = ExitStatus(1);
                }
            }
            None => match shell.aliases.get(&text) {
                Some(value) => line(&mut out, &text, value),
                None => {
                    io.say(format!("oh-my-shell: alias: {text}: not found"));
                    status = ExitStatus(1);
                }
            },
        }
    }
    shell.emit(io, 1, &out)?;
    Ok(status)
}

/// One line of the listing, in the form that can be typed back: `name='value'`.
fn line(out: &mut Vec<u8>, name: &str, value: &str) {
    out.extend_from_slice(name.as_bytes());
    out.push(b'=');
    out.extend_from_slice(&quote(value.as_bytes()));
    out.push(b'\n');
}

/// `unalias [-a] name...`.
pub(super) fn unalias(shell: &mut Shell, args: &[Vec<u8>], io: &Io) -> Flow {
    let mut rest = args;
    let mut all = false;
    while let Some(arg) = rest.first() {
        match arg.as_slice() {
            b"-a" => all = true,
            b"--" => {
                rest = &rest[1..];
                break;
            }
            _ => break,
        }
        rest = &rest[1..];
    }
    if all {
        shell.aliases.clear();
        return Ok(ExitStatus::SUCCESS);
    }
    if rest.is_empty() {
        io.say("oh-my-shell: unalias: usage: unalias [-a] name [name ...]");
        return Ok(ExitStatus::USAGE);
    }
    let mut status = ExitStatus::SUCCESS;
    for arg in rest {
        let name = String::from_utf8_lossy(arg).into_owned();
        if shell.aliases.remove(&name).is_none() {
            io.say(format!("oh-my-shell: unalias: {name}: not found"));
            status = ExitStatus(1);
        }
    }
    Ok(status)
}
