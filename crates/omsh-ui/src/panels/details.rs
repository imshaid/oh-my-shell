//! The AI call details panel (spec section 6): one table for the last call of a provider, with a
//! tab for each provider used in this session (`←→` switch, `Esc`, `q` or `Enter` close).
//!
//! A **full box**: every word in it is ours or a model name that is made harmless and cut to fit.
//! Rows: Provider, Model, Effort, Context, Input, Thinking, Output, Cached, Total, Time, Speed.
//! A number the provider did not report is a dash. Speed is labeled `(derived)`. A note under the
//! table says where the context limit came from, or that it is not known.
//!
//! With full motion the rows appear one by one and the numbers count up; with reduced motion or
//! none the table is complete at once. Keys are handled at once during the motion.

use std::cell::Cell;
use std::time::Duration;

use omsh_config::Motion;

use crate::boxes::{Panel, full};
use crate::caps::Caps;
use crate::motion::{TWEEN, Tween};
use crate::panels::MAX_WIDTH;
use crate::panels::facts::{CallFacts, Marks, seconds};
use crate::panels::meter::Meter;
use crate::text::{Line, Style, Tone};
use crate::widgets::{Key, Step, Widget};

/// How long after the previous row the next one appears.
const ROW_STEP: Duration = Duration::from_millis(50);
/// The width of the label column.
const LABELS: usize = 10;
/// The widest the table is, so that it stays a table on a wide window.
const WIDTH: u16 = 72;

/// The panel.
pub struct Details {
    tabs: Vec<CallFacts>,
    selected: usize,
    since_open: Duration,
    /// When the table on screen started to appear (the last tab change).
    started: Duration,
    motion: Cell<Motion>,
}

impl Details {
    /// A panel for these calls (one for each provider used), `selected` shown first.
    #[must_use]
    pub fn new(tabs: Vec<CallFacts>, selected: usize) -> Self {
        let selected = selected.min(tabs.len().saturating_sub(1));
        Self {
            tabs,
            selected,
            since_open: Duration::ZERO,
            started: Duration::ZERO,
            motion: Cell::new(Motion::Off),
        }
    }

    /// The tab that is shown (counting from 0).
    #[must_use]
    pub const fn selected(&self) -> usize {
        self.selected
    }

    fn switch(&mut self, forward: bool) {
        let count = self.tabs.len();
        if count < 2 {
            return;
        }
        self.selected = if forward {
            (self.selected + 1) % count
        } else {
            (self.selected + count - 1) % count
        };
        self.started = self.since_open;
    }

    fn elapsed(&self) -> Duration {
        self.since_open.saturating_sub(self.started)
    }

    /// The rows of one call, as `(label, value)`. `count_up` turns a number into the text shown
    /// `appeared` after its row did.
    fn rows(&self, facts: &CallFacts, caps: &Caps) -> Vec<(&'static str, Line)> {
        let marks = Marks::of(caps);
        let dim = |text: &str| Line::new().then(text, Style::of(Tone::Muted));
        let plain = |text: &str| Line::plain(text);
        let motion = caps.motion;
        let elapsed = self.elapsed();
        let number = |index: usize, value: Option<u32>| -> Line {
            let Some(value) = value else {
                return dim(marks.dash);
            };
            let appeared = elapsed.saturating_sub(ROW_STEP * u32::try_from(index).unwrap_or(0));
            let shown = Tween::new(0.0, f64::from(value), TWEEN).value(appeared, motion);
            plain(&crate::panels::facts::group(shown.round() as u32))
        };
        let meter = Meter::new(facts.context_used, facts.context_limit, None);
        let context_at = ROW_STEP * 3;
        vec![
            ("Provider", plain(&facts.provider)),
            ("Model", plain(&facts.model)),
            (
                "Effort",
                facts
                    .effort
                    .as_deref()
                    .map_or_else(|| dim(marks.dash), plain),
            ),
            (
                "Context",
                meter.line(caps, elapsed.saturating_sub(context_at)),
            ),
            ("Input", number(4, facts.input)),
            ("Thinking", number(5, facts.thinking)),
            ("Output", number(6, facts.output)),
            ("Cached", number(7, facts.cached)),
            ("Total", number(8, facts.total)),
            (
                "Time",
                facts
                    .time
                    .map_or_else(|| dim(marks.dash), |t| plain(&seconds(t))),
            ),
            (
                "Speed",
                facts.tokens_per_sec.map_or_else(
                    || dim(marks.dash),
                    |speed| plain(&format!("{speed:.1} tok/s (derived)")),
                ),
            ),
        ]
    }

    /// How long the whole table takes to appear and settle.
    fn animation(rows: usize) -> Duration {
        ROW_STEP * u32::try_from(rows).unwrap_or(0) + TWEEN
    }
}

impl Widget for Details {
    type Output = ();

    fn handle(&mut self, key: Key) -> Step<()> {
        match key {
            Key::Left | Key::BackTab => {
                self.switch(false);
                Step::Stay
            }
            Key::Right | Key::Tab => {
                self.switch(true);
                Step::Stay
            }
            Key::Enter | Key::Char('q' | 'Q') => Step::Done(()),
            Key::Esc => Step::Cancel,
            _ => Step::Stay,
        }
    }

    fn animating(&self) -> bool {
        self.motion.get() == Motion::Full && self.elapsed() < Self::animation(11)
    }

    fn tick(&mut self, since_open: Duration) {
        self.since_open = since_open;
    }

    fn render(&self, caps: &Caps, width: u16) -> Vec<Line> {
        self.motion.set(caps.motion);
        let width = width.min(MAX_WIDTH).min(WIDTH);
        let arrows = if caps.unicode { "←→" } else { "arrows" };
        let mut panel = Panel::new("AI call details").tone(Tone::Ai);
        if self.tabs.len() > 1 {
            panel = panel.hint(arrows, "provider");
        }
        panel = panel.hint("Esc", "close");
        let Some(facts) = self.tabs.get(self.selected) else {
            return full(&panel.line(Line::plain("no AI call yet")), caps, width);
        };
        if self.tabs.len() > 1 {
            let mut tabs = Line::new();
            for (index, tab) in self.tabs.iter().enumerate() {
                let style = if index == self.selected {
                    Style::of(Tone::Ai).selected()
                } else {
                    Style::of(Tone::Muted)
                };
                tabs.push(&format!(" {} ", tab.provider), style);
                tabs.push(" ", Style::PLAIN);
            }
            panel = panel.line(tabs).line(Line::new());
        }
        let motion = caps.motion;
        let elapsed = self.elapsed();
        for (index, (label, value)) in self.rows(facts, caps).into_iter().enumerate() {
            // A row that has not appeared yet is a blank line, so that the box does not grow.
            let appears = ROW_STEP * u32::try_from(index).unwrap_or(0);
            if motion == Motion::Full && elapsed < appears {
                panel = panel.line(Line::new());
                continue;
            }
            let mut row = Line::new().tone(Tone::Muted, &format!("{label:<LABELS$}"));
            row.append(&value);
            panel = panel.line(row);
        }
        panel = panel.line(Line::new());
        let note = facts.limit_note.clone().unwrap_or_else(|| {
            if facts.context_limit.is_some() {
                String::new()
            } else {
                "context limit unknown: the provider does not say; set [ai] context_limit"
                    .to_owned()
            }
        });
        if !note.is_empty() {
            panel = panel.line(Line::new().then(&note, Style::of(Tone::Muted)));
        }
        panel = panel.line(Line::new().then(
            "ctx is approximate: it is the last call's total",
            Style::of(Tone::Muted),
        ));
        full(&panel, caps, width)
    }
}

#[cfg(test)]
mod tests;
