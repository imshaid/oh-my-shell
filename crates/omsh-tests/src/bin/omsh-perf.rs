//! The start-up, memory and keystroke part of the performance gate (blueprint section 16).
//!
//! It starts the real release shell on a pseudo-terminal, as a person's terminal would, and
//! measures what only a whole process can show:
//!
//! - start to first prompt (budget 30 ms), in a folder with no repository, in a repository with
//!   the real `git`, and in a repository whose `git` never answers;
//! - idle memory (RSS) at the prompt (budget 15 MB), with an empty history and with a history file
//!   of 100,000 commands (S28b: the budget of 15 MB and a regression line of 13 MB; the case reads
//!   the memory at the first prompt and after ten more prompts, which each make a new hint
//!   ranking), and with a history whose newest 20,000 commands are all different (the worst case
//!   for the hint ranking; the budget of 15 MB only);
//! - one keystroke to the end of the redraw, typing lines of 100 and 499 characters with a history
//!   file of 100,000 commands, at 11 prompts each (a new prompt is made with Ctrl+C). This includes
//!   the editor's own painting, which the benchmark in `omsh-editor` does not. Three checks:
//!   the 99th percentile of every key, the first key of each prompt included (budget 5 ms), and a
//!   cap of 25 ms on the slowest key of all. The first key of a prompt used to cost 3 to 6 ms
//!   (the hinter's first database question) and had a budget of its own; since S19b the hinter
//!   makes its list when the prompt is drawn, and the first key is measured like the others.
//!
//! `--version` and `-c true` are measured by `hyperfine` in `scripts/perf-gate.sh`.
//!
//! Usage: `omsh-perf --bin target/release/oh-my-shell [--runs 21] [--scale 1.0] [--trace-keys]`. It prints a
//! table (and appends it to `$GITHUB_STEP_SUMMARY` when that is set) and exits with status 1 when
//! a budget is missed. `--scale F` multiplies every budget, so `--scale 0.01` shows that the gate
//! can fail.

use std::error::Error;
use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use omsh_history::Store;
use omsh_tests::{Launch, Session, procfs};

type Failure = Box<dyn Error>;

const START_BUDGET: Duration = Duration::from_millis(30);
const RSS_BUDGET_KB: u64 = 15 * 1024;
/// A regression line under the 15 MB budget for the idle shell with a 100,000-row history (S28b):
/// the shell was at 13.6 to 14.1 MB there before S28b, so the budget alone left no room to see
/// growth. The line fails the gate too.
const RSS_BIG_LINE_KB: u64 = 13 * 1024;
/// Blueprint section 16: keystroke to redraw, p99, for lines under 500 characters. Measured over
/// every key, the first key of each prompt included.
const KEYSTROKE_BUDGET: Duration = Duration::from_millis(5);
/// A loose cap on the slowest key of all, first keys included: it catches a real stall.
const MAX_KEY_CAP: Duration = Duration::from_millis(25);
/// Prompts per line length: each is a new prompt (Ctrl+C), and the line is typed again.
const PROMPTS: usize = 11;
const HISTORY_ROWS: usize = 100_000;
/// The hint ranking looks at this many of the newest rows (`omsh_history::HINT_WINDOW`).
const HINT_WINDOW_ROWS: usize = 20_000;

/// What the commands of the big history look like.
#[derive(Clone, Copy, PartialEq)]
enum Shape {
    /// Six kinds of command that repeat: about 3,700 distinct ones.
    Repeating,
    /// The worst case for the hint ranking: the newest 20,000 rows are all different commands
    /// (the older rows repeat three kinds).
    Distinct,
}

impl Shape {
    /// The folder names under the scratch folder: home and working folder.
    fn folders(self) -> (&'static str, &'static str) {
        match self {
            Self::Repeating => ("khome", "kwork"),
            Self::Distinct => ("dhome", "dwork"),
        }
    }

    fn command(self, i: usize) -> String {
        if self == Self::Distinct {
            return if i < HISTORY_ROWS - HINT_WINDOW_ROWS {
                match i % 3 {
                    0 => "cargo build --release".to_owned(),
                    1 => format!("cd /home/me/src/project{}", i % 50),
                    _ => format!("make test{}", i % 20),
                }
            } else {
                // About 100 characters, like a long real command.
                format!(
                    "git commit -m 'change number {i}: update the parser and the tests for case {}' --author=dev{i}",
                    i * 7919 % 100_003
                )
            };
        }
        match i % 6 {
            0 => format!("git commit -m 'change number {i}'"),
            1 => "cargo build --release".to_owned(),
            2 => format!("cd /home/me/src/project{}", i % 50),
            3 => format!("ls -la /usr/lib/x{}", i % 300),
            4 => format!("ssh host{}.example.org", i % 40),
            _ => format!("make test{}", i % 20),
        }
    }
}
/// Marks the end of one paint of the line editor (it shows the cursor again).
const PAINT_DONE: &str = "\x1b[?25h";
const PAINT_START: &str = "\x1b[?25l";

/// A typed line, cut to the length wanted; the same text as `omsh-editor/benches/keystroke.rs`.
const TEMPLATE: &str = "git commit -m 'fix the parser' && cargo build --release 2>&1 | \
tee build.log ; ls -la /usr/bin/*.so $HOME/src ~/notes.txt \"a b\" nosuchprogram --flag=value # done ";

fn line_of(len: usize) -> String {
    TEMPLATE.chars().cycle().take(len).collect()
}

/// One row of the report.
struct Row {
    name: String,
    measured: String,
    budget: String,
    ok: bool,
}

struct Report {
    rows: Vec<Row>,
}

impl Report {
    fn add(&mut self, name: &str, measured: String, budget: String, ok: bool) {
        println!(
            "{:<58} {:>22} {:>12} {}",
            name,
            measured,
            budget,
            if ok { "ok" } else { "OVER" }
        );
        self.rows.push(Row {
            name: name.to_owned(),
            measured,
            budget,
            ok,
        });
    }

    /// A number without a budget.
    fn note(&mut self, name: &str, measured: String) {
        println!("{name:<58} {measured:>22} {:>12} -", "");
        self.rows.push(Row {
            name: name.to_owned(),
            measured,
            budget: "none".to_owned(),
            ok: true,
        });
    }

    fn markdown(&self) -> String {
        let mut text = String::from(
            "### Performance gate (omsh-perf)\n\n| Case | Measured | Budget | |\n|---|---|---|---|\n",
        );
        for row in &self.rows {
            let _ = writeln!(
                text,
                "| {} | {} | {} | {} |",
                row.name,
                row.measured,
                row.budget,
                if row.ok { "ok" } else { "**OVER**" }
            );
        }
        text.push('\n');
        text
    }
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    let index = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[index]
}

fn ms(duration: Duration) -> String {
    format!("{:.2} ms", duration.as_secs_f64() * 1000.0)
}

/// Where the case runs and with which `PATH`.
struct Place {
    dir: PathBuf,
    path: String,
}

/// Starts the shell and returns the time until its prompt `$ ` is on the screen.
fn first_prompt(bin: &str, home: &Path, place: &Place) -> Result<(Duration, Session), Failure> {
    let launch = Launch::new(bin)
        .home(home)
        .cwd(&place.dir)
        .env("PATH", &place.path);
    let begin = Instant::now();
    let mut session = launch.start_raw()?;
    while !session.transcript().contains("$ ") {
        if begin.elapsed() > Duration::from_secs(10) {
            return Err(format!(
                "no prompt after 10 s; the terminal showed:\n{}",
                session.transcript()
            )
            .into());
        }
        session.read_some(Duration::from_millis(1));
    }
    Ok((begin.elapsed(), session))
}

fn run(args: &[String]) -> Result<bool, Failure> {
    let mut bin = None;
    let mut runs = 21_usize;
    let mut scale = 1.0_f64;
    let mut trace = false;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--bin" => bin = iter.next().cloned(),
            "--runs" => runs = iter.next().ok_or("--runs needs a number")?.parse()?,
            "--trace-keys" => trace = true,
            "--scale" => scale = iter.next().ok_or("--scale needs a number")?.parse()?,
            other => return Err(format!("unknown option {other}").into()),
        }
    }
    let bin = bin.ok_or("usage: omsh-perf --bin PATH [--runs N] [--scale F]")?;
    let bin = fs::canonicalize(&bin)?.to_string_lossy().into_owned();
    let start_budget = START_BUDGET.mul_f64(scale);
    let rss_budget = (RSS_BUDGET_KB as f64 * scale) as u64;

    let scratch = std::env::temp_dir().join(format!("omsh-perf-{}", std::process::id()));
    let _ = fs::remove_dir_all(&scratch);
    fs::create_dir_all(&scratch)?;
    let outcome = measure(
        &bin,
        &scratch,
        runs,
        start_budget,
        rss_budget,
        (scale, trace),
    );
    let _ = fs::remove_dir_all(&scratch);
    let report = outcome?;

    if let Some(summary) = std::env::var_os("GITHUB_STEP_SUMMARY") {
        let mut file = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(summary)?;
        file.write_all(report.markdown().as_bytes())?;
    }
    let over: Vec<&str> = report
        .rows
        .iter()
        .filter(|row| !row.ok)
        .map(|row| row.name.as_str())
        .collect();
    if over.is_empty() {
        println!("every budget measured here is met");
        Ok(true)
    } else {
        println!("OVER BUDGET: {}", over.join("; "));
        Ok(false)
    }
}

fn git_ok(dir: &Path, args: &[&str]) -> Result<(), Failure> {
    let status = Command::new("git")
        .args([
            "-c",
            "user.name=perf",
            "-c",
            "user.email=perf@example.invalid",
        ])
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("git {args:?} failed in {}", dir.display()).into())
    }
}

fn measure(
    bin: &str,
    scratch: &Path,
    runs: usize,
    start_budget: Duration,
    rss_budget_kb: u64,
    (scale, trace): (f64, bool),
) -> Result<Report, Failure> {
    let mut report = Report { rows: Vec::new() };
    println!(
        "{:<58} {:>22} {:>12}\n{}",
        "case",
        "measured",
        "budget",
        "-".repeat(100)
    );
    let home = scratch.join("home");
    let plain = scratch.join("plain");
    let repo = scratch.join("repo");
    for dir in [&home, &plain, &repo] {
        fs::create_dir_all(dir)?;
    }
    git_ok(&repo, &["init", "-q", "-b", "main"])?;
    fs::write(repo.join("file.txt"), "one\n")?;
    git_ok(&repo, &["add", "file.txt"])?;
    git_ok(&repo, &["commit", "-q", "-m", "first"])?;
    // A `git` that never answers, found before the real one.
    let fake = scratch.join("fakebin");
    fs::create_dir_all(&fake)?;
    fs::write(fake.join("git"), "#!/bin/sh\nexec sleep 30\n")?;
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(fake.join("git"), fs::Permissions::from_mode(0o755))?;
    }

    let normal_path = "/usr/bin:/bin".to_owned();
    let cases = [
        (
            "start to first prompt, no repository",
            Place {
                dir: plain.clone(),
                path: normal_path.clone(),
            },
        ),
        (
            "start to first prompt, repository, real git",
            Place {
                dir: repo.clone(),
                path: normal_path.clone(),
            },
        ),
        (
            "start to first prompt, repository, git never answers",
            Place {
                dir: repo.clone(),
                path: format!("{}:{normal_path}", fake.display()),
            },
        ),
    ];
    for (name, place) in &cases {
        // The first start makes the config file; the measured ones are warm.
        drop(first_prompt(bin, &home, place)?);
        let mut times = Vec::new();
        for _ in 0..runs {
            let (time, session) = first_prompt(bin, &home, place)?;
            drop(session);
            times.push(time);
        }
        times.sort_unstable();
        let median = percentile(&times, 0.5);
        report.add(
            &format!(
                "{name} (median of {runs}; p90 {})",
                ms(percentile(&times, 0.9))
            ),
            ms(median),
            ms(start_budget),
            median < start_budget,
        );
    }

    // Idle memory at the prompt, then after one Tab.
    let place = &cases[0].1;
    let (_, mut session) = first_prompt(bin, &home, place)?;
    session.pump(Duration::from_millis(400));
    let pid = session.shell_pid();
    let rss = procfs::status_number(pid, "VmRSS").ok_or("no VmRSS in /proc")?;
    report.add(
        "idle RSS at the prompt",
        format!("{:.2} MB", rss as f64 / 1024.0),
        format!("{:.1} MB", rss_budget_kb as f64 / 1024.0),
        rss < rss_budget_kb,
    );
    report.note(
        "threads at the prompt",
        procfs::status_number(pid, "Threads")
            .unwrap_or(0)
            .to_string(),
    );
    session.write_bytes(b"gi\t")?;
    session.pump(Duration::from_millis(400));
    let after_tab = procfs::status_number(pid, "VmRSS").unwrap_or(0);
    report.note(
        "RSS after one Tab on a command word",
        format!("{:.2} MB", after_tab as f64 / 1024.0),
    );
    drop(session);

    let big_line = (RSS_BIG_LINE_KB as f64 * scale) as u64;
    for shape in [Shape::Repeating, Shape::Distinct] {
        idle_with_big_history(bin, scratch, &mut report, shape, (rss_budget_kb, big_line))?;
    }
    keystrokes(bin, scratch, &mut report, scale, trace)?;
    Ok(report)
}

/// Makes (or reuses) the home with a history file of [`HISTORY_ROWS`] commands, plus the `typed`
/// lines, and returns the home and the working folder. The keystroke case and the idle memory
/// case share it, so the file is made once.
fn seed_history(
    scratch: &Path,
    typed: &[String],
    shape: Shape,
) -> Result<(PathBuf, PathBuf), Failure> {
    let (home, work) = shape.folders();
    let (home, work) = (scratch.join(home), scratch.join(work));
    if home.join(".local/share/oh-my-shell/history.db").is_file() {
        return Ok((home, work));
    }
    fs::create_dir_all(&work)?;
    let data = home.join(".local/share/oh-my-shell");
    fs::create_dir_all(&data)?;
    let cwd = work.to_string_lossy().into_owned();
    let store = Store::open(&data.join("history.db"), u32::MAX);
    let folders = [cwd.as_str(), "/home/me/src/web", "/home/me", "/etc", "/tmp"];
    for i in 0..HISTORY_ROWS {
        let command = shape.command(i);
        let token = store.start(&command, folders[i % folders.len()]);
        store.finish(token, i32::from(i % 17 == 0), Duration::from_millis(5));
    }
    for line in typed {
        let token = store.start(line, &cwd);
        store.finish(token, 0, Duration::from_millis(5));
    }
    if !store.settle(Duration::from_secs(120)) {
        return Err("the history file was not written in time".into());
    }
    store.close(Duration::from_secs(5));
    Ok((home, work))
}

/// Idle memory with a history file of 100,000 commands (S28b), of the given `shape`. The
/// empty-history case above cannot see what the history costs: SQLite's page cache, the hint
/// ranking and the reader.
/// Reads the memory at the first prompt and again after ten more prompts (each makes a new hint
/// ranking), and judges the larger one.
fn idle_with_big_history(
    bin: &str,
    scratch: &Path,
    report: &mut Report,
    shape: Shape,
    (budget_kb, line_kb): (u64, u64),
) -> Result<(), Failure> {
    let typed = vec![line_of(100), line_of(499)];
    let (home, work) = seed_history(scratch, &typed, shape)?;
    let place = Place {
        dir: work,
        path: "/usr/bin:/bin".to_owned(),
    };
    let (_, mut session) = first_prompt(bin, &home, &place)?;
    // Waits for the history writer (the `history` builtin settles it); a leading blank keeps the
    // line out of the history.
    session.command(" history -n 1 >/dev/null")?;
    session.pump(Duration::from_millis(500));
    let pid = session.shell_pid();
    let first = procfs::status_number(pid, "VmRSS").ok_or("no VmRSS in /proc")?;
    for _ in 0..10 {
        session.command(" true")?;
        session.pump(Duration::from_millis(100));
    }
    session.pump(Duration::from_millis(400));
    let later = procfs::status_number(pid, "VmRSS").ok_or("no VmRSS in /proc")?;
    let anon = procfs::status_number(pid, "RssAnon").unwrap_or(0);
    let file = procfs::status_number(pid, "RssFile").unwrap_or(0);
    let worst = first.max(later);
    let mb = |kb: u64| format!("{:.2} MB", kb as f64 / 1024.0);
    let what = match shape {
        Shape::Repeating => format!("{HISTORY_ROWS}-row history"),
        Shape::Distinct => format!(
            "{HISTORY_ROWS}-row history, the newest {HINT_WINDOW_ROWS} all different commands"
        ),
    };
    report.add(
        &format!(
            "idle RSS, {what} (first prompt {}, after 10 prompts {})",
            mb(first),
            mb(later)
        ),
        mb(worst),
        mb(budget_kb),
        worst < budget_kb,
    );
    // The regression line belongs to the ordinary history; the worst case has the budget only.
    if shape == Shape::Repeating {
        report.add(
            "idle RSS, 100,000-row history, regression line",
            mb(worst),
            mb(line_kb),
            worst < line_kb,
        );
    }
    report.note(
        "of which RssAnon / RssFile (after 10 prompts)",
        format!("{} / {}", mb(anon), mb(file)),
    );
    drop(session);
    Ok(())
}

/// Types lines one key at a time and times each key until the editor has finished painting.
fn keystrokes(
    bin: &str,
    scratch: &Path,
    report: &mut Report,
    scale: f64,
    trace: bool,
) -> Result<(), Failure> {
    let lengths = [100_usize, 499];
    let typed: Vec<String> = lengths.iter().map(|len| line_of(*len)).collect();
    let (home, work) = seed_history(scratch, &typed, Shape::Repeating)?;
    let place = Place {
        dir: work,
        path: "/usr/bin:/bin".to_owned(),
    };
    let mut worst = Duration::ZERO;
    let mut firsts: Vec<Duration> = Vec::new();
    let mut slowest = Duration::ZERO;
    let (mut paints, mut keys_typed) = (0_usize, 0_usize);
    for (len, line) in lengths.iter().zip(&typed) {
        let (_, mut session) = first_prompt(bin, &home, &place)?;
        let mut others = Vec::new();
        let mut firsts_here = Vec::new();
        for prompt in 0..PROMPTS {
            // Let the history reader open and the prompt settle, as it does while a person
            // looks at the screen. Every prompt after the first is a new one, made with Ctrl+C.
            if prompt > 0 {
                let from = session.transcript().len();
                session.write_bytes(&[3])?;
                let begin = Instant::now();
                while !session.transcript()[from..].contains("$ ") {
                    if begin.elapsed() > Duration::from_secs(5) {
                        return Err("no new prompt after Ctrl+C".into());
                    }
                    session.read_some(Duration::from_millis(1));
                }
            }
            session.pump(Duration::from_millis(if prompt == 0 { 500 } else { 200 }));
            let typing_from = session.transcript().len();
            for (index, ch) in line.chars().enumerate() {
                let mut bytes = [0_u8; 4];
                let from = session.transcript().len();
                let begin = Instant::now();
                session.write_bytes(ch.encode_utf8(&mut bytes).as_bytes())?;
                while !session.transcript()[from..].contains(PAINT_DONE) {
                    if begin.elapsed() > Duration::from_secs(5) {
                        return Err(format!("no repaint after a key ({len} characters)").into());
                    }
                    session.read_some(Duration::from_millis(1));
                }
                let took = begin.elapsed();
                if trace && took > Duration::from_millis(1) {
                    println!(
                        "  slow key: {len}-character line, prompt {} key {} ({ch:?}): {}",
                        prompt + 1,
                        index + 1,
                        ms(took)
                    );
                }
                slowest = slowest.max(took);
                if index == 0 {
                    firsts_here.push(took);
                } else {
                    others.push(took);
                }
            }
            paints += session.transcript()[typing_from..]
                .matches(PAINT_START)
                .count();
            keys_typed += line.chars().count();
        }
        others.sort_unstable();
        firsts_here.sort_unstable();
        let mut every_key = others.clone();
        every_key.extend_from_slice(&firsts_here);
        every_key.sort_unstable();
        let p99 = percentile(&every_key, 0.99);
        worst = worst.max(p99);
        report.note(
            &format!(
                "keystroke, {len}-character line, {} keys of {PROMPTS} prompts: p50 {}, p99.9 {}, max {}",
                every_key.len(),
                ms(percentile(&every_key, 0.5)),
                ms(percentile(&every_key, 0.999)),
                ms(every_key[every_key.len() - 1])
            ),
            format!("p99 {}", ms(p99)),
        );
        report.note(
            &format!(
                "of which the first key after a prompt, {len}-character line, {PROMPTS} prompts: min {}, max {}",
                ms(firsts_here[0]),
                ms(firsts_here[firsts_here.len() - 1])
            ),
            format!("median {}", ms(percentile(&firsts_here, 0.5))),
        );
        firsts.extend(firsts_here);
        drop(session);
    }
    report.note(
        "paints per key, all lines (1.00 expected)",
        format!("{:.2}", paints as f64 / keys_typed as f64),
    );
    let budget = KEYSTROKE_BUDGET.mul_f64(scale);
    report.add(
        "keystroke to end of redraw, worst p99 (every key, first keys included)",
        ms(worst),
        ms(budget),
        worst < budget,
    );
    firsts.sort_unstable();
    report.note(
        &format!(
            "first key after a prompt, all {} prompts: max",
            firsts.len()
        ),
        ms(firsts[firsts.len() - 1]),
    );
    let cap = MAX_KEY_CAP.mul_f64(scale);
    report.add(
        "slowest key of all (first keys included), a cap against stalls",
        ms(slowest),
        ms(cap),
        slowest < cap,
    );
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(error) => {
            eprintln!("omsh-perf: {error}");
            ExitCode::from(2)
        }
    }
}
