//! How long the command palette (Ctrl+P) and the settings view take to answer a key (blueprint
//! section 16: a key must be answered in under 5 ms at the 99th percentile).
//!
//! The palette lists the shell's own names. A real shell has some dozens; the benchmark uses 2,000
//! aliases and functions, many times more than a person defines. Every case is timed from the key
//! to the end of the drawing, the drawing going to an 80 x 24 test screen:
//!
//! - typing a 10-letter query one letter at a time (each key narrows the matches of the key before),
//! - Backspace, which looks at every entry again,
//! - moving the selection,
//! - and, for the settings view, typing a name and changing a value (a fake store; the real write
//!   is a small file and is not what a key costs).
//!
//! Opening (making the entries and the model) is timed and held to 20 ms.
//!
//! A plain program with no benchmark framework: it exits with status 1 when a budget is missed,
//! which is how CI gates it (`scripts/perf-gate.sh`). Run with
//! `cargo bench -p omsh-ui --bench palette`.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use omsh_config::{Change, Config};
use omsh_ui::palette::{Effect, Entry, Kind, Model, render};
use omsh_ui::settings::{self, Store};
use omsh_ui::view::draw::Styles;
use omsh_ui::{Look, Theme};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

const NAMES: usize = 2_000;
const ROUNDS: usize = 40;
const KEY_BUDGET: Duration = Duration::from_millis(5);
const OPEN_BUDGET: Duration = Duration::from_millis(20);

const QUERY: &str = "git status";

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    let index = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[index]
}

fn entries() -> Vec<Entry> {
    let mut out = vec![Entry::new(
        Kind::Action,
        "Settings",
        "Show the settings and change switches.",
        None,
        Effect::OpenSettings,
    )];
    for n in 0..NAMES {
        let kind = match n % 3 {
            0 => Kind::Alias,
            1 => Kind::Function,
            _ => Kind::Builtin,
        };
        out.push(Entry::new(
            kind,
            format!("name-{n}-{}", ["gs", "gco", "dps", "k8s", "tf"][n % 5]),
            format!("git status --short --branch number {n} of the list"),
            None,
            Effect::Insert(format!("name-{n} ")),
        ));
    }
    out
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// A store that keeps the file's text in memory.
struct Memory(String);

impl Store for Memory {
    fn save(&mut self, key: &str, change: &Change) -> Result<(), String> {
        let value = match change {
            Change::Bool(on) => on.to_string(),
            Change::Text(word) => format!("\"{word}\""),
            Change::Remove => return Ok(()),
        };
        self.0 = format!("{key} = {value}\n");
        Ok(())
    }

    fn text(&self) -> Option<String> {
        Some(self.0.clone())
    }
}

fn main() {
    let styles = Styles::new(
        &Theme::default(),
        Look {
            terminal: true,
            color: true,
            unicode: true,
            motion: omsh_config::Motion::Off,
        },
    );
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("a test screen");

    let mut opening = Vec::new();
    for _ in 0..ROUNDS {
        let start = Instant::now();
        let model = Model::new(entries());
        opening.push(start.elapsed());
        std::hint::black_box(model.matches());
    }
    opening.sort_unstable();

    let mut typing = Vec::new();
    let mut backspace = Vec::new();
    let mut moving = Vec::new();
    for _ in 0..ROUNDS {
        let mut model = Model::new(entries());
        terminal
            .draw(|frame| render::draw(frame, &mut model, &styles))
            .expect("draw");
        for c in QUERY.chars() {
            let start = Instant::now();
            model.handle_key(key(KeyCode::Char(c)));
            terminal
                .draw(|frame| render::draw(frame, &mut model, &styles))
                .expect("draw");
            typing.push(start.elapsed());
        }
        assert!(
            model.matches() > 0,
            "the query finds nothing: the benchmark measures nothing"
        );
        for code in [KeyCode::Down, KeyCode::Down, KeyCode::Up] {
            let start = Instant::now();
            model.handle_key(key(code));
            terminal
                .draw(|frame| render::draw(frame, &mut model, &styles))
                .expect("draw");
            moving.push(start.elapsed());
        }
        for _ in 0..3 {
            let start = Instant::now();
            model.handle_key(key(KeyCode::Backspace));
            terminal
                .draw(|frame| render::draw(frame, &mut model, &styles))
                .expect("draw");
            backspace.push(start.elapsed());
        }
    }

    // The settings view: type a name, then change the value of the chosen row.
    let mut settings_keys = Vec::new();
    for _ in 0..ROUNDS {
        let mut model =
            settings::Model::new(Config::default(), Some(Box::new(Memory(String::new()))));
        for c in "hints".chars() {
            let start = Instant::now();
            model.handle_key(key(KeyCode::Char(c)));
            terminal
                .draw(|frame| settings::render::draw(frame, &mut model, &styles))
                .expect("draw");
            settings_keys.push(start.elapsed());
        }
        for _ in 0..3 {
            let start = Instant::now();
            model.handle_key(key(KeyCode::Enter));
            terminal
                .draw(|frame| settings::render::draw(frame, &mut model, &styles))
                .expect("draw");
            settings_keys.push(start.elapsed());
        }
    }

    println!("{:>10} {:>10} {:>10}  case", "p50", "p99", "max");
    let mut worst = Duration::ZERO;
    for (name, mut times) in [
        ("palette: typing a 10-letter query (narrowing)", typing),
        ("palette: Backspace (a full search)", backspace),
        ("palette: moving the selection", moving),
        ("settings: typing a name, changing a value", settings_keys),
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
        "{:>10.3?} {open99:>10.3?} {:>10.3?}  opening: {NAMES} entries and the model (budget {OPEN_BUDGET:?})",
        percentile(&opening, 0.5),
        opening[opening.len() - 1]
    );
    println!("worst p99 of a key: {worst:.3?} (budget {KEY_BUDGET:?})");
    if worst > KEY_BUDGET || open99 > OPEN_BUDGET {
        eprintln!("palette benchmark: over the budget");
        std::process::exit(1);
    }
}
