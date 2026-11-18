//! The panels of a plan that is running (UI upgrade, phase 6).
//!
//! - [`Progress`]: the steps of the plan, one row each with its state (`✓` done, `✗` failed,
//!   `?` asks first, `◼` paused, `–` skipped or not run, `○` waiting), a progress bar and the
//!   time since the plan began. The step-by-step question ([`crate::panels::step::StepPanel`])
//!   shows it above its buttons, and [`summary`] shows it when the plan has ended.
//! - [`PausePanel`]: the question after Ctrl+Z stopped a step: **Continue** / **Skip** /
//!   **Stop plan** / **To background**, with **Stop plan** chosen. A stopped step is neither done
//!   nor failed, so the plan waits for the person.
//!
//! There is no row for a step that is running, and no spinner: while a step runs it owns the
//! terminal and the shell is waiting for it, so nothing can be drawn or read until it ends or
//! is stopped. The panels are drawn only where the shell has the keyboard.
//!
//! Step titles come from the model, so every panel here is open on the right.

use std::time::Duration;

use crate::boxes::{Panel, open};
use crate::caps::Caps;
use crate::panels::MAX_WIDTH;
use crate::text::{Line, Style, Tone};
use crate::widgets::buttons::Buttons;
use crate::widgets::{Key, Step, Widget};

/// Where one step of the plan is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowState {
    /// It ran and worked.
    Done,
    /// It ran and failed (the plan stopped here).
    Failed,
    /// The question about it is on screen.
    Ask,
    /// It was stopped with Ctrl+Z and waits for an answer, or the plan ended with it stopped.
    Paused,
    /// It was sent to the background and goes on there; the plan did not wait for it.
    Background,
    /// The person skipped it.
    Skipped,
    /// The plan ended before it.
    NotRun,
    /// Its turn has not come.
    Pending,
}

impl RowState {
    /// The mark in front of the row, and its word after the title (the word is there so that
    /// the color is never the only sign).
    fn mark(self, unicode: bool) -> &'static str {
        match (self, unicode) {
            (Self::Done, true) => "✓",
            (Self::Done, false) => "+",
            (Self::Failed, true) => "✗",
            (Self::Failed, false) => "x",
            (Self::Ask, _) => "?",
            (Self::Paused, true) => "◼",
            (Self::Paused, false) => "#",
            (Self::Background, _) => "&",
            (Self::Skipped | Self::NotRun, true) => "–",
            (Self::Skipped | Self::NotRun, false) => "-",
            (Self::Pending, true) => "○",
            (Self::Pending, false) => "o",
        }
    }

    fn word(self) -> &'static str {
        match self {
            Self::Done | Self::Pending => "",
            Self::Failed => "failed",
            Self::Ask => "asks first",
            Self::Paused => "paused",
            Self::Background => "in the background",
            Self::Skipped => "skipped",
            Self::NotRun => "not run",
        }
    }

    fn tone(self) -> Tone {
        match self {
            Self::Done => Tone::Ok,
            Self::Failed => Tone::Danger,
            Self::Ask => Tone::Key,
            Self::Paused => Tone::Warn,
            Self::Background => Tone::Info,
            Self::Skipped | Self::NotRun | Self::Pending => Tone::Dim,
        }
    }
}

/// One step of the plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunRow {
    pub title: String,
    pub state: RowState,
}

/// The steps and how long the plan has been going.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Progress {
    rows: Vec<RunRow>,
    elapsed: Duration,
}

/// Cells of the progress bar.
const CELLS: usize = 10;

/// `m:ss`, or `h:mm:ss` from an hour on.
#[must_use]
pub fn clock(elapsed: Duration) -> String {
    let total = elapsed.as_secs();
    let (hours, minutes, seconds) = (total / 3600, total / 60 % 60, total % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

impl Progress {
    #[must_use]
    pub fn new(rows: Vec<RunRow>, elapsed: Duration) -> Self {
        Self { rows, elapsed }
    }

    fn count(&self, state: RowState) -> usize {
        self.rows.iter().filter(|row| row.state == state).count()
    }

    /// The color of a panel that ends with this progress: red if a step failed, green if every
    /// step ran or was skipped, blue otherwise.
    #[must_use]
    pub fn tone(&self) -> Tone {
        if self.count(RowState::Failed) > 0 {
            Tone::Danger
        } else if self
            .rows
            .iter()
            .all(|row| matches!(row.state, RowState::Done | RowState::Skipped))
        {
            Tone::Ok
        } else {
            Tone::Info
        }
    }

    /// The bar, the counts and the time, then one row for each step.
    #[must_use]
    pub fn lines(&self, caps: &Caps) -> Vec<Line> {
        let total = self.rows.len();
        let done = self.count(RowState::Done);
        let skipped = self.count(RowState::Skipped);
        let (full, empty) = if caps.unicode {
            ("█", "░")
        } else {
            ("#", "-")
        };
        // Only a step that really ran counts; a plan of one step is not "10% done" at the start.
        let cells = (done * CELLS).checked_div(total).unwrap_or(0);
        let tone = self.tone();
        let mut counts = format!("{done} of {total} done");
        if skipped > 0 {
            counts.push_str(&format!(", {skipped} skipped"));
        }
        let mut out = vec![
            Line::new()
                .then(&full.repeat(cells), Style::of(tone))
                .then(&empty.repeat(CELLS - cells), Style::of(Tone::Dim))
                .then("  ", Style::PLAIN)
                .then(&counts, Style::of(tone))
                .then(&format!("  {}", clock(self.elapsed)), Style::of(Tone::Dim)),
        ];
        for (index, row) in self.rows.iter().enumerate() {
            let mut line = Line::new()
                .then(row.state.mark(caps.unicode), Style::of(row.state.tone()))
                .then(&format!(" {}. ", index + 1), Style::of(row.state.tone()))
                .then(&row.title, Style::of(row_text_tone(row.state)));
            let word = row.state.word();
            if !word.is_empty() {
                line.push(&format!("  ({word})"), Style::of(row.state.tone()));
            }
            out.push(line);
        }
        out
    }
}

/// The title of a step that is over or not yet reached is dim, so the step that matters stands
/// out.
fn row_text_tone(state: RowState) -> Tone {
    match state {
        RowState::Ask | RowState::Paused | RowState::Background | RowState::Failed => Tone::Plain,
        RowState::Done | RowState::Skipped | RowState::NotRun | RowState::Pending => Tone::Dim,
    }
}

/// The panel shown when the plan has ended: every step with its state. It keeps no buttons.
#[must_use]
pub fn summary(progress: &Progress, caps: &Caps, width: u16) -> Vec<Line> {
    let panel = Panel::new("Plan")
        .tone(progress.tone())
        .lines(progress.lines(caps));
    open(&panel, caps, width.min(MAX_WIDTH))
}

/// What the person chose for the stopped step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PauseChoice {
    /// Bring it back to the foreground (`fg`).
    Continue,
    /// End it and go on with the next step.
    Skip,
    /// Leave it stopped, in the job table; the rest of the plan does not run.
    Stop,
    /// Let it run on without the terminal (`bg`); the rest of the plan does not run.
    Background,
}

fn choice_of(index: usize) -> PauseChoice {
    match index {
        0 => PauseChoice::Continue,
        1 => PauseChoice::Skip,
        2 => PauseChoice::Stop,
        _ => PauseChoice::Background,
    }
}

impl PauseChoice {
    /// What the chosen button does, in a few words.
    fn explain(self) -> &'static str {
        match self {
            Self::Continue => "run it in the foreground again",
            Self::Skip => "end this step and go on with the next one",
            Self::Stop => "leave it stopped (see jobs); the rest of the plan does not run",
            Self::Background => "let it run on; the rest of the plan does not run",
        }
    }
}

/// The question after Ctrl+Z stopped step `number` of `total`.
pub struct PausePanel {
    number: usize,
    total: usize,
    body: Vec<Line>,
    progress: Progress,
    buttons: Buttons,
}

impl PausePanel {
    /// `body` is the command of the step, already wrapped. `progress` has this step as
    /// [`RowState::Paused`].
    #[must_use]
    pub fn new(number: usize, total: usize, body: Vec<Line>, progress: Progress) -> Self {
        Self {
            number,
            total,
            body,
            progress,
            // Stop plan is chosen: the step stays stopped, as it does without a panel.
            buttons: Buttons::new(
                &[
                    ("Continue", 'c'),
                    ("Skip", 's'),
                    ("Stop plan", 'n'),
                    ("To background", 'b'),
                ],
                2,
            ),
        }
    }

    /// The button that is chosen now.
    #[must_use]
    pub fn chosen(&self) -> PauseChoice {
        choice_of(self.buttons.chosen())
    }
}

impl Widget for PausePanel {
    type Output = PauseChoice;

    fn handle(&mut self, key: Key) -> Step<PauseChoice> {
        match self.buttons.handle(key) {
            Step::Stay => Step::Stay,
            Step::Done(index) => Step::Done(choice_of(index)),
            Step::Cancel => Step::Done(PauseChoice::Stop),
        }
    }

    fn render(&self, caps: &Caps, width: u16) -> Vec<Line> {
        let width = width.min(MAX_WIDTH);
        let arrows = if caps.unicode { "←→" } else { "arrows" };
        let mut panel = Panel::new(&format!("Paused: step {} of {}", self.number, self.total))
            .tone(Tone::Warn)
            .hint(arrows, "move")
            .hint("Enter", "choose")
            .hint("Esc", "stop plan");
        panel = panel.lines(self.progress.lines(caps));
        panel = panel.line(Line::new());
        panel = panel.lines(self.body.iter().cloned());
        panel = panel.line(Line::new());
        panel = panel.lines(self.buttons.render(caps, width.saturating_sub(2)));
        panel = panel.line(Line::new().tone(Tone::Dim, self.chosen().explain()));
        open(&panel, caps, width)
    }
}

#[cfg(test)]
mod tests;
