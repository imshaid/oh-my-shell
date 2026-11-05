//! `exit`, `return`, `break` and `continue`.

use super::parse_count;
use crate::exec::{Flow, Unwind};
use crate::io::Io;
use crate::shell::Shell;
use crate::status::ExitStatus;

/// `exit [n]`: ends the shell (or the subshell it is in) with status `n`, or the status of
/// the last command.
pub(super) fn exit(shell: &Shell, args: &[Vec<u8>], io: &Io) -> Flow {
    let status = match args.first() {
        None => shell.status,
        Some(arg) => match std::str::from_utf8(arg)
            .ok()
            .and_then(|s| s.parse::<i64>().ok())
        {
            // The kernel keeps only the low 8 bits of an exit status.
            Some(n) => ExitStatus((n & 0xff) as u8),
            None => {
                io.say(format!(
                    "oh-my-shell: exit: {}: numeric argument required",
                    String::from_utf8_lossy(arg)
                ));
                ExitStatus::USAGE
            }
        },
    };
    Err(Unwind::Exit(status))
}

/// `return [n]`: ends the function or sourced file. Outside of those it is an error that
/// does not end the shell (dash would end the script; a person typing must not lose the
/// session).
pub(super) fn return_(shell: &Shell, args: &[Vec<u8>], io: &Io) -> Flow {
    if shell.func_depth == 0 {
        io.say("oh-my-shell: return: only works in a function or a sourced file");
        return Ok(ExitStatus::USAGE);
    }
    let status = match args.first() {
        None => shell.status,
        Some(arg) => match parse_count(arg) {
            Some(n) => ExitStatus((n & 0xff) as u8),
            None => {
                io.say(format!(
                    "oh-my-shell: return: {}: numeric argument required",
                    String::from_utf8_lossy(arg)
                ));
                return Ok(ExitStatus::USAGE);
            }
        },
    };
    Err(Unwind::Return(status))
}

/// `break [n]` and `continue [n]`. Outside of a loop they do nothing, as in dash.
pub(super) fn loop_control(
    shell: &Shell,
    args: &[Vec<u8>],
    io: &Io,
    make: fn(u32) -> Unwind,
) -> Flow {
    let count = match args.first() {
        None => 1,
        Some(arg) => match parse_count(arg).filter(|n| *n >= 1) {
            Some(n) => n,
            None => {
                io.say(format!(
                    "oh-my-shell: Illegal number: {}",
                    String::from_utf8_lossy(arg)
                ));
                return Ok(ExitStatus::USAGE);
            }
        },
    };
    if shell.loop_depth == 0 {
        return Ok(ExitStatus::SUCCESS);
    }
    Err(make(count.min(shell.loop_depth)))
}
