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
//! The command lookup is a real search of `$PATH` with `stat`, like the shell's own. Run with
//! `cargo bench -p omsh-editor`. This is a plain program with no benchmark framework; CI gating
//! with criterion is S19's job.

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use omsh_editor::{CommandLookup, ShellHighlighter};
use omsh_ui::Theme;
use reedline::Highlighter;

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

fn measure(highlighter: &ShellHighlighter, line: &str, cold: bool, rounds: usize) -> Vec<Duration> {
    let ends: Vec<usize> = (1..=line.len())
        .filter(|e| line.is_char_boundary(*e))
        .collect();
    let mut times = Vec::with_capacity(rounds * ends.len());
    for _ in 0..rounds {
        highlighter.new_prompt();
        for &end in &ends {
            if cold {
                highlighter.new_prompt();
            }
            let start = Instant::now();
            let styled = highlighter.highlight(&line[..end], end);
            times.push(start.elapsed());
            std::hint::black_box(styled);
        }
    }
    times.sort_unstable();
    times
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
    // One run first, so that the file system cache is warm for every case.
    measure(&highlighter, &line_of(499), true, 1);

    println!(
        "{:>6} {:>5} {:>9} {:>9} {:>9} {:>9}",
        "length", "case", "p50", "p99", "max", "calls"
    );
    let mut worst = Duration::ZERO;
    for len in [10, 100, 499] {
        let line = line_of(len);
        for (name, cold) in [("warm", false), ("cold", true)] {
            let times = measure(&highlighter, &line, cold, if cold { 20 } else { 200 });
            let p99 = percentile(&times, 0.99);
            if !cold {
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
    println!("worst warm p99: {worst:.1?} (budget: 5 ms for highlight + hint + redraw)");
}
