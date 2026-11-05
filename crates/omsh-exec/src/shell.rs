//! The shell: runs a parsed line, one command after another.

use std::ffi::CString;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use nix::errno::Errno;
use omsh_syntax::ast::{CommandKind, List, SimpleCommand};
use omsh_syntax::{SyntaxDiagnostic, parse};

use crate::check::{NotYet, check_list};
use crate::expand::{ExpandError, Expander};
use crate::path::{Lookup, PathCache};
use crate::spawn::{ExecRequest, SpawnFailure, run as spawn};
use crate::status::ExitStatus;
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

/// The shell's state: its variables, where commands were found, and the last status.
#[derive(Clone, Debug)]
pub struct Shell {
    pub(crate) paths: PathCache,
    pub(crate) vars: Variables,
    pub(crate) status: ExitStatus,
    /// `$1`, `$2`, ... There is no way to change them yet (`set --` and functions come in
    /// S07).
    pub(crate) positional: Vec<Vec<u8>>,
    /// `$0`.
    pub(crate) name: Vec<u8>,
    /// True if a person is typing the commands, which `$-` reports as `i`.
    pub(crate) interactive: bool,
    /// The status of the last `$(...)` expanded in the current command. A command made of
    /// assignments only has this status.
    pub(crate) cmdsub_status: Option<ExitStatus>,
}

impl Default for Shell {
    fn default() -> Self {
        Self::new()
    }
}

impl Shell {
    /// A shell with the environment of this process.
    #[must_use]
    pub fn new() -> Self {
        Self::with_variables(Variables::from_process())
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
        }
    }

    /// Says whether a person is typing the commands. This only changes what `$-` shows.
    pub const fn set_interactive(&mut self, interactive: bool) {
        self.interactive = interactive;
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

    /// Parses `src` and runs it. Set `at_end` when no more input will come, so that an
    /// unfinished line is reported instead of waiting for more.
    ///
    /// Messages from the shell itself ("not found", "killed by SIGSEGV") go to `err`.
    pub fn run_source(&mut self, src: &str, at_end: bool, err: &mut dyn Write) -> LineResult {
        let list = match parse(src) {
            Ok(list) => list,
            Err(failure) => {
                if failure.is_incomplete() && !at_end {
                    return LineResult::Incomplete;
                }
                self.status = ExitStatus::USAGE;
                return LineResult::Rejected(failure.to_diagnostic(src));
            }
        };
        match self.run(&list, err) {
            Ok(outcome) => LineResult::Ran(outcome),
            Err(not_yet) => {
                self.status = ExitStatus::USAGE;
                LineResult::Rejected(not_yet.to_diagnostic(src))
            }
        }
    }

    /// Runs a parsed list.
    ///
    /// # Errors
    ///
    /// Returns [`NotYet`] if the list holds something this shell cannot run yet. This is
    /// checked for the whole list first, so in that case nothing has run.
    pub fn run(&mut self, list: &List, err: &mut dyn Write) -> Result<Outcome, NotYet> {
        check_list(list)?;
        Ok(self.execute(list, err, None))
    }

    /// Runs a list that [`check_list`] accepted. Each command is expanded just before it
    /// runs, because it can depend on the commands before it (`x=1; echo $x`).
    ///
    /// With `capture`, the output of the programs goes into the buffer instead of to the
    /// terminal.
    pub(crate) fn execute(
        &mut self,
        list: &List,
        err: &mut dyn Write,
        mut capture: Option<&mut Vec<u8>>,
    ) -> Outcome {
        for item in &list.items {
            let command = &item.and_or.first.commands[0];
            let CommandKind::Simple(simple) = &command.kind else {
                debug_assert!(false, "check_list lets only simple commands through");
                continue;
            };
            match self.run_simple(simple, err, capture.as_deref_mut()) {
                Ok(None) => {}
                Ok(Some(status)) => return Outcome::Exit(status),
                Err(ExpandError(message)) => {
                    let _ = writeln!(err, "oh-my-shell: {message}");
                    self.status = ExitStatus::USAGE;
                    return Outcome::Continue(ExitStatus::USAGE);
                }
            }
        }
        Outcome::Continue(self.status)
    }

    /// Runs one simple command. Returns a status if the shell should exit with it.
    fn run_simple(
        &mut self,
        simple: &SimpleCommand,
        err: &mut dyn Write,
        capture: Option<&mut Vec<u8>>,
    ) -> Result<Option<ExitStatus>, ExpandError> {
        self.cmdsub_status = None;
        // The words are expanded first, then the values of the assignments.
        let words = Expander { shell: self, err }.expand_words(&simple.words)?;

        if words.is_empty() {
            // Only assignments: they stay. The status is that of the last `$(...)`, if any.
            Expander { shell: self, err }.apply_assignments(&simple.assignments)?;
            self.status = self.cmdsub_status.unwrap_or(ExitStatus::SUCCESS);
            return Ok(None);
        }
        if words[0] == b"exit" {
            return Ok(Some(self.exit_status(&words, err)));
        }
        if simple.assignments.is_empty() {
            self.status = self.run_command(&words, &[], err, capture);
            return Ok(None);
        }
        // Assignments in front of a command last for that command only. They are applied to
        // the variables so that later ones can use earlier ones, and then taken back.
        let saved = self.vars.clone();
        let applied = Expander { shell: self, err }.apply_assignments(&simple.assignments);
        let result = applied.map(|assignments| {
            self.status = self.run_command(&words, &assignments, err, capture);
        });
        self.vars = saved;
        result.map(|()| None)
    }

    /// The status for `exit [n]`.
    fn exit_status(&self, words: &[Vec<u8>], err: &mut dyn Write) -> ExitStatus {
        let Some(arg) = words.get(1) else {
            return self.status;
        };
        match std::str::from_utf8(arg)
            .ok()
            .and_then(|s| s.parse::<i64>().ok())
        {
            // The kernel keeps only the low 8 bits of an exit status.
            Some(n) => ExitStatus((n & 0xff) as u8),
            None => {
                let _ = writeln!(
                    err,
                    "oh-my-shell: exit: {}: numeric argument required",
                    String::from_utf8_lossy(arg)
                );
                ExitStatus::USAGE
            }
        }
    }

    fn run_command(
        &mut self,
        words: &[Vec<u8>],
        assignments: &[(String, Vec<u8>)],
        err: &mut dyn Write,
        capture: Option<&mut Vec<u8>>,
    ) -> ExitStatus {
        let name = String::from_utf8_lossy(&words[0]).into_owned();
        let search_path = self.vars.search_path(assignments);
        let file = match self.paths.lookup(&words[0], &search_path) {
            Lookup::Found(file) => file,
            Lookup::Denied(_) => {
                let _ = writeln!(err, "oh-my-shell: {name}: permission denied");
                return ExitStatus::NOT_EXECUTABLE;
            }
            Lookup::NotFound => {
                let _ = writeln!(err, "oh-my-shell: {name}: not found");
                return ExitStatus::NOT_FOUND;
            }
        };
        let Some(argv) = to_cstrings(words) else {
            let _ = writeln!(err, "oh-my-shell: {name}: an argument holds a NUL byte");
            return ExitStatus::NOT_EXECUTABLE;
        };
        let Ok(path) = CString::new(Path::new(&file).as_os_str().as_bytes()) else {
            let _ = writeln!(err, "oh-my-shell: {name}: the path holds a NUL byte");
            return ExitStatus::NOT_EXECUTABLE;
        };
        let envp = self.vars.envp(assignments);

        let request = ExecRequest {
            path: &path,
            argv: &argv,
            envp: &envp,
        };
        match spawn(&request, capture) {
            Ok(ending) => {
                if let Some(line) = ending.describe(&name) {
                    let _ = writeln!(err, "{line}");
                }
                ending.status()
            }
            Err(SpawnFailure::Exec(errno)) => report_exec_failure(&name, errno, err),
            Err(SpawnFailure::Fork(errno)) => {
                let _ = writeln!(err, "oh-my-shell: cannot start {name}: {}", errno.desc());
                ExitStatus::NOT_EXECUTABLE
            }
        }
    }
}

/// Says why `execve` failed, and picks the status: 127 if there was nothing to run, 126 if
/// there was a file that could not be run.
fn report_exec_failure(name: &str, errno: Errno, err: &mut dyn Write) -> ExitStatus {
    match errno {
        Errno::ENOENT | Errno::ENOTDIR => {
            let _ = writeln!(err, "oh-my-shell: {name}: not found");
            ExitStatus::NOT_FOUND
        }
        Errno::EACCES => {
            let _ = writeln!(err, "oh-my-shell: {name}: permission denied");
            ExitStatus::NOT_EXECUTABLE
        }
        other => {
            let _ = writeln!(err, "oh-my-shell: {name}: {}", other.desc());
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

    fn shell() -> Shell {
        Shell::with_variables(Variables::from_pairs([("PATH", "/usr/bin:/bin")]))
    }

    /// Runs a line and returns the result and what was written to `err`.
    fn run(shell: &mut Shell, src: &str) -> (LineResult, String) {
        let mut err = Vec::new();
        let result = shell.run_source(src, true, &mut err);
        (result, String::from_utf8(err).unwrap())
    }

    fn status_of(shell: &mut Shell, src: &str) -> (u8, String) {
        let (result, err) = run(shell, src);
        match result {
            LineResult::Ran(Outcome::Continue(s) | Outcome::Exit(s)) => (s.code(), err),
            other => panic!("not run: {other:?}"),
        }
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
        let (code, _) = status_of(&mut shell(), "PATH=/nonexistent-omsh true");
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
        let mut err = Vec::new();
        assert!(matches!(
            sh.run_source("echo 'a", false, &mut err),
            LineResult::Incomplete
        ));
        assert!(matches!(
            sh.run_source("echo 'a", true, &mut err),
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

    #[test]
    fn something_not_implemented_runs_nothing_at_all() {
        let mut sh = shell();
        // The first command would make `exit 9` end the shell if it ran.
        let (result, _) = run(&mut sh, "exit 9; a | b");
        let LineResult::Rejected(report) = result else {
            panic!("expected a rejection");
        };
        assert_eq!(report.message(), "pipelines cannot run yet");
        assert_eq!(sh.last_status(), ExitStatus::USAGE);
    }

    #[test]
    fn every_unsupported_construct_is_named() {
        for (src, what) in [
            ("a &", "background jobs"),
            ("a && b", "`&&` and `||`"),
            ("! a", "`!`"),
            ("a | b", "pipelines"),
            ("a > f", "redirections"),
            ("(a)", "subshells"),
            ("{ a; }", "command groups"),
            ("if a; then b; fi", "`if` commands"),
            ("while a; do b; done", "loops"),
            ("for i in a; do b; done", "loops"),
            ("case a in b) c;; esac", "`case` commands"),
            ("f() { a; }", "function definitions"),
        ] {
            let (result, _) = run(&mut shell(), src);
            let LineResult::Rejected(report) = result else {
                panic!("{src} should be rejected");
            };
            assert_eq!(report.message(), format!("{what} cannot run yet"), "{src}");
        }
    }

    #[test]
    fn the_report_points_at_the_unsupported_part() {
        let (result, _) = run(&mut shell(), "echo ok; a | b");
        let LineResult::Rejected(report) = result else {
            panic!("expected a rejection");
        };
        let span = report.span();
        assert_eq!((span.start, span.end), (9, 14));
    }
}
