//! Pipelines: `a | b | c`.
//!
//! Each stage is a subshell: it runs on a thread of its own, on a copy of the shell, with the
//! ends of the pipes in its table of files ([`Io`]). A stage that names a program starts it
//! with `fork` from its thread; a builtin, a function or a compound command runs right on the
//! thread. §4.3 forbids the usual way (fork first, then run shell code in the child) because
//! the child of a multi-threaded process may only make a few system calls.
//!
//! A pipe has a reading end and a writing end. The reader sees the end of its input only when
//! every holder of the writing end has let go. Here, the holders are the tables of files, and
//! each is dropped as soon as its stage is over, which is what ends the next stage's input.

use std::os::fd::OwnedFd;
use std::sync::Arc;
use std::thread;

use nix::fcntl::OFlag;
use nix::unistd::pipe2;
use omsh_syntax::ast::Command;

use super::{Flow, Unwind};
use crate::io::Io;
use crate::shell::Shell;
use crate::signals;
use crate::status::ExitStatus;

/// The stack of a stage's thread. The main thread has 8 MiB; the stages get the same, so that
/// the depth limit is safe on both. Memory that is not used is never mapped.
const STAGE_STACK: usize = 8 * 1024 * 1024;

impl Shell {
    /// Runs the commands of a pipeline at the same time and waits for all of them. The
    /// status is that of the last one.
    pub(super) fn run_stages(&mut self, commands: &[Command], io: &Io) -> Flow {
        let mut tables: Vec<Io> = Vec::with_capacity(commands.len());
        let mut previous: Option<OwnedFd> = None;
        for index in 0..commands.len() {
            let mut table = io.clone();
            if let Some(reader) = previous.take() {
                table.set(0, Arc::new(reader));
            }
            if index + 1 < commands.len() {
                let Ok((reader, writer)) = pipe2(OFlag::O_CLOEXEC) else {
                    io.say("oh-my-shell: cannot make a pipe");
                    return Ok(ExitStatus(1));
                };
                table.set(1, Arc::new(writer));
                previous = Some(reader);
            }
            tables.push(table);
        }

        let statuses: Vec<ExitStatus> = thread::scope(|scope| {
            let handles: Vec<_> = commands
                .iter()
                .zip(tables)
                .map(|(command, table)| {
                    let mut stage = self.subshell();
                    thread::Builder::new()
                        .stack_size(STAGE_STACK)
                        .spawn_scoped(scope, move || {
                            let flow = stage.run_command(command, &table);
                            let status = stage.finish_copy(flow, &table);
                            // The table goes away here, which closes this stage's ends of
                            // the pipes.
                            drop(table);
                            status
                        })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| match handle {
                    Ok(handle) => handle.join().unwrap_or(ExitStatus(134)),
                    Err(error) => {
                        io.say(format!(
                            "oh-my-shell: cannot start a pipeline stage: {error}"
                        ));
                        ExitStatus(1)
                    }
                })
                .collect()
        });
        if signals::interrupted() {
            return Err(Unwind::Interrupted);
        }
        let mut status = statuses.last().copied().unwrap_or(ExitStatus::SUCCESS);
        if self.options.pipefail {
            // The last command that failed, if any.
            status = statuses
                .iter()
                .rev()
                .copied()
                .find(|s| *s != ExitStatus::SUCCESS)
                .unwrap_or(ExitStatus::SUCCESS);
        }
        self.status = status;
        Ok(status)
    }
}
