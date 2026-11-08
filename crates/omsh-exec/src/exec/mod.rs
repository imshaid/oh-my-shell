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

mod background;
mod compound;
mod pipeline;
mod stopped;

use std::ffi::{CString, OsStr};
use std::os::unix::ffi::OsStrExt;

use omsh_syntax::ast::{FunctionDef, Redirect, SimpleCommand, Word};

use crate::builtins::{self, Class};
use crate::expand::{ExpandError, Expander};
use crate::io::Io;
use crate::jobs::Job;
use crate::path::{DEFAULT_PATH, Lookup};
use crate::redirect::RedirectError;
use crate::shell::Shell;
use crate::signals;
use crate::spawn::{ExecRequest, SpawnFailure, Waited, replace_process, run as spawn};
use crate::status::{ExitStatus, Termination};
use crate::terminal;
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
    /// Ctrl+Z stopped a program while loops were running. The loops are left, one after the
    /// other, and the rest of the line goes on with this status (148 for Ctrl+Z), as in bash.
    Stopped(ExitStatus),
}

/// The result of running something.
pub(crate) type Flow = Result<ExitStatus, Unwind>;

/// What comes out of a subshell, a pipeline stage or a `$(...)`: a status.
pub(crate) fn boundary(flow: Flow) -> ExitStatus {
    match flow {
        Ok(status)
        | Err(
            Unwind::Exit(status)
            | Unwind::Return(status)
            | Unwind::Error(status)
            | Unwind::Stopped(status),
        ) => status,
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
        // The files for the next command: `io`, changed by any `exec` so far.
        let mut local: Option<Io> = None;
        for item in &list.items {
            if self.interrupted() {
                return Err(Unwind::Interrupted);
            }
            if signals::hung_up() && !self.in_subshell {
                return Err(Unwind::Exit(ExitStatus(129)));
            }
            self.run_pending_traps(local.as_ref().unwrap_or(io))?;
            status = if item.background {
                self.run_background(item, local.as_ref().unwrap_or(io))?
            } else {
                self.run_and_or(&item.and_or, local.as_ref().unwrap_or(io))?
            };
            self.follow_fd_changes(io, &mut local);
            self.run_pending_traps(local.as_ref().unwrap_or(io))?;
        }
        Ok(status)
    }

    /// Takes over what `exec` changed in the files of the commands that follow. The changes
    /// stay recorded, so that the list that contains this one takes them over too.
    fn follow_fd_changes(&self, base: &Io, local: &mut Option<Io>) {
        if self.fd_changes.is_empty() {
            return;
        }
        let mut table = local.take().unwrap_or_else(|| base.clone());
        table.apply(&self.fd_changes);
        *local = Some(table);
    }

    /// Records the changes `exec` made to a table, replacing earlier ones for the same
    /// descriptors.
    pub(crate) fn record_fd_changes(&mut self, changes: Vec<(u32, Option<crate::io::Slot>)>) {
        for (fd, slot) in changes {
            self.fd_changes.retain(|(old, _)| *old != fd);
            self.fd_changes.push((fd, slot));
        }
    }

    fn run_and_or(&mut self, and_or: &AndOr, io: &Io) -> Flow {
        // Under `-e`, a failure on the left of `&&` or `||` is expected and ends nothing.
        let mut status = self.ignoring_errexit(!and_or.rest.is_empty(), |shell| {
            shell.run_pipeline(&and_or.first, io)
        })?;
        let mut local: Option<Io> = None;
        for (index, part) in and_or.rest.iter().enumerate() {
            self.follow_fd_changes(io, &mut local);
            let current = local.as_ref().unwrap_or(io);
            let go = match part.op {
                AndOrOp::And => status == ExitStatus::SUCCESS,
                AndOrOp::Or => status != ExitStatus::SUCCESS,
            };
            if go {
                let is_last = index + 1 == and_or.rest.len();
                status = self.ignoring_errexit(!is_last, |shell| {
                    shell.run_pipeline(&part.pipeline, current)
                })?;
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
                    let status = shell.run_stages(commands, io, pipeline.span)?;
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
        self.current_span = Some(command.span);
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

    fn classify(&self, name: &[u8], skip_functions: bool) -> Target {
        if let Some(class @ Class::Special) = builtins::classify(name) {
            return Target::Builtin(class);
        }
        if skip_functions {
            return builtins::classify(name).map_or(Target::External, Target::Builtin);
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
        if self.interrupted() {
            return Err(Unwind::Interrupted);
        }
        let (words, via_command) = self.strip_command(words);
        let target = words
            .first()
            .map(|name| self.classify(name, via_command.is_some()));
        let special =
            via_command.is_none() && matches!(target, Some(Target::Builtin(Class::Special)));

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

        // `exec` with redirections and no command changes the files of what follows.
        if matches!(words.as_slice(), [name] | [name, _] if name == b"exec")
            && words.get(1).is_none_or(|w| w == b"--")
        {
            self.record_fd_changes(table.diff(io));
        }

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
                // A special builtin keeps its assignments but does not pass them on, except
                // `exec`, which becomes the program that should receive them.
                if !special || words[0] == b"exec" {
                    for assignment in &simple.assignments {
                        self.vars.export(&assignment.name);
                    }
                }
                // `command -p` searches the default path for this one command.
                let saved_path = via_command.and_then(|p| {
                    p.then(|| self.path_override.replace(DEFAULT_PATH.as_bytes().to_vec()))
                });
                let flow = self.dispatch(&target, &words, table);
                if let Some(old) = saved_path {
                    self.path_override = old;
                }
                flow
            }
            Err(error) => Err(self.expand_failed(&error, io)),
        };
        for (name, old) in saved.into_iter().rev() {
            self.vars.restore(name, old);
        }
        flow
    }

    /// `command name args` runs `name` while skipping functions, and a special builtin run
    /// this way loses its special properties. Returns the words without `command` and its
    /// options, and `Some(use_default_path)` if they were removed. The forms that only look
    /// something up (`command -v name`) are left for the builtin.
    fn strip_command(&self, words: Vec<Vec<u8>>) -> (Vec<Vec<u8>>, Option<bool>) {
        if words.first().map(Vec::as_slice) != Some(b"command")
            || self.functions.contains_key("command")
        {
            return (words, None);
        }
        let mut at = 1;
        let mut default_path = false;
        while let Some(word) = words.get(at) {
            match word.as_slice() {
                b"-p" => default_path = true,
                b"--" => {
                    at += 1;
                    break;
                }
                _ => break,
            }
            at += 1;
        }
        match words.get(at) {
            // No command follows, or the next word is an option such as `-v`.
            None => (words, None),
            Some(word)
                if word.starts_with(b"-")
                    && word.len() > 1
                    && at == 1 + usize::from(default_path) =>
            {
                (words, None)
            }
            Some(_) => (words[at..].to_vec(), Some(default_path)),
        }
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
            Target::External => self.run_external(words, io),
        }
    }

    /// Calls a function with the given arguments as `$1`, `$2`, ...
    fn call_function(&mut self, function: &FunctionDef, args: &[Vec<u8>], io: &Io) -> Flow {
        self.enter(io)?;
        let saved = std::mem::replace(&mut self.positional, args.to_vec());
        // The spans in the body index the line that defined the function, not this one.
        let saved_source = self.source.take();
        self.func_depth += 1;
        let result = self.run_command(&function.body, io);
        self.func_depth -= 1;
        self.source = saved_source;
        self.positional = saved;
        self.depth -= 1;
        match result {
            Err(Unwind::Return(status)) => Ok(status),
            other => other,
        }
    }

    /// Finds the program for `words` and builds everything `execve` needs. A problem is
    /// reported to descriptor 2 of `io` and gives the status of the command.
    fn prepare_external(&mut self, words: &[Vec<u8>], io: &Io) -> Result<Program, ExitStatus> {
        let name = String::from_utf8_lossy(&words[0]).into_owned();
        let search_path = self
            .path_override
            .clone()
            .unwrap_or_else(|| self.vars.search_path(&[]));
        let file = match self
            .paths
            .lookup(&words[0], &search_path, self.cwd_override.as_deref())
        {
            Lookup::Found(file) => file,
            Lookup::Denied(_) => {
                io.say(format!("oh-my-shell: {name}: permission denied"));
                return Err(ExitStatus::NOT_EXECUTABLE);
            }
            Lookup::NotFound => {
                io.say(format!("oh-my-shell: {name}: not found"));
                return Err(ExitStatus::NOT_FOUND);
            }
        };
        let Some(argv) = to_cstrings(words) else {
            io.say(format!("oh-my-shell: {name}: an argument holds a NUL byte"));
            return Err(ExitStatus::NOT_EXECUTABLE);
        };
        let Ok(path) = CString::new(file.as_os_str().as_bytes()) else {
            io.say(format!("oh-my-shell: {name}: the path holds a NUL byte"));
            return Err(ExitStatus::NOT_EXECUTABLE);
        };
        let cwd = self
            .cwd_override
            .as_ref()
            .and_then(|dir| CString::new(OsStr::as_bytes(dir.as_os_str())).ok());
        Ok(Program {
            name,
            path,
            argv,
            envp: self.vars.envp(&[]),
            cwd,
            umask: self.umask_override,
            ignored: self.traps.ignored_mask()
                | signals::inherited_ignored()
                | self.job_ignored_signals(),
        })
    }

    /// Starts a program and waits for it. Returns its status. Problems are reported to
    /// descriptor 2 of `io`.
    fn run_external(&mut self, words: &[Vec<u8>], io: &Io) -> Flow {
        match self.prepare_external(words, io) {
            Ok(program) => self.run_program(&program, io),
            Err(status) => Ok(status),
        }
    }

    /// `exec command args`: the shell becomes the program. In a copy of the shell (a
    /// subshell, a pipeline stage) that cannot be done, because the copy is a thread and
    /// `execve` would replace the whole process: the program runs, and the copy ends with
    /// its status. If the program cannot be started, the shell goes on.
    pub(crate) fn replace_with(&mut self, words: &[Vec<u8>], io: &Io) -> Flow {
        let program = match self.prepare_external(words, io) {
            Ok(program) => program,
            Err(status) => return Ok(status),
        };
        if self.in_subshell {
            return Err(Unwind::Exit(boundary(self.run_program(&program, io))));
        }
        let mut plan = io.plan();
        // The program takes the shell's place, so it must be in the group that owns the
        // terminal when the shell started, or it would be stopped when it reads.
        if self.options.monitor {
            terminal::release_terminal();
        }
        let errno = replace_process(&program.request(), &mut plan);
        if self.options.monitor {
            terminal::reacquire_terminal();
        }
        Ok(report_exec_failure(&program.name, errno, io))
    }

    /// The job a program started now belongs to, and whether this call made it (and so must
    /// finish it). None if the program stays in the shell's own group.
    fn job_for_program(&self) -> Option<(Arc<Job>, bool)> {
        if let Some(job) = &self.job {
            return Some((Arc::clone(job), false));
        }
        (self.options.monitor && !self.no_new_jobs)
            .then(|| (Job::foreground(self.jobs.bell(), 1, false), true))
    }

    /// Starts the program, waits for it, and reports how it ended. If the program is a job of
    /// its own and Ctrl+Z stops it, the job goes to the table and the shell goes on with
    /// status 128 + the signal (see `stopped.rs`).
    fn run_program(&self, program: &Program, io: &Io) -> Flow {
        let name = &program.name;
        let mut plan = io.plan();
        let job = self.job_for_program();
        let result = match &job {
            Some((job, true)) => match job.start(&program.request(), &mut plan) {
                Ok(member) => {
                    let task = job.task(0);
                    match job.wait_or_stop(&member) {
                        Waited::Ended(ending) => {
                            task.done(ending.status());
                            Ok(ending)
                        }
                        Waited::Stopped(_) => {
                            return self.hand_over_and_stop(job, member, task, program, io);
                        }
                    }
                }
                Err(failure) => Err(failure),
            },
            Some((job, false)) => job
                .start(&program.request(), &mut plan)
                .map(|member| job.wait_until_end(&member)),
            None => spawn(&program.request(), &mut plan),
        };
        if let Some((job, true)) = &job {
            job.finish();
        }
        match result {
            Ok(ending) => {
                // Without job control the shell got the Ctrl+C too. A program that survived
                // it dealt with it itself; one that died of it leaves the flag set, which
                // stops the rest of the line, as in bash. With job control the shell never
                // got it, and the flag is set only by a program that died of it.
                let died_of_it = matches!(
                    ending,
                    Termination::Signaled { signal, .. } if signal == libc::SIGINT
                );
                if !self.options.monitor
                    && !self.in_background_job()
                    && signals::interrupted()
                    && !died_of_it
                {
                    signals::clear_interrupt();
                }
                if let Some(line) = ending.describe(name) {
                    io.say(line);
                }
                Ok(ending.status())
            }
            Err(SpawnFailure::Exec(errno)) => Ok(report_exec_failure(name, errno, io)),
            Err(SpawnFailure::Fork(errno)) => {
                io.say(format!(
                    "oh-my-shell: cannot start {name}: {}",
                    errno.desc()
                ));
                Ok(ExitStatus::NOT_EXECUTABLE)
            }
        }
    }
}

/// A program ready to start: the strings that `execve` takes, built before any `fork`.
struct Program {
    name: String,
    path: CString,
    argv: Vec<CString>,
    envp: Vec<CString>,
    cwd: Option<CString>,
    umask: Option<u32>,
    ignored: u64,
}

impl Program {
    fn request(&self) -> ExecRequest<'_> {
        ExecRequest {
            path: &self.path,
            argv: &self.argv,
            envp: &self.envp,
            cwd: self.cwd.as_deref(),
            umask: self.umask,
            ignored: self.ignored,
            group: None,
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
