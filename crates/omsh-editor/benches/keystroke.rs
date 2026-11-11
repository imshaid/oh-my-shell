//! How long the highlighter takes per keystroke (blueprint §16: keystroke to redraw, highlight
//! and hint together, under 5 ms at the 99th percentile for lines under 500 characters).
//!
//! Typing a line is simulated by highlighting each prefix of it, one call per keystroke. Two
//! cases are measured:
//!
//! - **warm**: the answers about command names and files are remembered for the whole line, as
//!   they are while one prompt is being typed;
//! - **cold**: everything is forgotten before every keystroke, so each word is looked up on the
//!   real file system again. This is far worse than typing really is.
//!
//! The hint comes from a real history file of 100,000 commands (blueprint §16), made for the
//! run in a scratch folder. The typed lines are commands that are in that history, so every
//! keystroke has a hint to find (the expensive case; a line with no hint costs one query at
//! most). Each keystroke is timed as the highlighter plus the hinter, which is what the editor
//! does before it redraws.
//!
//! The command lookup is a real search of `$PATH` with `stat`, like the shell's own. Run with
//! `cargo bench -p omsh-editor`. This is a plain program with no benchmark framework; CI gating
//! with criterion is S19's job.

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use nu_ansi_term::Style;
use omsh_editor::{CommandLookup, HintReset, ShellHighlighter, ShellHinter};
use omsh_history::Store;
use omsh_ui::Theme;
use reedline::{FileBackedHistory, Highlighter, Hinter};

/// Looks for a program in `$PATH` the way the shell does, without any memory of its own.
struct PathSearch {
    path: Vec<PathBuf>,
}

impl CommandLookup for PathSearch {
    fn is_command(&self, name: &str) -> bool {
        name == "cd"
            || self
                .path
                .iter()
                .any(|dir| fs::metadata(dir.join(name)).is_ok())
    }
}

const TEMPLATE: &str = "git commit -m 'fix the parser' && cargo build --release 2>&1 | \
tee build.log ; ls -la /usr/bin/*.so $HOME/src ~/notes.txt \"a b\" nosuchprogram --flag=value # done ";

/// A line of exactly `len` characters, cut out of a repeated template.
fn line_of(len: usize) -> String {
    TEMPLATE.chars().cycle().take(len).collect()
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    let index = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[index]
}

/// What the editor does for one keystroke, before it redraws.
struct Typing<'a> {
    highlighter: &'a ShellHighlighter,
    hinter: ShellHinter,
    reset: HintReset,
    history: FileBackedHistory,
}

impl Typing<'_> {
    fn new_prompt(&self) {
        self.highlighter.new_prompt();
        self.reset.new_prompt();
    }

    fn keystroke(&mut self, line: &str, end: usize) {
        let styled = self.highlighter.highlight(&line[..end], end);
        let hint = self
            .hinter
            .handle(&line[..end], end, &self.history, true, "/");
        std::hint::black_box((styled, hint));
    }
}

fn measure(typing: &mut Typing<'_>, line: &str, cold: bool, rounds: usize) -> Vec<Duration> {
    let ends: Vec<usize> = (1..=line.len())
        .filter(|e| line.is_char_boundary(*e))
        .collect();
    let mut times = Vec::with_capacity(rounds * ends.len());
    for _ in 0..rounds {
        typing.new_prompt();
        for &end in &ends {
            if cold {
                typing.new_prompt();
            }
            let start = Instant::now();
            typing.keystroke(line, end);
            times.push(start.elapsed());
        }
    }
    times.sort_unstable();
    times
}

/// A history file of `rows` commands in a folder of its own, a mix like a real one, and the
/// typed lines themselves among them.
fn make_history(dir: &std::path::Path, rows: usize, typed: &[String]) -> Store {
    let store = Store::open(&dir.join("history.db"), u32::MAX);
    let folders = [
        "/home/me/src/api",
        "/home/me/src/web",
        "/home/me",
        "/etc",
        "/tmp",
    ];
    let started = Instant::now();
    for i in 0..rows {
        let command = match i % 6 {
            0 => format!("git commit -m 'change number {i}'"),
            1 => "cargo build --release".to_owned(),
            2 => format!("cd /home/me/src/project{}", i % 50),
            3 => format!("ls -la /usr/lib/x{}", i % 300),
            4 => format!("ssh host{}.example.org", i % 40),
            _ => format!("make test{}", i % 20),
        };
        let token = store.start(&command, folders[i % folders.len()]);
        store.finish(token, i32::from(i % 17 == 0), Duration::from_millis(5));
    }
    for line in typed {
        let token = store.start(line, "/home/me/src/api");
        store.finish(token, 0, Duration::from_millis(5));
    }
    assert!(store.settle(Duration::from_secs(120)));
    println!(
        "history of {rows} commands made in {:.1?}",
        started.elapsed()
    );
    store
}

fn main() {
    let path: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|value| std::env::split_paths(&value).collect())
        .unwrap_or_default();
    println!("PATH has {} directories", path.len());
    let highlighter = ShellHighlighter::standalone(
        Theme::default(),
        Box::new(PathSearch { path }),
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
        std::env::var_os("HOME").map(PathBuf::from),
    );
    let dir = std::env::temp_dir().join(format!("omsh-bench-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let lengths = [10, 100, 499];
    let typed: Vec<String> = lengths.iter().map(|len| line_of(*len)).collect();
    let store = make_history(&dir, 100_000, &typed);
    let (hinter, reset) = ShellHinter::standalone(
        store.clone(),
        "/home/me/src/api",
        Style::new().fg(nu_ansi_term::Color::DarkGray),
    );
    let mut typing = Typing {
        highlighter: &highlighter,
        hinter,
        reset,
        history: FileBackedHistory::default(),
    };
    // One run first, so that the file system cache is warm for every case.
    measure(&mut typing, &line_of(499), true, 1);

    // The hint's question alone, on the first keystroke of a prompt.
    let mut first = Vec::new();
    for _ in 0..200 {
        let start = Instant::now();
        std::hint::black_box(store.hint("g", "/home/me/src/api", omsh_history::now_ms()));
        first.push(start.elapsed());
    }
    first.sort_unstable();
    println!(
        "hint query alone, 1-character prefix: p50 {:.1?}, p99 {:.1?}, max {:.1?}",
        percentile(&first, 0.5),
        percentile(&first, 0.99),
        first[first.len() - 1]
    );

    println!(
        "{:>6} {:>5} {:>9} {:>9} {:>9} {:>9}",
        "length", "case", "p50", "p99", "max", "calls"
    );
    let mut worst = Duration::ZERO;
    let mut worst_cold = Duration::ZERO;
    for len in lengths {
        let line = line_of(len);
        for (name, cold) in [("warm", false), ("cold", true)] {
            let times = measure(&mut typing, &line, cold, if cold { 20 } else { 200 });
            let p99 = percentile(&times, 0.99);
            if cold {
                worst_cold = worst_cold.max(p99);
            } else {
                worst = worst.max(p99);
            }
            println!(
                "{len:>6} {name:>5} {:>9.1?} {:>9.1?} {:>9.1?} {:>9}",
                percentile(&times, 0.5),
                p99,
                times[times.len() - 1],
                times.len()
            );
        }
    }
    println!(
        "worst warm p99: {worst:.1?}; worst cold p99: {worst_cold:.1?} \
         (budget: 5 ms for highlight + hint + redraw)"
    );
    drop(typing);
    store.close(Duration::from_secs(5));
    let _ = fs::remove_dir_all(&dir);
}
