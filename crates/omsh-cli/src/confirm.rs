//! The question before a risky line runs (§4.2 step 4, §12.1).
//!
//! Every line a person types goes through the safety analyzer after the aliases are replaced
//! and the line is parsed, and before anything runs. A line the analyzer likes runs at once.
//! One that is only worth a word gets a one-line note and runs. One at or above the level of
//! `[safety] confirm` shows what is wrong with it and waits for an answer. The panel is plain
//! text on standard error; styling comes with the theme work (S26).

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

use omsh_config::ConfirmLevel;
use omsh_editor::working_directory;
use omsh_exec::{
    ExitStatus, Io, LineResult, Outcome, Prepared, ReadError, Shell, Terminal, clear_interrupt,
};
use omsh_safety::{Analysis, Finding, FsProbe, Level, Paths, analyze};

/// What to do with the line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Run it.
    Run,
    /// Do not run it; `$?` becomes this status.
    Cancel(u8),
}

/// The status after Enter, "no", or the end of the input at the question.
const DECLINED: u8 = 1;
/// The status after Ctrl+C at the question, as for a program killed by SIGINT.
const INTERRUPTED: u8 = 130;

/// How many lines of the command the panel shows, and how many characters of each.
const SHOWN_LINES: usize = 5;
const SHOWN_WIDTH: usize = 160;

/// Prepares a typed line, asks about it if it is risky, and runs it if the answer allows.
/// Lines that are not finished or are wrong come back as `prepare` reports them.
pub fn run_line(
    shell: &mut Shell,
    text: &str,
    at_end: bool,
    files: &mut Io,
    confirm: ConfirmLevel,
) -> LineResult {
    let prepared = match shell.prepare(text, at_end) {
        Ok(prepared) => prepared,
        Err(result) => return *result,
    };
    let analysis = review(shell, &prepared);
    let decision = {
        let mut terminal = shell.terminal(files);
        decide(&analysis, prepared.text(), confirm, &mut terminal)
    };
    match decision {
        Decision::Run => shell.run_prepared(prepared, files),
        Decision::Cancel(code) => {
            // Ctrl+C at the question must not stop the next line.
            clear_interrupt();
            shell.set_last_status(ExitStatus(code));
            LineResult::Ran(Outcome::Continue(ExitStatus(code)))
        }
    }
}

/// Runs the analyzer on a line (or one step of a plan), with the shell's working directory and home folder.
pub(crate) fn review(shell: &Shell, prepared: &Prepared) -> Analysis {
    let paths = Paths {
        cwd: Some(working_directory(shell.var("PWD"))),
        home: shell
            .var("HOME")
            .filter(|home| home.starts_with(b"/"))
            .map(|home| PathBuf::from(OsStr::from_bytes(home))),
    };
    analyze(prepared.list(), prepared.text(), &paths.context(&FsProbe))
}

/// The lowest level that asks, from the setting.
const fn threshold(confirm: ConfirmLevel) -> Level {
    match confirm {
        ConfirmLevel::Caution => Level::Caution,
        ConfirmLevel::Dangerous => Level::Dangerous,
        ConfirmLevel::Critical => Level::Critical,
    }
}

/// Says what is wrong with the line and asks, if the analysis calls for it.
pub fn decide(
    analysis: &Analysis,
    text: &str,
    confirm: ConfirmLevel,
    terminal: &mut dyn Terminal,
) -> Decision {
    let level = analysis.level();
    if level == Level::Safe {
        return Decision::Run;
    }
    let asks_from = threshold(confirm);
    // What is below the line gets a note, and the line goes on.
    for finding in analysis.findings.iter().filter(|f| f.level < asks_from) {
        terminal.err(&format!(
            "oh-my-shell: {}: {}\n",
            finding.level.name(),
            finding.reason
        ));
    }
    if level < asks_from {
        return Decision::Run;
    }
    let asking: Vec<&Finding> = analysis.at_least(asks_from).collect();
    terminal.err(&panel(level, text, &asking));
    let critical: Vec<&str> = asking
        .iter()
        .filter(|finding| finding.level == Level::Critical)
        .filter_map(|finding| finding.target.as_deref())
        .collect();
    ask(level, &critical, terminal)
}

/// Asks the question for `level`: "Run it? [y/N]" for a dangerous line, and for a critical one
/// that `yes` or one of the `critical` targets be typed. Anything else cancels.
pub fn ask(level: Level, critical: &[&str], terminal: &mut dyn Terminal) -> Decision {
    let prompt = if level == Level::Critical {
        match critical.first() {
            Some(target) => format!("Type \"yes\" or \"{target}\" to run it: "),
            None => "Type \"yes\" to run it: ".to_owned(),
        }
    } else {
        "Run it? [y/N] ".to_owned()
    };
    let mut answer = Vec::new();
    match terminal.read_line(&prompt, false, &mut answer) {
        Ok(()) => {
            let answer = String::from_utf8_lossy(&answer);
            let answer = answer.trim();
            let accepted = if level == Level::Critical {
                answer.eq_ignore_ascii_case("yes") || critical.contains(&answer)
            } else {
                answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes")
            };
            if accepted {
                Decision::Run
            } else {
                terminal.err("oh-my-shell: cancelled; nothing ran\n");
                Decision::Cancel(DECLINED)
            }
        }
        Err(ReadError::Interrupted) => {
            // The terminal echoed `^C` and the cursor is still on the prompt's line.
            terminal.err("\noh-my-shell: cancelled; nothing ran\n");
            Decision::Cancel(INTERRUPTED)
        }
        Err(ReadError::EndOfInput) => {
            terminal.err("\noh-my-shell: cancelled; nothing ran\n");
            Decision::Cancel(DECLINED)
        }
        Err(ReadError::Failed) => {
            terminal.err("oh-my-shell: cannot read an answer; nothing ran\n");
            Decision::Cancel(DECLINED)
        }
    }
}

/// The text above the question: the level, the command, and one line for each reason.
fn panel(level: Level, text: &str, findings: &[&Finding]) -> String {
    let mut out = format!("oh-my-shell: this command is {}:\n", level.name());
    let mut lines = text.trim_end().lines();
    for line in lines.by_ref().take(SHOWN_LINES) {
        out.push_str("  ");
        out.push_str(&shorten(line));
        out.push('\n');
    }
    if lines.next().is_some() {
        out.push_str("  ...\n");
    }
    for finding in findings {
        out.push_str("  - ");
        out.push_str(&finding.reason);
        out.push('\n');
        if let Some(target) = &finding.target {
            out.push_str("    on: ");
            out.push_str(&shorten(target));
            out.push('\n');
        }
    }
    out
}

/// `line`, cut to the width of the panel on a character boundary.
fn shorten(line: &str) -> String {
    if line.chars().count() <= SHOWN_WIDTH {
        return line.to_owned();
    }
    let mut cut: String = line.chars().take(SHOWN_WIDTH).collect();
    cut.push_str("...");
    cut
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use omsh_config::ConfirmLevel;
    use omsh_exec::{ReadError, Shell, Terminal};
    use omsh_safety::{Context, FsProbe, analyze};
    use std::path::Path;

    use super::{Decision, decide, shorten};

    /// A terminal that gives scripted answers and keeps what was printed.
    struct Scripted {
        answers: VecDeque<Result<&'static str, ReadError>>,
        err: String,
        prompts: Vec<String>,
    }

    impl Scripted {
        fn new(answers: &[Result<&'static str, ReadError>]) -> Self {
            Self {
                answers: answers.iter().copied().collect(),
                err: String::new(),
                prompts: Vec::new(),
            }
        }
    }

    impl Terminal for Scripted {
        fn input_is_terminal(&self) -> bool {
            true
        }
        fn out(&mut self, _text: &str) {}
        fn err(&mut self, text: &str) {
            self.err.push_str(text);
        }
        fn read_line(
            &mut self,
            prompt: &str,
            _hidden: bool,
            line: &mut Vec<u8>,
        ) -> Result<(), ReadError> {
            self.prompts.push(prompt.to_owned());
            line.clear();
            match self.answers.pop_front().expect("asked more than scripted") {
                Ok(text) => {
                    line.extend_from_slice(text.as_bytes());
                    Ok(())
                }
                Err(error) => Err(error),
            }
        }
        fn interrupted(&self) -> bool {
            false
        }
    }

    fn ask(
        text: &str,
        confirm: ConfirmLevel,
        answers: &[Result<&'static str, ReadError>],
    ) -> (Decision, Scripted) {
        let mut shell = Shell::new();
        let prepared = shell.prepare(text, true).expect("the line parses");
        let ctx = Context {
            cwd: Some(Path::new("/home/me/work")),
            home: Some(Path::new("/home/me")),
            probe: &FsProbe,
        };
        let analysis = analyze(prepared.list(), prepared.text(), &ctx);
        let mut terminal = Scripted::new(answers);
        let decision = decide(&analysis, prepared.text(), confirm, &mut terminal);
        (decision, terminal)
    }

    const D: ConfirmLevel = ConfirmLevel::Dangerous;

    #[test]
    fn a_safe_line_runs_without_a_word() {
        let (decision, terminal) = ask("ls -la", D, &[]);
        assert_eq!(decision, Decision::Run);
        assert!(terminal.err.is_empty());
        assert!(terminal.prompts.is_empty());
    }

    #[test]
    fn a_caution_runs_with_one_line_and_no_question() {
        let (decision, terminal) = ask("git clean -f", D, &[]);
        assert_eq!(decision, Decision::Run);
        assert!(
            terminal.err.starts_with("oh-my-shell: caution: "),
            "{}",
            terminal.err
        );
        assert_eq!(terminal.err.lines().count(), 1);
        assert!(terminal.prompts.is_empty());
    }

    #[test]
    fn a_dangerous_line_runs_after_y_or_yes() {
        for answer in ["y", "Y", "yes", "YES", " y "] {
            let (decision, terminal) = ask("rm -rf build", D, &[Ok(answer)]);
            assert_eq!(decision, Decision::Run, "{answer:?}");
            assert_eq!(terminal.prompts, ["Run it? [y/N] "]);
        }
    }

    #[test]
    fn a_dangerous_line_is_cancelled_by_anything_else() {
        for answer in ["", "n", "no", "maybe", "yy", "ye"] {
            let (decision, terminal) = ask("rm -rf build", D, &[Ok(answer)]);
            assert_eq!(decision, Decision::Cancel(1), "{answer:?}");
            assert!(terminal.err.contains("cancelled; nothing ran"));
        }
    }

    #[test]
    fn a_critical_line_needs_yes_or_the_target_typed() {
        for answer in ["yes", "YES", "/"] {
            let (decision, terminal) = ask("rm -rf /", D, &[Ok(answer)]);
            assert_eq!(decision, Decision::Run, "{answer:?}");
            assert_eq!(terminal.prompts, ["Type \"yes\" or \"/\" to run it: "]);
        }
        for answer in ["y", "no", "", "//", "/etc", "ok"] {
            let (decision, _) = ask("rm -rf /", D, &[Ok(answer)]);
            assert_eq!(decision, Decision::Cancel(1), "{answer:?}");
        }
    }

    #[test]
    fn a_critical_line_without_a_target_asks_for_yes_only() {
        let (decision, terminal) = ask("mkfs.ext4", D, &[Ok("yes")]);
        assert_eq!(decision, Decision::Run);
        assert_eq!(terminal.prompts, ["Type \"yes\" to run it: "]);
    }

    #[test]
    fn ctrl_c_at_the_question_gives_130_and_end_of_input_gives_1() {
        let (decision, terminal) = ask("rm -rf build", D, &[Err(ReadError::Interrupted)]);
        assert_eq!(decision, Decision::Cancel(130));
        assert!(terminal.err.contains("cancelled"));
        let (decision, _) = ask("rm -rf build", D, &[Err(ReadError::EndOfInput)]);
        assert_eq!(decision, Decision::Cancel(1));
        let (decision, terminal) = ask("rm -rf build", D, &[Err(ReadError::Failed)]);
        assert_eq!(decision, Decision::Cancel(1));
        assert!(terminal.err.contains("cannot read an answer"));
    }

    #[test]
    fn the_confirm_setting_moves_the_line_between_note_and_question() {
        // `critical`: a dangerous line only gets its note.
        let (decision, terminal) = ask("rm -rf build", ConfirmLevel::Critical, &[]);
        assert_eq!(decision, Decision::Run);
        assert!(
            terminal.err.starts_with("oh-my-shell: dangerous: "),
            "{}",
            terminal.err
        );
        assert!(terminal.prompts.is_empty());
        // ... but a critical one still asks.
        let (decision, _) = ask("rm -rf /", ConfirmLevel::Critical, &[Ok("no")]);
        assert_eq!(decision, Decision::Cancel(1));
        // `caution`: a caution asks too, with the short question.
        let (decision, terminal) = ask("git clean -f", ConfirmLevel::Caution, &[Ok("y")]);
        assert_eq!(decision, Decision::Run);
        assert_eq!(terminal.prompts, ["Run it? [y/N] "]);
    }

    #[test]
    fn the_panel_shows_the_command_and_every_reason_that_asks() {
        let (_, terminal) = ask("git clean -f; rm -rf build", D, &[Ok("n")]);
        let err = &terminal.err;
        assert!(err.contains("this command is dangerous:"), "{err}");
        assert!(err.contains("  git clean -f; rm -rf build\n"), "{err}");
        assert!(err.contains("Deletes files without asking"), "{err}");
        assert!(err.contains("    on: build\n"), "{err}");
        // The caution is a note above the panel, not a line of it.
        assert!(err.starts_with("oh-my-shell: caution: "), "{err}");
        assert!(
            !err.contains("  - Deletes the files that git does not track"),
            "{err}"
        );
    }

    #[test]
    fn a_long_command_is_cut_for_the_panel() {
        let line = format!("rm -rf {}", "a".repeat(400));
        let (_, terminal) = ask(&line, D, &[Ok("n")]);
        assert!(terminal.err.contains("..."), "{}", terminal.err);
        assert!(!terminal.err.contains(&"a".repeat(200)));
        let many = "rm -rf a\n".repeat(9);
        let (_, terminal) = ask(&many, D, &[Ok("n")]);
        assert!(terminal.err.contains("  ...\n"), "{}", terminal.err);
    }

    #[test]
    fn shorten_cuts_on_characters_not_bytes() {
        let text = "ক".repeat(300);
        let cut = shorten(&text);
        assert_eq!(cut.chars().count(), 163);
        assert_eq!(shorten("short"), "short");
    }
}
