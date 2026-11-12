//! The shell: its state, and the entry points that run a line.

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use nix::sys::signal::Signal;
use omsh_syntax::ast::{FunctionDef, List};
use omsh_syntax::{Span, SyntaxDiagnostic, parse};

use crate::builtins::Terminal;
use crate::check::{NotYet, check_list};
use crate::exec::Unwind;
use crate::io::Io;
use crate::jobs::{Job, Phase};
use crate::jobtable::Jobs;
use crate::options::Options;
use crate::path::PathCache;
use crate::signals;
use crate::status::ExitStatus;
use crate::traps::Traps;
use crate::vars::Variables;

/// What the shell does after a line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Read the next line. The status is the status of the last command.
    Continue(ExitStatus),
    /// The shell should end, with this status.
    Exit(ExitStatus),
}

impl Outcome {
    /// The status, whichever way it ended.
    #[must_use]
    pub const fn status(self) -> ExitStatus {
        match self {
            Self::Continue(status) | Self::Exit(status) => status,
        }
    }
}

/// The result of [`Shell::run_source`].
#[derive(Clone, Debug)]
pub enum LineResult {
    /// The line was run.
    Ran(Outcome),
    /// The line is unfinished (an open quote, a trailing `|`): read another line and try
    /// again with both together.
    Incomplete,
    /// The line was not run, because it is wrong or uses something that is not implemented
    /// yet. Nothing in it ran. The status is now 2.
    Rejected(SyntaxDiagnostic),
}

/// A line that has had its aliases replaced and has been parsed, but has not run.
#[derive(Clone, Debug)]
pub struct Prepared {
    list: List,
    text: String,
}

impl Prepared {
    /// The syntax tree.
    #[must_use]
    pub const fn list(&self) -> &List {
        &self.list
    }

    /// The text the tree was parsed from: the line after alias replacement. Spans in the tree
    /// point into it.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// The shell's state: its variables, functions, where commands were found, and the last
/// status.
#[derive(Clone, Debug)]
pub struct Shell {
    pub(crate) paths: PathCache,
    pub(crate) vars: Variables,
    pub(crate) status: ExitStatus,
    /// `$1`, `$2`, ...
    pub(crate) positional: Vec<Vec<u8>>,
    /// `$0`.
    pub(crate) name: Vec<u8>,
    /// True if a person is typing the commands, which `$-` reports as `i`.
    pub(crate) interactive: bool,
    /// The status of the last `$(...)` expanded in the current command. A command made of
    /// assignments only has this status.
    pub(crate) cmdsub_status: Option<ExitStatus>,
    /// Functions by name. The definitions are shared, so a copy of the shell is cheap.
    pub(crate) functions: HashMap<String, Arc<FunctionDef>>,
    /// Aliases by name. They are replaced in the text of a line before it is parsed (see
    /// `alias.rs`).
    pub(crate) aliases: BTreeMap<String, String>,
    /// The traps set with `trap`.
    pub(crate) traps: Traps,
    /// True while the command of a trap runs, so that a trap does not run inside itself.
    pub(crate) running_trap: bool,
    /// Changes to the files of the commands that `exec` (with no command) has made and that
    /// the code running the following commands has not taken over yet. Each list of commands
    /// applies them to its own table; the top of the line applies what is left to the table it
    /// was given, so they last from line to line. A copy of the shell starts with none and
    /// loses them at its end.
    pub(crate) fd_changes: Vec<(u32, Option<crate::io::Slot>)>,
    /// The file creation mask of a copy of the shell that used `umask`, when it is not the
    /// process's. Like the working directory, the mask belongs to the whole process, so a
    /// copy (which runs on a thread next to others) keeps its own and tells its children and
    /// the files it creates itself.
    pub(crate) umask_override: Option<u32>,
    /// The directories to search for a program while `command -p` runs it.
    pub(crate) path_override: Option<Vec<u8>>,
    /// The options set with `set`.
    pub(crate) options: Options,
    /// More than 0 while a failing command must not end the shell under `-e`: in a condition,
    /// on the left of `&&` and `||`, and after `!`. It is a count and not a flag because these
    /// can be nested, and it reaches into functions called from there.
    pub(crate) errexit_off: u32,
    /// Which program files take `--color=auto` (see `color.rs`). Shared by every copy.
    pub(crate) color_probes: crate::color::ProbeCache,
    /// What the `history` builtin runs (see `builtins/history.rs`). Shared by every copy.
    pub(crate) history: crate::builtins::HistoryHook,
    /// What the `j` builtin asks (see `builtins/jump.rs`). Shared by every copy.
    pub(crate) jump: crate::builtins::JumpHook,
    /// What names the "did you mean" line offers (see `suggest.rs`). Shared by every copy.
    pub(crate) suggest: crate::suggest::SuggestHook,
    /// What the `omsh` builtin runs (see `builtins/omsh.rs`). Shared by every copy.
    pub(crate) omsh: crate::builtins::OmshHook,
    /// What `undo` and the trash do (see `builtins/journal.rs`). Shared by every copy.
    pub(crate) journal: crate::builtins::JournalHook,
    /// The working directory of a copy of the shell (a subshell, a pipeline stage, a
    /// `$(...)`), when it is not the process's. `cd` in a copy must not call `chdir`: the
    /// working directory belongs to the whole process, which other threads are using. A copy
    /// keeps its own and tells its children with `chdir` before `execve`.
    pub(crate) cwd_override: Option<PathBuf>,
    /// True in a copy of the shell. A write to a closed pipe then ends it like `SIGPIPE`
    /// ends a process.
    pub(crate) in_subshell: bool,
    /// How many lists are being run inside each other. Bounded, so that endless recursion
    /// ends with an error and not with a stack overflow.
    pub(crate) depth: u32,
    /// How many loops are being run inside each other.
    pub(crate) loop_depth: u32,
    /// How many functions and sourced files are being run inside each other.
    pub(crate) func_depth: u32,
    /// The job whose programs this shell (a pipeline stage, or something inside one) starts.
    /// They all join its process group, so Ctrl+C reaches them together.
    pub(crate) job: Option<Arc<Job>>,
    /// The task of `job` this copy runs for (the index of its pipeline stage). A copy made
    /// inside a stage keeps it, so what runs there settles the stage (see `Job::settle`).
    pub(crate) task: usize,
    /// True in a `$(...)` that is not inside a job. Its programs stay in the shell's own
    /// group and in the foreground with it, as in bash: the substitution is part of the
    /// command that is being expanded.
    pub(crate) no_new_jobs: bool,
    /// The background jobs (`cmd &`). A copy of the shell starts with an empty table.
    pub(crate) jobs: Arc<Jobs>,
    /// `$!`: the process id of the first program of the last background job, if one started.
    pub(crate) last_background: Option<u32>,
    /// The text of the line being run, for the `jobs` listing. Spans in the tree index into
    /// it. It is not set inside a function, whose spans index the line that defined it.
    pub(crate) source: Option<Arc<str>>,
    /// Where the command that is being run is in `source`. A job that stops is named after it.
    pub(crate) current_span: Option<Span>,
    /// `exit` was refused once because of stopped jobs (see [`Shell::confirm_exit`]).
    pub(crate) exit_warned: bool,
    /// The same, as it stood when the current line began: a second `exit` goes through only
    /// if nothing else ran in between.
    pub(crate) exit_carried: bool,
}

impl Default for Shell {
    fn default() -> Self {
        Self::new()
    }
}

impl Shell {
    /// A shell with the environment of this process. `PWD` is set from the working
    /// directory, unless the environment already has a correct one.
    #[must_use]
    pub fn new() -> Self {
        let mut shell = Self::with_variables(Variables::from_process());
        shell.init_pwd();
        shell.init_environment();
        shell
    }

    /// `SHLVL` goes up by one for each shell inside another (a missing or bad value counts as
    /// 0), and `OMSH_VERSION` tells programs which shell they run under. Both are exported.
    /// `SHELL` is left as the system gave it.
    fn init_environment(&mut self) {
        let level = self
            .vars
            .get("SHLVL")
            .and_then(|v| std::str::from_utf8(v).ok())
            .and_then(|v| v.trim().parse::<i64>().ok())
            .filter(|n| *n >= 0)
            .map_or(1, |n| n.saturating_add(1));
        // The variables come from the environment, so none is read only yet.
        let _ = self.vars.set("SHLVL", level.to_string().into_bytes());
        self.vars.export("SHLVL");
        let _ = self.vars.set(
            "OMSH_VERSION",
            env!("CARGO_PKG_VERSION").as_bytes().to_vec(),
        );
        self.vars.export("OMSH_VERSION");
    }

    /// A shell with the given variables.
    #[must_use]
    pub fn with_variables(vars: Variables) -> Self {
        Self {
            paths: PathCache::new(),
            vars,
            status: ExitStatus::SUCCESS,
            positional: Vec::new(),
            name: b"oh-my-shell".to_vec(),
            interactive: false,
            cmdsub_status: None,
            functions: HashMap::new(),
            aliases: BTreeMap::new(),
            traps: Traps::default(),
            running_trap: false,
            fd_changes: Vec::new(),
            umask_override: None,
            path_override: None,
            options: Options::default(),
            errexit_off: 0,
            color_probes: crate::color::ProbeCache::default(),
            history: crate::builtins::HistoryHook::default(),
            jump: crate::builtins::JumpHook::default(),
            suggest: crate::suggest::SuggestHook::default(),
            omsh: crate::builtins::OmshHook::default(),
            journal: crate::builtins::JournalHook::default(),
            cwd_override: None,
            in_subshell: false,
            depth: 0,
            loop_depth: 0,
            func_depth: 0,
            job: None,
            task: 0,
            no_new_jobs: false,
            jobs: Jobs::new(),
            last_background: None,
            source: None,
            current_span: None,
            exit_warned: false,
            exit_carried: false,
        }
    }

    /// Keeps `$PWD` if it names the working directory (it may go through symbolic links, so
    /// it can differ from the real path), and sets it to the real path otherwise.
    fn init_pwd(&mut self) {
        use std::os::unix::fs::MetadataExt;
        let same = |a: &Path, b: &Path| match (std::fs::metadata(a), std::fs::metadata(b)) {
            (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
            _ => false,
        };
        let keep = self.vars.get("PWD").is_some_and(|pwd| {
            let path = Path::new(OsStr::from_bytes(pwd));
            path.is_absolute()
                && path.components().all(|c| {
                    !matches!(
                        c,
                        std::path::Component::CurDir | std::path::Component::ParentDir
                    )
                })
                && same(path, Path::new("."))
        });
        if keep {
            return;
        }
        if let Ok(dir) = std::env::current_dir() {
            // The variables come from the environment, so none is read only yet.
            let _ = self.vars.set("PWD", dir.as_os_str().as_bytes().to_vec());
            self.vars.export("PWD");
        }
    }

    /// The value of a shell variable, if it is set.
    #[must_use]
    pub fn var(&self, name: &str) -> Option<&[u8]> {
        self.vars.get(name)
    }

    /// The names of the variables that are set (a name that is only exported, with no value,
    /// is left out), sorted. Names that are not valid UTF-8 are left out too.
    #[must_use]
    pub fn variable_names(&self) -> Vec<String> {
        self.vars
            .sorted()
            .into_iter()
            .filter(|(_, value, _)| value.is_some())
            .filter_map(|(name, ..)| std::str::from_utf8(name).ok().map(str::to_owned))
            .collect()
    }

    /// The names of the functions defined so far.
    #[must_use]
    pub fn function_names(&self) -> Vec<String> {
        self.functions.keys().cloned().collect()
    }

    /// The names of the aliases defined so far.
    #[must_use]
    pub fn alias_names(&self) -> Vec<String> {
        self.aliases.keys().cloned().collect()
    }

    /// Sets `$0`: the name the shell was started under (`-oh-my-shell` for a login shell).
    pub fn set_name(&mut self, name: &[u8]) {
        self.name = name.to_vec();
    }

    /// Says whether a person is typing the commands. This only changes what `$-` shows.
    pub const fn set_interactive(&mut self, interactive: bool) {
        self.interactive = interactive;
    }

    /// Turns job control on or off. With it on, every command the shell runs is a job: its
    /// programs get a process group of their own, and the terminal while they run. Call it
    /// after a successful [`crate::start_interactive`]. Without a terminal it still makes the
    /// groups, which is what the tests use.
    pub const fn set_job_control(&mut self, on: bool) {
        self.options.monitor = on;
    }

    /// The status of the last command (`$?`).
    #[must_use]
    pub const fn last_status(&self) -> ExitStatus {
        self.status
    }

    /// Records a status for a line that never reached the parser (for example input that is
    /// not valid UTF-8), so that `exit` and the end of the input report it.
    pub const fn set_last_status(&mut self, status: ExitStatus) {
        self.status = status;
    }

    /// A copy of the shell for a subshell, a pipeline stage or a `$(...)`: changes made in
    /// the copy do not reach this shell.
    pub(crate) fn subshell(&self) -> Self {
        let mut copy = self.clone();
        copy.in_subshell = true;
        copy.fd_changes.clear();
        // A subshell has no jobs of its own to list or wait for, as in bash.
        copy.jobs = Jobs::new();
        copy.traps = self.traps.for_subshell();
        if copy.cwd_override.is_none() {
            copy.cwd_override = std::env::current_dir().ok();
        }
        copy
    }

    /// This copy is about to run a command that starts no program. Tell the job, so that it
    /// can count as stopped while a program of another stage is.
    pub(crate) fn settle_task(&self) {
        if let Some(job) = &self.job {
            job.settle(self.task);
        }
    }

    /// True if the commands being run should stop: the user pressed Ctrl+C, or, for the shell
    /// code of a background job, someone sent it a signal with `kill`. A background job must
    /// not read the Ctrl+C flag: Ctrl+C is meant for the foreground job.
    pub(crate) fn interrupted(&self) -> bool {
        match &self.job {
            Some(job) => {
                // A stopped job's shell code waits here until the job runs again.
                job.pause_while_stopped();
                if job.is_background() {
                    job.is_cancelled()
                } else {
                    signals::interrupted()
                }
            }
            None => signals::interrupted(),
        }
    }

    /// Asks whether the shell may end now. With stopped jobs the first attempt is refused with
    /// a warning, and the same attempt again (an `exit` or end of input right after, with no
    /// other command in between) goes through, as in bash. Only an interactive shell with job
    /// control asks.
    pub fn confirm_exit(&mut self, io: &Io) -> bool {
        if !self.options.monitor
            || !self.interactive
            || self.in_subshell
            || !self.jobs.has_stopped()
            || self.exit_carried
            || self.exit_warned
        {
            return true;
        }
        self.exit_warned = true;
        io.say("There are stopped jobs.");
        false
    }

    /// The shell is ending: stopped jobs get `SIGHUP` (and `SIGCONT`, so they can act on it),
    /// since nobody could continue them any more. After a hangup, running jobs do too. A job
    /// that was disowned with `-h` is left alone.
    pub fn end_session(&mut self) {
        let hangup = signals::hung_up();
        for view in self.jobs.list() {
            if view.no_hup {
                continue;
            }
            match view.phase {
                Phase::Stopped(_) => {
                    let _ = view.job.signal(Signal::SIGHUP);
                }
                Phase::Running if hangup => {
                    let _ = view.job.signal(Signal::SIGHUP);
                }
                Phase::Running | Phase::Finished(_) => {}
            }
        }
    }

    /// Writes what happened to the background jobs since the last call: one line for each that
    /// finished or stopped. Call it before showing a prompt. Each line is shown once, and a
    /// finished job leaves the table.
    pub fn job_notices(&self, io: &Io) {
        for line in self.jobs.notices() {
            io.say(line);
        }
    }

    /// A path as the working directory of this shell sees it: a relative path is joined to
    /// the directory of a copy of the shell (see `cwd_override`).
    pub(crate) fn resolve(&self, path: &[u8]) -> PathBuf {
        let path = Path::new(OsStr::from_bytes(path));
        match &self.cwd_override {
            Some(base) if path.is_relative() => base.join(path),
            _ => path.to_path_buf(),
        }
    }

    /// Parses `src` and runs it. Set `at_end` when no more input will come, so that an
    /// unfinished line is reported instead of waiting for more.
    ///
    /// Messages from the shell itself ("not found", "killed by SIGSEGV") go to descriptor 2
    /// of `io`.
    pub fn run_source(&mut self, src: &str, at_end: bool, io: &mut Io) -> LineResult {
        match self.prepare(src, at_end) {
            Ok(prepared) => self.run_prepared(prepared, io),
            Err(result) => *result,
        }
    }

    /// The first half of [`run_source`](Self::run_source): replaces aliases and parses, and
    /// stops before anything runs. The caller may look at the tree (the safety analysis does)
    /// and then hand it to [`run_prepared`](Self::run_prepared).
    ///
    /// # Errors
    ///
    /// The [`LineResult`] that `run_source` would have given (boxed, it is large): `Incomplete`
    /// when more input is needed, `Rejected` when the line is wrong.
    pub fn prepare(&mut self, src: &str, at_end: bool) -> Result<Prepared, Box<LineResult>> {
        let src = &*self.expand_aliases(src).into_owned();
        self.source = Some(Arc::from(src));
        match parse(src) {
            Ok(list) => Ok(Prepared {
                list,
                text: src.to_owned(),
            }),
            Err(failure) => {
                if failure.is_incomplete() && !at_end {
                    return Err(Box::new(LineResult::Incomplete));
                }
                self.status = ExitStatus::USAGE;
                Err(Box::new(LineResult::Rejected(failure.to_diagnostic(src))))
            }
        }
    }

    /// A copy of `prepared` in which the text at each of `spans` (all inside
    /// [`Prepared::text`]) is replaced by `with`; the result is parsed again. Aliases are not
    /// replaced a second time. The shell takes the new text as the line it is running, so that
    /// the spans of the new tree match it. This is how an `rm` becomes a move to the trash.
    ///
    /// # Errors
    ///
    /// `None` if a span does not fit the text, or the result does not parse (the caller then
    /// runs nothing rather than something it did not look at).
    pub fn rewrite(
        &mut self,
        prepared: &Prepared,
        spans: &[omsh_syntax::Span],
        with: &str,
    ) -> Option<Prepared> {
        let mut spans = spans.to_vec();
        spans.sort_by_key(|span| span.start);
        let mut text = String::with_capacity(prepared.text.len() + spans.len() * with.len());
        let mut at = 0;
        for span in &spans {
            if span.start < at || span.end < span.start {
                return None;
            }
            text.push_str(prepared.text.get(at..span.start)?);
            text.push_str(with);
            at = span.end;
        }
        text.push_str(prepared.text.get(at..)?);
        let list = parse(&text).ok()?;
        self.source = Some(Arc::from(text.as_str()));
        Some(Prepared { list, text })
    }

    /// The second half of [`run_source`](Self::run_source): runs a line from
    /// [`prepare`](Self::prepare).
    pub fn run_prepared(&mut self, prepared: Prepared, io: &mut Io) -> LineResult {
        match self.run(&prepared.list, io) {
            Ok(outcome) => LineResult::Ran(outcome),
            Err(not_yet) => {
                self.status = ExitStatus::USAGE;
                LineResult::Rejected(not_yet.to_diagnostic(&prepared.text))
            }
        }
    }

    /// A handle for asking the person a question, the way `omsh ai setup` does: byte by byte
    /// from `io`'s standard input, aware of Ctrl+C.
    pub fn terminal<'a>(&'a self, io: &'a Io) -> impl Terminal + 'a {
        crate::builtins::terminal_handle(self, io)
    }

    /// Runs a parsed list.
    ///
    /// # Errors
    ///
    /// Returns [`NotYet`] if the list holds something this shell cannot run yet. This is
    /// checked for the whole list first, so in that case nothing has run.
    pub fn run(&mut self, list: &List, io: &mut Io) -> Result<Outcome, NotYet> {
        check_list(list)?;
        signals::clear_interrupt();
        self.exit_carried = std::mem::take(&mut self.exit_warned);
        let result = self.run_list(list, io);
        // `exec 3>file` changes the files of everything that comes after it, also on the next
        // line.
        io.apply(&self.fd_changes);
        self.fd_changes.clear();
        let outcome = match result {
            Ok(status) => {
                // A program killed by Ctrl+C ended the line's last command: the terminal
                // has echoed `^C`, so move to a new line, as other shells do.
                if signals::interrupted() {
                    signals::clear_interrupt();
                    if self.interactive && status == ExitStatus::from_signal(2) {
                        let _ = io.write_all(2, b"\n");
                    }
                }
                Outcome::Continue(status)
            }
            Err(Unwind::Exit(status)) => Outcome::Exit(status),
            Err(Unwind::Error(status) | Unwind::Return(status) | Unwind::Stopped(status)) => {
                Outcome::Continue(status)
            }
            Err(Unwind::Break(_) | Unwind::Continue(_)) => Outcome::Continue(self.status),
            Err(Unwind::Interrupted) => {
                signals::clear_interrupt();
                if self.interactive {
                    let _ = io.write_all(2, b"\n");
                }
                Outcome::Continue(ExitStatus::from_signal(2))
            }
        };
        if let Outcome::Continue(status) = outcome {
            self.status = status;
        }
        Ok(outcome)
    }
}

impl NotYet {
    /// Builds the report for the user. `src` must be the text that was parsed.
    #[must_use]
    pub fn to_diagnostic(&self, src: &str) -> SyntaxDiagnostic {
        SyntaxDiagnostic::new(
            src,
            self.span,
            "omsh::exec::not_yet",
            format!("{} cannot run yet", self.what),
            "not implemented yet",
            Some("Nothing in this line was run.".to_owned()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::capture::Captured;

    fn shell() -> Shell {
        Shell::with_variables(Variables::from_pairs([("PATH", "/usr/bin:/bin")]))
    }

    /// Runs a line and returns the result and what was written to descriptor 2.
    fn run(shell: &mut Shell, src: &str) -> (LineResult, String) {
        let mut captured = Captured::new();
        let result = shell.run_source(src, true, &mut captured.io);
        (result, captured.stderr())
    }

    fn status_of(shell: &mut Shell, src: &str) -> (u8, String) {
        let (result, err) = run(shell, src);
        match result {
            LineResult::Ran(Outcome::Continue(s) | Outcome::Exit(s)) => (s.code(), err),
            other => panic!("not run: {other:?}"),
        }
    }

    #[test]
    fn variable_names_lists_set_variables_sorted_and_not_bare_exports() {
        let mut sh = shell();
        let _ = status_of(&mut sh, "zeta=1; alpha=2; export bare");
        let names = sh.variable_names();
        assert!(names.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(names.contains(&"alpha".to_owned()) && names.contains(&"zeta".to_owned()));
        assert!(names.contains(&"PATH".to_owned()));
        assert!(
            !names.contains(&"bare".to_owned()),
            "exported with no value is not set"
        );
        let _ = status_of(&mut sh, "unset alpha");
        assert!(!sh.variable_names().contains(&"alpha".to_owned()));
    }

    #[test]
    fn a_command_that_succeeds_and_one_that_fails() {
        let mut sh = shell();
        assert_eq!(status_of(&mut sh, "true").0, 0);
        assert_eq!(status_of(&mut sh, "false").0, 1);
        assert_eq!(sh.last_status(), ExitStatus(1));
    }

    #[test]
    fn a_missing_command_gives_127_and_a_message() {
        let (code, err) = status_of(&mut shell(), "no-such-command-omsh");
        assert_eq!(code, 127);
        assert_eq!(err, "oh-my-shell: no-such-command-omsh: not found\n");
    }

    #[test]
    fn a_directory_gives_126() {
        let (code, err) = status_of(&mut shell(), "/tmp");
        assert_eq!(code, 126);
        assert_eq!(err, "oh-my-shell: /tmp: permission denied\n");
    }

    #[test]
    fn a_killed_program_gives_128_plus_the_signal_and_a_message() {
        let (code, err) = status_of(&mut shell(), "sh -c 'kill -SEGV $$'");
        assert_eq!(code, 139);
        assert!(err.contains("killed by SIGSEGV"), "{err}");
    }

    #[test]
    fn an_assignment_prefix_reaches_the_command_only() {
        let mut sh = shell();
        let (code, _) = status_of(&mut sh, "OMSH_X=yes sh -c 'test \"$OMSH_X\" = yes'");
        assert_eq!(code, 0);
        // It was not kept: the next command does not see it.
        let (code, _) = status_of(&mut sh, "sh -c 'test -z \"$OMSH_X\"'");
        assert_eq!(code, 0);
    }

    #[test]
    fn a_path_assignment_is_used_to_find_the_command() {
        let (code, _) = status_of(&mut shell(), "PATH=/nonexistent-omsh sh -c :");
        assert_eq!(code, 127);
    }

    #[test]
    fn exit_ends_the_shell_with_its_argument_or_the_last_status() {
        let mut sh = shell();
        assert!(matches!(
            run(&mut sh, "exit 3").0,
            LineResult::Ran(Outcome::Exit(ExitStatus(3)))
        ));
        run(&mut sh, "false");
        assert!(matches!(
            run(&mut sh, "exit").0,
            LineResult::Ran(Outcome::Exit(ExitStatus(1)))
        ));
        assert!(matches!(
            run(&mut sh, "exit 259").0,
            LineResult::Ran(Outcome::Exit(ExitStatus(3)))
        ));
    }

    #[test]
    fn exit_with_a_bad_argument_says_so_and_uses_2() {
        let (result, err) = run(&mut shell(), "exit abc");
        assert!(matches!(
            result,
            LineResult::Ran(Outcome::Exit(ExitStatus(2)))
        ));
        assert!(err.contains("numeric argument required"));
    }

    #[test]
    fn exit_stops_the_rest_of_the_line() {
        let mut sh = shell();
        run(&mut sh, "false");
        let (result, _) = run(&mut sh, "exit 5; true");
        assert!(matches!(
            result,
            LineResult::Ran(Outcome::Exit(ExitStatus(5)))
        ));
    }

    #[test]
    fn an_empty_line_keeps_the_last_status() {
        let mut sh = shell();
        run(&mut sh, "false");
        assert_eq!(status_of(&mut sh, "").0, 1);
        assert_eq!(status_of(&mut sh, "# a comment").0, 1);
    }

    #[test]
    fn an_unfinished_line_waits_unless_the_input_has_ended() {
        let mut sh = shell();
        let mut captured = Captured::new();
        assert!(matches!(
            sh.run_source("echo 'a", false, &mut captured.io),
            LineResult::Incomplete
        ));
        assert!(matches!(
            sh.run_source("echo 'a", true, &mut captured.io),
            LineResult::Rejected(_)
        ));
        assert_eq!(sh.last_status(), ExitStatus::USAGE);
    }

    #[test]
    fn a_syntax_error_is_rejected_with_status_2() {
        let mut sh = shell();
        let (result, _) = run(&mut sh, "fi");
        assert!(matches!(result, LineResult::Rejected(_)));
        assert_eq!(sh.last_status(), ExitStatus::USAGE);
    }
}
