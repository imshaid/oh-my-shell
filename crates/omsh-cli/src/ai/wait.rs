//! The line shown while the model thinks (§15.2: only real numbers).
//!
//! On a terminal it is one line, `Asking <provider> (<model>)... 2.4 s`, redrawn in place with the
//! real elapsed time (and whatever extra real number the caller has, such as the characters that
//! have arrived). Anywhere else it is one plain line, printed once.

use std::cell::Cell;
use std::io::{self, IsTerminal};
use std::time::{Duration, Instant};

use omsh_exec::Terminal;

/// How often the line is redrawn.
const REDRAW: Duration = Duration::from_millis(150);

pub struct WaitLine {
    label: String,
    live: bool,
    started: Instant,
    last_draw: Cell<Option<Instant>>,
}

impl WaitLine {
    /// Starts waiting. Without a terminal this prints the plain line now.
    pub fn start(label: &str, terminal: &mut dyn Terminal) -> Self {
        let live = io::stderr().is_terminal();
        if !live {
            terminal.err(&format!("{label}...\n"));
        }
        Self {
            label: label.to_owned(),
            live,
            started: Instant::now(),
            last_draw: Cell::new(None),
        }
    }

    /// Redraws the line if it is due. `extra` is appended as it is (for example `, 310
    /// characters`).
    pub fn tick(&self, terminal: &mut dyn Terminal, extra: &str) {
        if self.live && self.last_draw.get().is_none_or(|at| at.elapsed() >= REDRAW) {
            terminal.err(&format!(
                "\r\x1b[K{}... {:.1} s{extra}",
                self.label,
                self.started.elapsed().as_secs_f64()
            ));
            self.last_draw.set(Some(Instant::now()));
        }
    }

    /// Erases the line, if one was drawn.
    pub fn clear(&self, terminal: &mut dyn Terminal) {
        if self.last_draw.take().is_some() {
            terminal.err("\r\x1b[K");
        }
    }
}
