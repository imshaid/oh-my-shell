//! How long the history explorer (Ctrl+R) takes to answer a key (blueprint section 16: a key
//! must be answered in under 5 ms at the 99th percentile).
//!
//! The list is as large as the view ever gets: the newest 10,000 runs of the history, here
//! 10,000 different commands. Every case is timed from the key to the end of the drawing, the
//! drawing going to an 80 x 24 test screen:
//!
//! - typing a 12-letter query one letter at a time (each key narrows the matches of the key
//!   before), then
//! - the keys that cannot narrow: Backspace, and `Ctrl+R` (a new scope), which look at every
//!   command again, then
//! - moving the selection.
//!
//! The 99th percentile of each must be under the budget. Opening the view (making the items and
//! the model from the rows) is timed and held to the history lookup budget of 20 ms.
//!
//! A plain program with no benchmark framework: it exits with status 1 when a budget is missed,
//! which is how CI gates it (`scripts/perf-gate.sh`). Run with
//! `cargo bench -p omsh-ui --bench explorer`.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use omsh_ui::explorer::render::{Context, Styles, draw};
use omsh_ui::explorer::{Item, Model};
use omsh_ui::{Look, Theme};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

const COMMANDS: usize = 10_000;
const ROUNDS: usize = 40;
const KEY_BUDGET: Duration = Duration::from_millis(5);
const OPEN_BUDGET: Duration = Duration::from_millis(20);
const NOW: i64 = 1_800_000_000_000;

const QUERY: &str = "docker compo";

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    let index = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[index]
}

fn items() -> Vec<Item> {
    let tools = [
        "docker compose up -d",
        "docker ps -a",
        "git commit -m",
        "git push origin",
        "cargo build",
        "cargo test -p",
        "kubectl get pods",
        "ssh dev@host",
        "grep -rn todo",
        "systemctl restart",
    ];
    (0..COMMANDS)
        .map(|n| {
            let command = format!(
                "{} --opt-{} value-{n} some/path/file-{n}.txt",
                tools[n % tools.len()],
                n % 97
            );
            let status = if n % 9 == 0 { Some(1) } else { Some(0) };
            Item::new(
                command,
                format!("/home/user/project-{}", n % 50),
                status,
                NOW - (n as i64) * 60_000,
                1 + (n % 4) as u64,
            )
        })
        .collect()
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

/// One key and the drawing that follows it.
fn step(
    model: &mut Model,
    terminal: &mut Terminal<TestBackend>,
    context: &Context,
    event: KeyEvent,
) -> Duration {
    let start = Instant::now();
    model.handle_key(event);
    terminal
        .draw(|frame| draw(frame, model, context))
        .expect("the test screen takes any drawing");
    start.elapsed()
}

fn main() {
    let look = Look {
        terminal: true,
        color: true,
        unicode: true,
        motion: omsh_config::Motion::Off,
    };
    let context = Context {
        styles: Styles::new(&Theme::default(), look),
        window: COMMANDS,
        unavailable: false,
        now_ms: NOW,
    };
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("a test screen");

    // Opening: the items from the rows, and the first search with no query.
    let mut opening = Vec::new();
    for _ in 0..ROUNDS {
        let start = Instant::now();
        let model = Model::new(items(), "/home/user/project-7".to_owned(), "");
        opening.push(start.elapsed());
        std::hint::black_box(model.matches());
    }
    opening.sort_unstable();

    let mut typing = Vec::new();
    let mut backspace = Vec::new();
    let mut scope = Vec::new();
    let mut moving = Vec::new();
    for _ in 0..ROUNDS {
        let mut model = Model::new(items(), "/home/user/project-7".to_owned(), "");
        terminal
            .draw(|frame| draw(frame, &mut model, &context))
            .expect("draw");
        for c in QUERY.chars() {
            typing.push(step(
                &mut model,
                &mut terminal,
                &context,
                key(KeyCode::Char(c)),
            ));
        }
        assert!(
            model.matches() > 0,
            "the query finds nothing: the benchmark measures nothing"
        );
        for _ in 0..3 {
            moving.push(step(&mut model, &mut terminal, &context, key(KeyCode::Up)));
        }
        for _ in 0..3 {
            backspace.push(step(
                &mut model,
                &mut terminal,
                &context,
                key(KeyCode::Backspace),
            ));
        }
        for _ in 0..3 {
            scope.push(step(&mut model, &mut terminal, &context, ctrl('r')));
        }
    }

    println!("{:>10} {:>10} {:>10}  case", "p50", "p99", "max");
    let mut worst = Duration::ZERO;
    for (name, mut times) in [
        ("typing a 12-letter query (narrowing)", typing),
        ("Backspace (a full search)", backspace),
        ("Ctrl+R, a new scope (a full search)", scope),
        ("moving the selection", moving),
    ] {
        times.sort_unstable();
        let p99 = percentile(&times, 0.99);
        worst = worst.max(p99);
        println!(
            "{:>10.3?} {p99:>10.3?} {:>10.3?}  {name} ({} keys)",
            percentile(&times, 0.5),
            times[times.len() - 1],
            times.len()
        );
    }
    let open99 = percentile(&opening, 0.99);
    println!(
        "{:>10.3?} {open99:>10.3?} {:>10.3?}  opening: {COMMANDS} items and the model (budget {OPEN_BUDGET:?})",
        percentile(&opening, 0.5),
        opening[opening.len() - 1]
    );
    println!("worst p99 of a key: {worst:.3?} (budget {KEY_BUDGET:?})");
    if worst > KEY_BUDGET || open99 > OPEN_BUDGET {
        eprintln!("explorer benchmark: over the budget");
        std::process::exit(1);
    }
}
