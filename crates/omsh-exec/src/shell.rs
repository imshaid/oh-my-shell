//! The shell: runs a parsed line, one command after another.

use std::ffi::CString;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use nix::errno::Errno;
use omsh_syntax::ast::{Assignment, CommandKind, List, SimpleCommand};
use omsh_syntax::{Span, SyntaxDiagnostic, parse};

use crate::env::Environment;
use crate::expand::{NotYet, Position, expand_word};
use crate::path::{Lookup, PathCache};
use crate::spawn::{ExecRequest, SpawnFailure, run as spawn};
use crate::status::ExitStatus;

/// What the shell does after a line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Read the next line. The status is the status of the last command.
    Continue(ExitStatus),
    /// The shell should end, with this status.
    Exit(ExitStatus),
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

/// One command, with its words turned into plain text.
struct Planned {
    assignments: Vec<(String, String)>,
    words: Vec<String>,
}

/// The shell's state: where commands were found, the environment, and the last status.
#[derive(Debug)]
pub struct Shell {
    paths: PathCache,
    env: Environment,
    status: ExitStatus,
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
        Self::with_environment(Environment::from_process())
    }

    /// A shell with the given environment.
    #[must_use]
    pub fn with_environment(env: Environment) -> Self {
        Self {
            paths: PathCache::new(),
            env,
            status: ExitStatus::SUCCESS,
        }
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
        let plan = plan(list)?;
        for command in &plan {
            if command.words.first().is_some_and(|w| w == "exit") {
                return Ok(Outcome::Exit(self.exit_status(&command.words, err)));
            }
            self.status = self.run_command(command, err);
        }
        Ok(Outcome::Continue(self.status))
    }

    /// The status for `exit [n]`.
    fn exit_status(&self, words: &[String], err: &mut dyn Write) -> ExitStatus {
        let Some(arg) = words.get(1) else {
            return self.status;
        };
        match arg.parse::<i64>() {
            // The kernel keeps only the low 8 bits of an exit status.
            Ok(n) => ExitStatus((n & 0xff) as u8),
            Err(_) => {
                let _ = writeln!(err, "oh-my-shell: exit: {arg}: numeric argument required");
                ExitStatus::USAGE
            }
        }
    }

    fn run_command(&mut self, command: &Planned, err: &mut dyn Write) -> ExitStatus {
        let Some(name) = command.words.first() else {
            return ExitStatus::SUCCESS;
        };
        let search_path = self.env.search_path(&command.assignments);
        let file = match self.paths.lookup(name, &search_path) {
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
        let Some(argv) = to_cstrings(&command.words) else {
            let _ = writeln!(err, "oh-my-shell: {name}: an argument holds a NUL byte");
            return ExitStatus::NOT_EXECUTABLE;
        };
        let Ok(path) = CString::new(Path::new(&file).as_os_str().as_bytes()) else {
            let _ = writeln!(err, "oh-my-shell: {name}: the path holds a NUL byte");
            return ExitStatus::NOT_EXECUTABLE;
        };
        let envp = self.env.envp(&command.assignments);

        let request = ExecRequest {
            path: &path,
            argv: &argv,
            envp: &envp,
        };
        match spawn(&request) {
            Ok(ending) => {
                if let Some(line) = ending.describe(name) {
                    let _ = writeln!(err, "{line}");
                }
                ending.status()
            }
            Err(SpawnFailure::Exec(errno)) => report_exec_failure(name, errno, err),
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

fn to_cstrings(words: &[String]) -> Option<Vec<CString>> {
    words
        .iter()
        .map(|w| CString::new(w.as_bytes()).ok())
        .collect()
}

/// Checks the whole list and expands its words. Nothing runs until this succeeds.
fn plan(list: &List) -> Result<Vec<Planned>, NotYet> {
    let mut out = Vec::with_capacity(list.items.len());
    for item in &list.items {
        if item.background {
            return Err(NotYet::new("background jobs", item.span));
        }
        let and_or = &item.and_or;
        if let Some(part) = and_or.rest.first() {
            return Err(NotYet::new("`&&` and `||`", part.pipeline.span));
        }
        let pipeline = &and_or.first;
        if pipeline.negated {
            return Err(NotYet::new("`!`", pipeline.span));
        }
        let [command] = pipeline.commands.as_slice() else {
            return Err(NotYet::new("pipelines", pipeline.span));
        };
        if let Some(redirect) = command.redirects.first() {
            return Err(NotYet::new("redirections", redirect.span));
        }
        out.push(plan_command(&command.kind, command.span)?);
    }
    Ok(out)
}

fn plan_command(kind: &CommandKind, span: Span) -> Result<Planned, NotYet> {
    let simple = match kind {
        CommandKind::Simple(simple) => simple,
        CommandKind::Subshell(_) => return Err(NotYet::new("subshells", span)),
        CommandKind::Group(_) => return Err(NotYet::new("command groups", span)),
        CommandKind::If(_) => return Err(NotYet::new("`if` commands", span)),
        CommandKind::While(_) | CommandKind::Until(_) | CommandKind::For(_) => {
            return Err(NotYet::new("loops", span));
        }
        CommandKind::Case(_) => return Err(NotYet::new("`case` commands", span)),
        CommandKind::Function(_) => return Err(NotYet::new("function definitions", span)),
    };
    plan_simple(simple, span)
}

fn plan_simple(simple: &SimpleCommand, span: Span) -> Result<Planned, NotYet> {
    if simple.words.is_empty() {
        return Err(NotYet::new("variable assignments", span));
    }
    let assignments = simple
        .assignments
        .iter()
        .map(expand_assignment)
        .collect::<Result<Vec<_>, _>>()?;
    let words = simple
        .words
        .iter()
        .map(|word| expand_word(word, Position::Argument))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Planned { assignments, words })
}

fn expand_assignment(assignment: &Assignment) -> Result<(String, String), NotYet> {
    let value = expand_word(&assignment.value, Position::AssignmentValue)?;
    Ok((assignment.name.clone(), value))
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
        Shell::with_environment(Environment::from_pairs([("PATH", "/usr/bin:/bin")]))
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
            ("A=1", "variable assignments"),
            ("echo $x", "parameter expansion"),
            ("echo *", "pathname expansion"),
            ("echo ~", "tilde expansion"),
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
