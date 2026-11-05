//! `trap [action signal...]`.

use super::quote;
use crate::exec::{Flow, Unwind};
use crate::io::Io;
use crate::shell::Shell;
use crate::signals::{Disposition, set_disposition};
use crate::status::ExitStatus;
use crate::traps::{Action, SIGNALS, signal_name};

/// Why a signal could not be used.
enum Bad {
    /// Not a signal at all, or one that can never be trapped (`KILL`, `STOP`).
    Unknown,
    /// A real signal that this shell cannot trap yet.
    NotYet,
}

/// A signal given by name (`INT`, `SIGINT`) or number.
fn parse_signal(text: &[u8]) -> Result<i32, Bad> {
    let text = std::str::from_utf8(text).map_err(|_| Bad::Unknown)?;
    if text.bytes().all(|b| b.is_ascii_digit()) && !text.is_empty() {
        let number: i32 = text.parse().map_err(|_| Bad::Unknown)?;
        return match SIGNALS.iter().find(|(_, n)| *n == number) {
            Some(_) => Ok(number),
            None if (1..=64).contains(&number) && !matches!(number, 9 | 19) => Err(Bad::NotYet),
            None => Err(Bad::Unknown),
        };
    }
    let name = text.strip_prefix("SIG").unwrap_or(text);
    if let Some((_, number)) = SIGNALS.iter().find(|(n, _)| *n == name) {
        return Ok(*number);
    }
    match name {
        "CHLD" | "CONT" | "TSTP" | "TTIN" | "TTOU" | "PIPE" | "WINCH" | "URG" | "ABRT" | "FPE"
        | "ILL" | "SEGV" | "BUS" | "TRAP" | "SYS" | "VTALRM" | "PROF" | "XCPU" | "XFSZ" => {
            Err(Bad::NotYet)
        }
        _ => Err(Bad::Unknown),
    }
}

/// `trap`: with no arguments lists the traps. `trap - SIG...` (or `trap SIG...` with a number
/// first) sets the default back, `trap '' SIG...` ignores the signals, and
/// `trap 'command' SIG...` runs the command when one arrives.
pub(super) fn trap(shell: &mut Shell, args: &[Vec<u8>], io: &Io) -> Flow {
    let args = match args.first().map(Vec::as_slice) {
        Some(b"--") => &args[1..],
        _ => args,
    };
    let args = match args.first().map(Vec::as_slice) {
        Some(b"-p") => &args[1..],
        _ => args,
    };
    let Some((first, signals)) = args.split_first() else {
        return list(shell, io);
    };
    // A first argument that is a number is a signal: `trap 2` puts the default back. So is
    // a lone argument, because an action with no signal means nothing (dash does the same).
    let numeric = !first.is_empty() && first.iter().all(u8::is_ascii_digit);
    let (action, signals) = if numeric || (args.len() == 1 && first != b"-") {
        (None, args)
    } else if first == b"-" {
        (None, signals)
    } else if first.is_empty() {
        (Some(Action::Ignore), signals)
    } else {
        (
            Some(Action::Command(String::from_utf8_lossy(first).into_owned())),
            signals,
        )
    };
    if signals.is_empty() {
        io.say("oh-my-shell: trap: usage: trap [action] signal...");
        return Ok(ExitStatus::USAGE);
    }
    let mut status = ExitStatus::SUCCESS;
    for text in signals {
        let shown = String::from_utf8_lossy(text);
        match parse_signal(text) {
            Ok(signal) => set(shell, signal, action.clone()),
            Err(Bad::Unknown) => {
                io.say(format!("oh-my-shell: trap: {shown}: bad trap"));
                status = ExitStatus(1);
            }
            Err(Bad::NotYet) => {
                io.say(format!(
                    "oh-my-shell: trap: {shown}: this signal cannot be trapped yet"
                ));
                status = ExitStatus(1);
            }
        }
    }
    Ok(status)
}

fn set(shell: &mut Shell, signal: i32, action: Option<Action>) {
    // The disposition of a signal belongs to the whole process. A copy of the shell (a
    // subshell, a thread) only notes the trap for itself.
    if signal > 0 && !shell.in_subshell {
        set_disposition(
            signal,
            match &action {
                None => Disposition::Default,
                Some(Action::Ignore) => Disposition::Ignore,
                Some(Action::Command(_)) => Disposition::Catch,
            },
        );
    }
    shell.traps.set(signal, action);
}

/// The traps in a form that can be typed back.
fn list(shell: &Shell, io: &Io) -> Flow {
    let mut out = Vec::new();
    for (signal, action) in shell.traps.iter() {
        out.extend_from_slice(b"trap -- ");
        match action {
            Action::Ignore => out.extend_from_slice(b"''"),
            Action::Command(text) => out.extend_from_slice(&quote(text.as_bytes())),
        }
        out.push(b' ');
        out.extend_from_slice(signal_name(signal).as_bytes());
        out.push(b'\n');
    }
    shell.emit(io, 1, &out).map_err(|e: Unwind| e)
}
