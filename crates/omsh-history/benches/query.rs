//! How long history lookups take on a file of 100,000 commands (blueprint section 16: under
//! 20 ms). It replaces the ignored timing test that S13b had in `store/tests.rs`.
//!
//! Every lookup the shell makes is timed 100 times on a real file, made for the run in a scratch
//! folder: Up (first press, next press, with a prefix that is found and one that nothing has, which
//! reads every row), search, `history` (newest 100), `--here`, `--failed`, `--since`, and the
//! hint. The 99th percentile of each must be under the budget. The full dump `history --all` is
//! reported and has no budget: it is output, not a lookup.
//!
//! A plain program with no benchmark framework: it exits with status 1 when a budget is missed,
//! which is how CI gates it (`scripts/perf-gate.sh`). Run with
//! `cargo bench -p omsh-history --bench query`.

use std::fs;
use std::time::{Duration, Instant};

use omsh_history::{Query, Store, now_ms};

const ROWS: usize = 100_000;
const RUNS: usize = 100;
const BUDGET: Duration = Duration::from_millis(20);

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    let index = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[index]
}

/// Times `work` `RUNS` times; returns the sorted times.
fn time(mut work: impl FnMut() -> usize) -> (Vec<Duration>, usize) {
    let mut found = 0;
    let mut times = Vec::with_capacity(RUNS);
    for _ in 0..RUNS {
        let start = Instant::now();
        found = work();
        times.push(start.elapsed());
    }
    times.sort_unstable();
    (times, found)
}

fn main() {
    let dir = std::env::temp_dir().join(format!("omsh-history-bench-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    if let Err(error) = fs::create_dir_all(&dir) {
        eprintln!("query benchmark: cannot make {}: {error}", dir.display());
        std::process::exit(2);
    }
    let store = Store::open(&dir.join("history.db"), 1_000_000);
    let filling = Instant::now();
    for n in 0..ROWS {
        let cwd = format!("/home/user/project-{}", n % 50);
        let command = format!(
            "command-{} --flag value-{n} some/path/file-{n}.txt",
            n % 997
        );
        let token = store.start(&command, &cwd);
        store.finish(token, i32::from(n % 10 == 0), Duration::from_millis(7));
    }
    assert!(
        store.settle(Duration::from_secs(120)),
        "the history was not written in time"
    );
    println!("filled {ROWS} rows in {:?}", filling.elapsed());
    let newest = store.query(&Query {
        newest_first: true,
        limit: Some(1),
        ..Query::default()
    })[0]
        .id;

    let up = Query {
        own_or_older: true,
        newest_first: true,
        limit: Some(1),
        ..Query::default()
    };
    let cases: Vec<(&str, Query)> = vec![
        ("Up, first press", up.clone()),
        (
            "Up, next press (skips repeats)",
            Query {
                below_id: Some(newest),
                unlike: Some(newest),
                ..up.clone()
            },
        ),
        (
            "Up with a prefix that is found",
            Query {
                prefix: Some("command-500 ".to_owned()),
                ..up.clone()
            },
        ),
        (
            "Up with a prefix that nothing has (reads every row)",
            Query {
                prefix: Some("zzz".to_owned()),
                ..up
            },
        ),
        (
            "search for a text that nothing has (reads every row)",
            Query {
                newest_first: true,
                limit: Some(100),
                contains: Some("zzz".to_owned()),
                ..Query::default()
            },
        ),
        (
            "history (newest 100)",
            Query {
                newest_first: true,
                limit: Some(100),
                ..Query::default()
            },
        ),
        (
            "history --here (newest 100 of 2,000)",
            Query {
                newest_first: true,
                limit: Some(100),
                cwd: Some("/home/user/project-7".to_owned()),
                ..Query::default()
            },
        ),
        (
            "history --failed (newest 100)",
            Query {
                newest_first: true,
                limit: Some(100),
                failed: true,
                ..Query::default()
            },
        ),
        (
            "history --since (newest 100)",
            Query {
                newest_first: true,
                limit: Some(100),
                since_ms: Some(0),
                ..Query::default()
            },
        ),
    ];

    println!("{:>10} {:>10} {:>10}  case", "p50", "p99", "max");
    let mut worst = Duration::ZERO;
    for (name, query) in &cases {
        let (times, found) = time(|| store.query(query).len());
        let p99 = percentile(&times, 0.99);
        worst = worst.max(p99);
        println!(
            "{:>10.3?} {p99:>10.3?} {:>10.3?}  {name} ({found} rows)",
            percentile(&times, 0.5),
            times[times.len() - 1]
        );
    }
    let (times, _) = time(|| {
        store
            .hint("command-5", "/home/user/project-7", now_ms())
            .map_or(0, |hint| usize::from(hint.is_some()))
    });
    let p99 = percentile(&times, 0.99);
    worst = worst.max(p99);
    println!(
        "{:>10.3?} {p99:>10.3?} {:>10.3?}  hint for a typed prefix",
        percentile(&times, 0.5),
        times[times.len() - 1]
    );

    // Reported only: a dump of every row is output, not a lookup. This is the streamed read that
    // `history --all` uses (pages of 2,000 rows, never all in memory), and for comparison the
    // single read that builds the whole list.
    let all = Query {
        newest_first: false,
        ..Query::default()
    };
    let (streamed, rows) = time(|| {
        let mut rows = 0;
        store.query_each(&all, |entry| {
            std::hint::black_box(&entry);
            rows += 1;
            true
        });
        rows
    });
    println!(
        "{:>10.3?} (median)  history --all: streaming all {rows} rows (no budget)",
        percentile(&streamed, 0.5)
    );
    let (listed, rows) = time(|| store.query(&all).len());
    println!(
        "{:>10.3?} (median)  for comparison, one read of all {rows} rows into a list",
        percentile(&listed, 0.5)
    );

    println!("worst p99 of the lookups: {worst:.3?} (budget {BUDGET:?})");
    store.close(Duration::from_secs(5));
    let _ = fs::remove_dir_all(&dir);
    if worst > BUDGET {
        eprintln!("query benchmark: over the budget");
        std::process::exit(1);
    }
}
