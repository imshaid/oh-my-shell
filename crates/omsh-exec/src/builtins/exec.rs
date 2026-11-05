//! `exec [command [argument...]]`.
//!
//! With a command, the shell is replaced by it (see `Shell::replace_with`). With none,
//! `exec` is only there for its redirections: `exec 3>file` or `exec 2>&1` change the files of
//! every command that comes after. The executor records those changes when it makes the
//! redirections of a command that is `exec` with no words (see `run_simple`), so there is
//! nothing to do here.

use crate::exec::Flow;
use crate::io::Io;
use crate::shell::Shell;
use crate::status::ExitStatus;

/// `exec`.
pub(super) fn exec(shell: &mut Shell, args: &[Vec<u8>], io: &Io) -> Flow {
    let args = match args.first().map(Vec::as_slice) {
        Some(b"--") => &args[1..],
        _ => args,
    };
    if args.is_empty() {
        return Ok(ExitStatus::SUCCESS);
    }
    shell.replace_with(args, io)
}
