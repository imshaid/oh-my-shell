//! How long the lexer and the parser take (S19a). A report, not a gate: the budget of blueprint
//! section 16 that depends on them (keystroke to redraw) is gated by the `keystroke` benchmark
//! of `omsh-editor` and by `omsh-perf`. This one is for finding out where time goes, and for the
//! cost of the arithmetic look-ahead (`$((`), which was an open gap.
//!
//! Run with `cargo bench -p omsh-syntax --bench parse`. A plain program, no framework.

use std::hint::black_box;
use std::time::{Duration, Instant};

use omsh_syntax::{lex, lex_for_highlight, parse, parse_tolerant};

const TEMPLATE: &str = "git commit -m 'fix the parser' && cargo build --release 2>&1 | \
tee build.log ; ls -la /usr/bin/*.so $HOME/src ~/notes.txt \"a b\" nosuchprogram --flag=value # done ";

fn line_of(len: usize) -> String {
    TEMPLATE.chars().cycle().take(len).collect()
}

/// Median time of `work` over `runs` runs.
fn median(runs: usize, mut work: impl FnMut()) -> Duration {
    let mut times: Vec<Duration> = (0..runs)
        .map(|_| {
            let start = Instant::now();
            work();
            start.elapsed()
        })
        .collect();
    times.sort_unstable();
    times[times.len() / 2]
}

fn main() {
    let mut inputs: Vec<(String, String)> = [10_usize, 100, 499]
        .iter()
        .map(|len| (format!("{len}-character line"), line_of(*len)))
        .collect();
    let script: String = (0..2000)
        .map(|i| format!("if [ -f file{i} ]; then echo \"$(basename file{i})\" | tr a b; fi\n"))
        .collect();
    inputs.push(("2,000-line script".to_owned(), script));
    inputs.push((
        "20 arithmetic expansions, nested 8 deep".to_owned(),
        format!(
            "echo {}",
            "$((1+(2*(3+(4*(5+(6*(7+(8*9))))))))) ".repeat(20)
        ),
    ));
    inputs.push((
        "$(( ... without a closing )) (look-ahead fails)".to_owned(),
        format!("echo $(({}", "(1+".repeat(40)),
    ));
    println!(
        "{:>12}  {:>12}  {:>12}  {:>12}  input",
        "lex", "highlight", "parse", "tolerant"
    );
    for (name, text) in &inputs {
        let runs = if text.len() > 10_000 { 20 } else { 2000 };
        let lexed = median(runs, || {
            black_box(lex(black_box(text)));
        });
        let highlighted = median(runs, || {
            black_box(lex_for_highlight(black_box(text)));
        });
        let parsed = median(runs, || {
            let _ = black_box(parse(black_box(text)));
        });
        let tolerant = median(runs, || {
            black_box(parse_tolerant(black_box(text)));
        });
        println!(
            "{lexed:>12.1?}  {highlighted:>12.1?}  {parsed:>12.1?}  {tolerant:>12.1?}  {name}"
        );
    }
}
