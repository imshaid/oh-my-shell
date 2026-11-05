//! Running a parsed list: lists, `&&` and `||`, pipelines, simple commands and functions.
//! The compound commands are in [`compound`], pipelines in [`pipeline`].
//!
//! # How a command ends
//!
//! Every function here returns a [`Flow`]: the exit status, or an [`Unwind`]. `exit`,
//! `return`, `break`, `continue` and a failed expansion are not statuses. They stop what is
//! running and travel up to whoever handles them: a loop takes `break` and `continue`, a
//! function takes `return`, and the top of the line takes `exit`. A subshell, a pipeline stage
//! and a `$(...)` are boundaries: nothing gets out of them but a status (see [`boundary`]).

mod compound;
mod pipeline;

use std::ffi::{CString, OsStr};
use std::os::unix::ffi::OsStrExt;

use omsh_syntax::ast::{FunctionDef, Redirect, SimpleCommand, Word};

use crate::builtins::{self, Class};
use crate::expand::{ExpandError, Expander};
use crate::io::Io;
use crate::path::Lookup;
use crate::redirect::RedirectError;
use crate::shell::Shell;
use crate::signals;
use crate::spawn::{ExecRequest, SpawnFailure, run as spawn};
use crate::status::{ExitStatus, Termination};
use omsh_syntax::ast::{AndOr, AndOrOp, Command, CommandKind, List, Pipeline};

use nix::errno::Errno;
use nix::libc;
use std::sync::Arc;

/// How many lists (and function calls) may be run inside each other. A function that calls
/// itself forever reaches this and gets an error. Without a limit it would overflow the stack,
/// which kills the shell.
///
/// Measured with the 8 MiB stack of a main thread: one level uses 2 to 8 KiB in a debug build
/// (the program crashes between 2000 and 4000 levels for most shapes of recursion, and
/// between 1000 and 2000 for recursion through `$(...)`) and 0.5 to 2 KiB in a release build.
/// The limit leaves at least four times as much room as it needs in the worst case. A
/// function that calls itself uses about three levels per call.
pub(crate) const MAX_DEPTH: u32 = 500;

/// Something that stops the commands being run and travels up to the code that handles it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unwind {
    /// `exit`: the shell ends with this status.
    Exit(ExitStatus),
    /// `return`: the function or sourced file ends with this status.
    Return(ExitStatus),
    /// `break n`: this many more loops to leave.
    Break(u32),
    /// `continue n`: this many more loops to leave, the last one by going to its next pass.
    Continue(u32),
    /// A problem that ends the line, such as `${x?message}` or an unknown redirection. The
    /// message was printed.
    Error(ExitStatus),
    /// Ctrl+C.
    Interrupted,
}

/// The result of running something.
pub(crate) type Flow = Result<ExitStatus, Unwind>;

/// What comes out of a subshell, a pipeline stage or a `$(...)`: a status.
pub(crate) fn boundary(flow: Flow) -> ExitStatus {
    match flow {
        Ok(status) | Err(Unwind::Exit(status) | Unwind::Return(status) | Unwind::Error(status)) => {
            status
        }
        Err(Unwind::Break(_) | Unwind::Continue(_)) => ExitStatus::SUCCESS,
        Err(Unwind::Interrupted) => ExitStatus::from_signal(2),
    }
}

/// What a command name stands for.
enum Target {
    Builtin(Class),
    Function(Arc<FunctionDef>),
    External,
}

impl Shell {
    /// Runs a list: one item after another.
    pub(crate) fn run_list(&mut self, list: &List, io: &Io) -> Flow {
        self.enter(io)?;
        let result = self.run_items(list, io);
        self.depth -= 1;
        result
    }

    /// Counts one more level of nesting, or says it is too deep.
    fn enter(&mut self, io: &Io) -> Result<(), Unwind> {
        if self.depth >= MAX_DEPTH {
            io.say("oh-my-shell: too deeply nested");
            return Err(Unwind::Error(ExitStatus::USAGE));
        }
        self.depth += 1;
        Ok(())
    }

    fn run_items(&mut self, list: &List, io: &Io) -> Flow {
        let mut status = self.status;
        for item in &list.items {
            if signals::interrupted() {
                return Err(Unwind::Interrupted);
            }
            status = self.run_and_or(&item.and_or, io)?;
        }
        Ok(status)
    }

    fn run_and_or(&mut self, and_or: &AndOr, io: &Io) -> Flow {
        // Under `-e`, a failure on the left of `&&` or `||` is expected and ends nothing.
        let mut status = self.ignoring_errexit(!and_or.rest.is_empty(), |shell| {
            shell.run_pipeline(&and_or.first, io)
        })?;
        for (index, part) in and_or.rest.iter().enumerate() {
            let go = match part.op {
                AndOrOp::And => status == ExitStatus::SUCCESS,
                AndOrOp::Or => status != ExitStatus::SUCCESS,
            };
            if go {
                let is_last = index + 1 == and_or.rest.len();
                status = self
                    .ignoring_errexit(!is_last, |shell| shell.run_pipeline(&part.pipeline, io))?;
            }
        }
        Ok(status)
    }

    /// Runs `body`. If `ignore` is set, a command that fails inside does not end the shell
    /// under `-e` (see `errexit_off`).
    pub(crate) fn ignoring_errexit<T>(
        &mut self,
        ignore: bool,
        body: impl FnOnce(&mut Self) -> Result<T, Unwind>,
    ) -> Result<T, Unwind> {
        if !ignore {
            return body(self);
        }
        self.errexit_off += 1;
        let result = body(self);
        self.errexit_off -= 1;
        result
    }

    /// Ends the shell if `-e` is on and a command failed where that counts.
    pub(crate) fn errexit_check(&self, status: ExitStatus) -> Result<(), Unwind> {
        if status != ExitStatus::SUCCESS && self.options.errexit && self.errexit_off == 0 {
            return Err(Unwind::Exit(status));
        }
        Ok(())
    }

    fn run_pipeline(&mut self, pipeline: &Pipeline, io: &Io) -> Flow {
        // After `!`, the failure is the point, so `-e` does not apply inside.
        let status = self.ignoring_errexit(pipeline.negated, |shell| {
            match pipeline.commands.as_slice() {
                [command] => shell.run_command(command, io),
                commands => {
                    let status = shell.run_stages(commands, io)?;
                    shell.errexit_check(status)?;
                    Ok(status)
                }
            }
        })?;
        if !pipeline.negated {
            return Ok(status);
        }
        let negated = if status == ExitStatus::SUCCESS {
            ExitStatus(1)
        } else {
            ExitStatus::SUCCESS
        };
        self.status = negated;
        Ok(negated)
    }

    /// Runs one command with its redirections, and records its status in `$?`.
    pub(crate) fn run_command(&mut self, command: &Command, io: &Io) -> Flow {
        let checked = matches!(
            command.kind,
            CommandKind::Simple(_) | CommandKind::Subshell(_)
        );
        let status = if let CommandKind::Simple(simple) = &command.kind {
            self.run_simple(simple, &command.redirects, io)?
        } else {
            let redirected;
            let inner = if command.redirects.is_empty() {
                io
            } else {
                match self.apply_redirects(&command.redirects, io) {
                    Ok(table) => {
                        redirected = table;
                        &redirected
                    }
                    Err(failure) => return self.redirect_failed(failure, io),
                }
            };
            self.run_compound(&command.kind, inner)?
        };
        self.status = status;
        if checked {
            self.errexit_check(status)?;
        }
        Ok(status)
    }

    /// Reports a redirection that could not be made. The command does not run and its
    /// status is 2, as in dash.
    fn redirect_failed(&mut self, failure: RedirectError, io: &Io) -> Flow {
        match failure {
            RedirectError::Failed(message) => {
                io.say(format!("oh-my-shell: {message}"));
                self.status = ExitStatus::USAGE;
                Ok(ExitStatus::USAGE)
            }
            RedirectError::Expand(error) => Err(self.expand_failed(&error, io)),
        }
    }

    /// Prints an expansion error. It ends the line with status 2.
    pub(crate) fn expand_failed(&mut self, error: &ExpandError, io: &Io) -> Unwind {
        io.say(format!("oh-my-shell: {}", error.0));
        Unwind::Error(ExitStatus::USAGE)
    }

    /// Expands the words of a command (every step, one word can give any number of results).
    pub(crate) fn expand_words(&mut self, words: &[Word], io: &Io) -> Result<Vec<Vec<u8>>, Unwind> {
        let result = Expander { shell: self, io }.expand_words(words);
        result.map_err(|e| self.expand_failed(&e, io))
    }

    fn classify(&self, name: &[u8]) -> Target {
        if let Some(class @ Class::Special) = builtins::classify(name) {
            return Target::Builtin(class);
        }
        if let Some(function) = std::str::from_utf8(name)
            .ok()
            .and_then(|n| self.functions.get(n))
        {
            return Target::Function(Arc::clone(function));
        }
        builtins::classify(name).map_or(Target::External, Target::Builtin)
    }

    /// Runs a simple command: expands its words, makes its redirections, applies its
    /// assignments, then runs the builtin, function or program.
    fn run_simple(&mut self, simple: &SimpleCommand, redirects: &[Redirect], io: &Io) -> Flow {
        self.cmdsub_status = None;
        let words = self.expand_words(&simple.words, io)?;
        // Ctrl+C during a `$(...)` leaves the words incomplete: do not run the command.
        if signals::interrupted() {
            return Err(Unwind::Interrupted);
        }
        let target = words.first().map(|name| self.classify(name));
        let special = matches!(target, Some(Target::Builtin(Class::Special)));

        let redirected;
        let table = if redirects.is_empty() {
            io
        } else {
            match self.apply_redirects(redirects, io) {
                Ok(table) => {
                    redirected = table;
                    &redirected
                }
                Err(RedirectError::Failed(message)) if special => {
                    // A mistake in a redirection of a special builtin ends the line.
                    io.say(format!("oh-my-shell: {message}"));
                    return Err(Unwind::Error(ExitStatus::USAGE));
                }
                Err(failure) => return self.redirect_failed(failure, io),
            }
        };

        let Some(target) = target else {
            // Only assignments: they stay. The status is that of the last `$(...)`, if any.
            let result = Expander { shell: self, io }.apply_assignments(&simple.assignments);
            let applied = result.map_err(|e| self.expand_failed(&e, io))?;
            self.trace(&applied, &words, io);
            return Ok(self.cmdsub_status.unwrap_or(ExitStatus::SUCCESS));
        };

        // Assignments in front of a command last for that command only, except for special
        // builtins. They are visible to the command (and to the programs it starts), so they
        // are exported while it runs. Only the variables named here are saved and put back:
        // anything else the command changes stays changed.
        let saved: Vec<_> = if special {
            Vec::new()
        } else {
            simple
                .assignments
                .iter()
                .map(|a| (a.name.as_str(), self.vars.snapshot(&a.name)))
                .collect()
        };
        let applied = Expander { shell: self, io }.apply_assignments(&simple.assignments);
        let flow = match applied {
            Ok(applied) => {
                self.trace(&applied, &words, io);
                if !special {
                    for assignment in &simple.assignments {
                        self.vars.export(&assignment.name);
                    }
                }
                self.dispatch(&target, &words, table)
            }
            Err(error) => Err(self.expand_failed(&error, io)),
        };
        for (name, old) in saved.into_iter().rev() {
            self.vars.restore(name, old);
        }
        flow
    }

    /// Under `-x`, writes the command about to run to standard error, after `$PS4`.
    fn trace(&self, assignments: &[(String, Vec<u8>)], words: &[Vec<u8>], io: &Io) {
        if !self.options.xtrace {
            return;
        }
        let mut line = self
            .vars
            .get("PS4")
            .map_or_else(|| b"+ ".to_vec(), <[u8]>::to_vec);
        let mut first = true;
        for (name, value) in assignments {
            if !std::mem::take(&mut first) {
                line.push(b' ');
            }
            line.extend_from_slice(name.as_bytes());
            line.push(b'=');
            line.extend_from_slice(value);
        }
        for word in words {
            if !std::mem::take(&mut first) {
                line.push(b' ');
            }
            line.extend_from_slice(word);
        }
        line.push(b'\n');
        let _ = io.write_all(2, &line);
    }

    fn dispatch(&mut self, target: &Target, words: &[Vec<u8>], io: &Io) -> Flow {
        match target {
            Target::Builtin(_) => builtins::run(self, &words[0], &words[1..], io),
            Target::Function(function) => self.call_function(function, &words[1..], io),
            Target::External => Ok(self.run_external(words, io)),
        }
    }

    /// Calls a function with the given arguments as `$1`, `$2`, ...
    fn call_function(&mut self, function: &FunctionDef, args: &[Vec<u8>], io: &Io) -> Flow {
        self.enter(io)?;
        let saved = std::mem::replace(&mut self.positional, args.to_vec());
        self.func_depth += 1;
        let result = self.run_command(&function.body, io);
        self.func_depth -= 1;
        self.positional = saved;
        self.depth -= 1;
        match result {
            Err(Unwind::Return(status)) => Ok(status),
            other => other,
        }
    }

    /// Starts a program and waits for it. Returns its status. Problems are reported to
    /// descriptor 2 of `io`.
    fn run_external(&mut self, words: &[Vec<u8>], io: &Io) -> ExitStatus {
        let name = String::from_utf8_lossy(&words[0]).into_owned();
        let search_path = self.vars.search_path(&[]);
        let file = match self
            .paths
            .lookup(&words[0], &search_path, self.cwd_override.as_deref())
        {
            Lookup::Found(file) => file,
            Lookup::Denied(_) => {
                io.say(format!("oh-my-shell: {name}: permission denied"));
                return ExitStatus::NOT_EXECUTABLE;
            }
            Lookup::NotFound => {
                io.say(format!("oh-my-shell: {name}: not found"));
                return ExitStatus::NOT_FOUND;
            }
        };
        let Some(argv) = to_cstrings(words) else {
            io.say(format!("oh-my-shell: {name}: an argument holds a NUL byte"));
            return ExitStatus::NOT_EXECUTABLE;
        };
        let Ok(path) = CString::new(file.as_os_str().as_bytes()) else {
            io.say(format!("oh-my-shell: {name}: the path holds a NUL byte"));
            return ExitStatus::NOT_EXECUTABLE;
        };
        let cwd = self
            .cwd_override
            .as_ref()
            .and_then(|dir| CString::new(OsStr::as_bytes(dir.as_os_str())).ok());
        let envp = self.vars.envp(&[]);

        let request = ExecRequest {
            path: &path,
            argv: &argv,
            envp: &envp,
            cwd: cwd.as_deref(),
        };
        let mut plan = io.plan();
        match spawn(&request, &mut plan) {
            Ok(ending) => {
                // A program that survived Ctrl+C dealt with it itself. One that died of it
                // leaves the flag set, which stops the rest of the line, as in bash.
                let died_of_it = matches!(
                    ending,
                    Termination::Signaled { signal, .. } if signal == libc::SIGINT
                );
                if signals::interrupted() && !died_of_it {
                    signals::clear_interrupt();
                }
                if let Some(line) = ending.describe(&name) {
                    io.say(line);
                }
                ending.status()
            }
            Err(SpawnFailure::Exec(errno)) => report_exec_failure(&name, errno, io),
            Err(SpawnFailure::Fork(errno)) => {
                io.say(format!(
                    "oh-my-shell: cannot start {name}: {}",
                    errno.desc()
                ));
                ExitStatus::NOT_EXECUTABLE
            }
        }
    }
}

/// Says why `execve` failed, and picks the status: 127 if there was nothing to run, 126 if
/// there was a file that could not be run.
fn report_exec_failure(name: &str, errno: Errno, io: &Io) -> ExitStatus {
    match errno {
        Errno::ENOENT | Errno::ENOTDIR => {
            io.say(format!("oh-my-shell: {name}: not found"));
            ExitStatus::NOT_FOUND
        }
        Errno::EACCES => {
            io.say(format!("oh-my-shell: {name}: permission denied"));
            ExitStatus::NOT_EXECUTABLE
        }
        other => {
            io.say(format!("oh-my-shell: {name}: {}", other.desc()));
            ExitStatus::NOT_EXECUTABLE
        }
    }
}

fn to_cstrings(words: &[Vec<u8>]) -> Option<Vec<CString>> {
    words
        .iter()
        .map(|w| CString::new(w.as_slice()).ok())
        .collect()
}

#[cfg(test)]
mod tests;
