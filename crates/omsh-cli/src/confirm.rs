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
use omsh_safety::blast::Radius;
use omsh_safety::{Analysis, Finding, Level, Paths, analyze};

use crate::probe::ShellProbe;
use crate::radius::{self, BlastLimits};

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
    blast: BlastLimits,
) -> LineResult {
    let prepared = match shell.prepare(text, at_end) {
        Ok(prepared) => prepared,
        Err(result) => return *result,
    };
    let analysis = review(shell, &prepared);
    let decision = {
        let mut terminal = shell.terminal(files);
        decide(
            &analysis,
            prepared.text(),
            confirm,
            Some(blast),
            &mut terminal,
        )
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
    review_at(shell, prepared, &StartDir::Shell)
}

/// Where a line starts, for the analysis.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum StartDir {
    /// The shell's working directory now.
    Shell,
    /// A directory an earlier step of a plan moved to.
    At(PathBuf),
    /// Not known: an earlier step moved somewhere that depends on how it runs. Relative paths
    /// are then not judged, as for any line whose directory is not known.
    Unknown,
}

/// Like [`review`], but the line starts where `start` says: a plan's next step starts where the
/// one before it left off.
pub(crate) fn review_at(shell: &Shell, prepared: &Prepared, start: &StartDir) -> Analysis {
    let paths = Paths {
        cwd: match start {
            StartDir::Shell => Some(working_directory(shell.var("PWD"))),
            StartDir::At(dir) => Some(dir.clone()),
            StartDir::Unknown => None,
        },
        home: shell
            .var("HOME")
            .filter(|home| home.starts_with(b"/"))
            .map(|home| PathBuf::from(OsStr::from_bytes(home))),
    };
    analyze(
        prepared.list(),
        prepared.text(),
        &paths.context(&ShellProbe),
    )
}

/// The lowest level that asks, from the setting.
pub(crate) const fn threshold(confirm: ConfirmLevel) -> Level {
    match confirm {
        ConfirmLevel::Caution => Level::Caution,
        ConfirmLevel::Dangerous => Level::Dangerous,
        ConfirmLevel::Critical => Level::Critical,
    }
}

/// Says what is wrong with the line and asks, if the analysis calls for it. With `blast`, the
/// panel counts what the file commands in it would touch (§12.3); without, it does not (tests).
pub fn decide(
    analysis: &Analysis,
    text: &str,
    confirm: ConfirmLevel,
    blast: Option<BlastLimits>,
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
    let radii = match blast {
        Some(limits) => radius::measure(&asking, limits),
        None => vec![None; asking.len()],
    };
    terminal.err(&panel(
        level,
        text,
        &asking,
        &radii,
        blast.unwrap_or_default(),
    ));
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
fn panel(
    level: Level,
    text: &str,
    findings: &[&Finding],
    radii: &[Option<Radius>],
    blast: BlastLimits,
) -> String {
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
    for (index, finding) in findings.iter().enumerate() {
        out.push_str("  - ");
        out.push_str(&finding.reason);
        out.push('\n');
        if let Some(target) = &finding.target {
            out.push_str("    on: ");
            out.push_str(&shorten(target));
            out.push('\n');
        }
        if let Some(Some(found)) = radii.get(index) {
            let cwd = finding
                .files
                .as_ref()
                .and_then(|files| files.cwd.as_deref());
            for line in radius::lines(found, cwd, blast) {
                out.push_str("    ");
                out.push_str(&line);
                out.push('\n');
            }
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
    use omsh_safety::{Context, analyze};
    use std::path::{Path, PathBuf};

    use super::{Decision, decide, shorten};
    use crate::probe::ShellProbe;
    use crate::radius::BlastLimits;

    /// Limits whose time budget cannot run out, so a busy machine cannot change a count.
    const TEST_LIMITS: BlastLimits = BlastLimits {
        max_entries: 50_000,
        max_ms: 600_000,
    };

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
        ask_in("/home/me/work", None, text, confirm, answers)
    }

    /// Like `ask`, from `cwd`, and counting what the line touches when `blast` is given.
    fn ask_in(
        cwd: &str,
        blast: Option<BlastLimits>,
        text: &str,
        confirm: ConfirmLevel,
        answers: &[Result<&'static str, ReadError>],
    ) -> (Decision, Scripted) {
        let mut shell = Shell::new();
        let prepared = shell.prepare(text, true).expect("the line parses");
        let ctx = Context {
            cwd: Some(Path::new(cwd)),
            home: Some(Path::new("/home/me")),
            probe: &ShellProbe,
        };
        let analysis = analyze(prepared.list(), prepared.text(), &ctx);
        let mut terminal = Scripted::new(answers);
        let decision = decide(&analysis, prepared.text(), confirm, blast, &mut terminal);
        (decision, terminal)
    }

    /// A folder in the temporary directory, removed at the end.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            use std::sync::atomic::{AtomicU32, Ordering};
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("omsh-confirm-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn files(&self, names: impl IntoIterator<Item = String>) {
            for name in names {
                let path = self.0.join(name);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, "12345").unwrap();
            }
        }

        fn path(&self) -> &str {
            self.0.to_str().unwrap()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
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

    #[test]
    fn the_panel_counts_what_the_command_would_touch() {
        let dir = Scratch::new();
        dir.files([
            "build/a.o".to_owned(),
            "build/lib/b.o".to_owned(),
            "build/c.o".to_owned(),
        ]);
        let (decision, terminal) =
            ask_in(dir.path(), Some(TEST_LIMITS), "rm -rf build", D, &[Ok("n")]);
        assert_eq!(decision, Decision::Cancel(1));
        assert!(
            terminal
                .err
                .contains("    touches 3 files and 2 folders (15 B of file data)\n"),
            "{}",
            terminal.err
        );
        assert!(
            terminal
                .err
                .contains("none outside the current folder; none owned by someone else")
        );
        assert!(
            terminal.err.contains("for example: build, "),
            "{}",
            terminal.err
        );
        // The counting reads the disk and changes nothing.
        assert!(dir.0.join("build/lib/b.o").exists());
    }

    #[test]
    fn without_limits_nothing_is_counted() {
        let dir = Scratch::new();
        dir.files(["build/a.o".to_owned()]);
        let (_, terminal) = ask_in(dir.path(), None, "rm -rf build", D, &[Ok("n")]);
        assert!(!terminal.err.contains("touches"), "{}", terminal.err);
    }

    #[test]
    fn a_note_below_the_asking_level_counts_nothing() {
        let dir = Scratch::new();
        dir.files(["build/a.o".to_owned()]);
        // `rm -rf build` is Dangerous; with `confirm = critical` it only gets a note.
        let (decision, terminal) = ask_in(
            dir.path(),
            Some(TEST_LIMITS),
            "rm -rf build",
            ConfirmLevel::Critical,
            &[],
        );
        assert_eq!(decision, Decision::Run);
        assert!(terminal.err.starts_with("oh-my-shell: dangerous: "));
        assert!(!terminal.err.contains("touches"));
    }

    #[test]
    fn a_pattern_over_twenty_matches_asks_and_says_how_many() {
        let dir = Scratch::new();
        dir.files((0..21).map(|i| format!("f{i}.tmp")));
        let (decision, terminal) = ask_in(dir.path(), Some(TEST_LIMITS), "rm *.tmp", D, &[Ok("n")]);
        assert_eq!(decision, Decision::Cancel(1));
        assert!(
            terminal.err.contains("The pattern matches 21 entries."),
            "{}",
            terminal.err
        );
        assert!(
            terminal.err.contains("touches 21 files"),
            "{}",
            terminal.err
        );
        assert_eq!(terminal.prompts, ["Run it? [y/N] "]);
        assert!(dir.0.join("f0.tmp").exists());
    }

    #[test]
    fn a_pattern_of_twenty_or_fewer_only_gets_a_note() {
        let dir = Scratch::new();
        dir.files((0..20).map(|i| format!("f{i}.tmp")));
        let (decision, terminal) = ask_in(dir.path(), Some(TEST_LIMITS), "rm *.tmp", D, &[]);
        assert_eq!(decision, Decision::Run);
        assert!(terminal.err.starts_with("oh-my-shell: caution: "));
        assert!(terminal.prompts.is_empty());
    }

    #[test]
    fn find_delete_asks_and_says_what_it_searches() {
        let dir = Scratch::new();
        dir.files([
            "a.log".to_owned(),
            "sub/b.log".to_owned(),
            "sub/c.txt".to_owned(),
        ]);
        let (decision, terminal) = ask_in(
            dir.path(),
            Some(TEST_LIMITS),
            "find . -name '*.log' -delete",
            D,
            &[Ok("n")],
        );
        assert_eq!(decision, Decision::Cancel(1));
        assert!(
            terminal
                .err
                .contains("searches 3 files and 2 folders (15 B of file data); it deletes only the ones its tests match"),
            "{}",
            terminal.err
        );
        assert!(dir.0.join("a.log").exists());
    }

    #[test]
    fn one_budget_covers_the_commands_of_a_line() {
        let dir = Scratch::new();
        dir.files((0..10).map(|i| format!("a/f{i}")));
        dir.files((0..10).map(|i| format!("b/f{i}")));
        let limits = BlastLimits {
            max_entries: 15,
            max_ms: 600_000,
        };
        let (_, terminal) = ask_in(
            dir.path(),
            Some(limits),
            "rm -rf a; rm -rf b",
            D,
            &[Ok("n")],
        );
        // a has 11 entries; b gets the 4 that are left and says the count stopped.
        assert!(
            terminal.err.contains("touches 10 files and 1 folder"),
            "{}",
            terminal.err
        );
        assert!(
            terminal.err.contains("touches at least"),
            "{}",
            terminal.err
        );
        assert!(
            terminal
                .err
                .contains("counting stopped at the limit of 15 entries"),
            "{}",
            terminal.err
        );
    }

    #[test]
    fn a_command_with_two_findings_is_counted_once() {
        let dir = Scratch::new();
        dir.files(["a".to_owned()]);
        // `/` is critical and `$x` is unknown: two findings about the same command. Small limits
        // keep the look at `/` short.
        let limits = BlastLimits {
            max_entries: 10,
            max_ms: 600_000,
        };
        let (decision, terminal) = ask_in(dir.path(), Some(limits), "rm -rf / $x", D, &[Ok("no")]);
        assert_eq!(decision, Decision::Cancel(1));
        assert_eq!(
            terminal.err.matches("touches").count(),
            1,
            "{}",
            terminal.err
        );
        assert!(
            terminal.err.matches("  - ").count() >= 2,
            "{}",
            terminal.err
        );
        // Counted a second time, it would find the budget used up by the first count.
        assert!(
            !terminal.err.contains("used up by earlier commands"),
            "{}",
            terminal.err
        );
    }
}
