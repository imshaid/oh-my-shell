//! The jobs table (UI upgrade, phase 6): `jobs -i` and the palette's "Jobs" entry.
//!
//! One row for each job: `%id` with its mark, the state with its real word (`● Running`,
//! `◼ Stopped`, `✓ Done`, `✘ Terminated`, `✘ Exit 3`), the time since the job was made and the
//! command. Below the rows, four buttons act on the chosen job: **Foreground**, **Background**
//! (a stopped job), **Stop** (a running job) and **Kill**. A button that does not fit the job's
//! state is shown dim and does nothing. **Kill** asks first, with **Cancel** chosen.
//!
//! The time comes before the command so that a long or Bangla command is cut at the end of the
//! row and never takes the time with it (the panel has no right edge, see [`crate::boxes`]).
//!
//! The panel does not know the shell. It is given a function that reads the rows, and calls it
//! when a key is pressed and once a second while a frame is due, so the times and states follow
//! the real table. With motion off no frame is due on its own: the rows are read again at the
//! next key.

use std::time::Duration;

use crate::boxes::{Panel, open};
use crate::caps::Caps;
use crate::panels::MAX_WIDTH;
use crate::panels::plan_run::clock;
use crate::text::{Line, Style, Tone, width as columns};
use crate::widgets::buttons::Buttons;
use crate::widgets::{Key, Step, Widget};

/// How a job is, for the mark and the color. The words are the shell's own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    Running,
    Stopped,
    /// Over with status 0.
    Done,
    /// Over with a nonzero status or by a signal.
    Failed,
}

impl RowKind {
    const fn is_over(self) -> bool {
        matches!(self, Self::Done | Self::Failed)
    }

    fn mark(self, unicode: bool) -> &'static str {
        match (self, unicode) {
            (Self::Running, true) => "●",
            (Self::Running, false) => "*",
            (Self::Stopped, true) => "◼",
            (Self::Stopped, false) => "#",
            (Self::Done, true) => "✓",
            (Self::Done, false) => "+",
            (Self::Failed, true) => "✘",
            (Self::Failed, false) => "x",
        }
    }

    fn tone(self) -> Tone {
        match self {
            Self::Running => Tone::Info,
            Self::Stopped => Tone::Warn,
            Self::Done => Tone::Ok,
            Self::Failed => Tone::Danger,
        }
    }
}

/// One job.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableRow {
    pub number: u32,
    /// `+`, `-` or a space.
    pub mark: char,
    pub kind: RowKind,
    /// The state in the shell's words (`Running`, `Stopped (tty input)`, `Terminated`).
    pub word: String,
    pub text: String,
    /// Since the job was made, or until it ended.
    pub elapsed: Duration,
}

/// What the person asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableAction {
    Foreground,
    Background,
    Stop,
    Kill,
}

/// The four buttons, in the order they are drawn.
const ACTIONS: [(&str, char, TableAction); 4] = [
    ("Foreground", 'f', TableAction::Foreground),
    ("Background", 'b', TableAction::Background),
    ("Stop", 's', TableAction::Stop),
    ("Kill", 'k', TableAction::Kill),
];

/// Whether `action` makes sense for a job in this state.
#[must_use]
pub const fn allowed(kind: RowKind, action: TableAction) -> bool {
    match action {
        TableAction::Foreground | TableAction::Kill => !kind.is_over(),
        TableAction::Background => matches!(kind, RowKind::Stopped),
        TableAction::Stop => matches!(kind, RowKind::Running),
    }
}

/// Rows of the table shown at once.
const ROWS: usize = 8;

/// How often the rows are read again while frames are being drawn.
const REFRESH: Duration = Duration::from_secs(1);

enum Stage {
    Table,
    /// Asking whether to kill this job.
    Kill(u32),
}

/// The table. Its answer is the job number and the action.
pub struct JobsTable {
    source: Box<dyn Fn() -> Vec<TableRow>>,
    rows: Vec<TableRow>,
    /// The index of the chosen row.
    cursor: usize,
    /// The number of the chosen job, so that the choice follows the job when the rows change.
    chosen_number: Option<u32>,
    /// The first row shown.
    top: usize,
    /// The index in [`ACTIONS`] that is chosen.
    button: usize,
    stage: Stage,
    question: Buttons,
    /// When the rows were read, counted from the opening of the panel.
    read_at: Duration,
}

fn kill_question() -> Buttons {
    Buttons::new(&[("Kill", 'k'), ("Cancel", 'n')], 1)
}

impl JobsTable {
    /// A table that reads its rows with `source`.
    #[must_use]
    pub fn new(source: Box<dyn Fn() -> Vec<TableRow>>) -> Self {
        let rows = source();
        let mut table = Self {
            source,
            chosen_number: rows.first().map(|row| row.number),
            rows,
            cursor: 0,
            top: 0,
            button: 0,
            stage: Stage::Table,
            question: kill_question(),
            read_at: Duration::ZERO,
        };
        table.settle_button();
        table
    }

    /// The chosen row.
    #[must_use]
    pub fn chosen(&self) -> Option<&TableRow> {
        self.rows.get(self.cursor)
    }

    /// True while the question about killing a job is on screen.
    #[must_use]
    pub fn asking(&self) -> bool {
        matches!(self.stage, Stage::Kill(_))
    }

    fn refresh(&mut self) {
        self.rows = (self.source)();
        self.cursor = match self
            .chosen_number
            .and_then(|number| self.rows.iter().position(|row| row.number == number))
        {
            Some(index) => index,
            None => self.cursor.min(self.rows.len().saturating_sub(1)),
        };
        self.chosen_number = self.chosen().map(|row| row.number);
        self.keep_in_the_window();
        self.settle_button();
        if let Stage::Kill(number) = self.stage
            && !self.rows.iter().any(|row| row.number == number)
        {
            self.stage = Stage::Table;
        }
    }

    fn keep_in_the_window(&mut self) {
        if self.cursor < self.top {
            self.top = self.cursor;
        } else if self.cursor >= self.top + ROWS {
            self.top = self.cursor + 1 - ROWS;
        }
        self.top = self.top.min(self.rows.len().saturating_sub(ROWS));
    }

    fn allowed_now(&self, index: usize) -> bool {
        self.chosen()
            .is_some_and(|row| allowed(row.kind, ACTIONS[index].2))
    }

    /// Moves the chosen button to one that fits the chosen job, if the one it was on does not.
    fn settle_button(&mut self) {
        if self.allowed_now(self.button) {
            return;
        }
        self.button = (0..ACTIONS.len())
            .find(|index| self.allowed_now(*index))
            .unwrap_or(0);
    }

    fn move_button(&mut self, forward: bool) {
        let count = ACTIONS.len();
        for step in 1..=count {
            let index = if forward {
                (self.button + step) % count
            } else {
                (self.button + count - step % count) % count
            };
            if self.allowed_now(index) {
                self.button = index;
                return;
            }
        }
    }

    fn move_row(&mut self, down: bool) {
        if down {
            if self.cursor + 1 < self.rows.len() {
                self.cursor += 1;
            }
        } else {
            self.cursor = self.cursor.saturating_sub(1);
        }
        self.chosen_number = self.chosen().map(|row| row.number);
        self.button = 0;
        self.keep_in_the_window();
        self.settle_button();
    }

    /// The answer for `action` on the chosen job, if it fits. Kill goes to its question.
    fn act(&mut self, action: TableAction) -> Step<(u32, TableAction)> {
        let Some(row) = self.chosen() else {
            return Step::Stay;
        };
        if !allowed(row.kind, action) {
            return Step::Stay;
        }
        let number = row.number;
        if action == TableAction::Kill {
            self.stage = Stage::Kill(number);
            self.question = kill_question();
            return Step::Stay;
        }
        Step::Done((number, action))
    }

    fn button_row(&self) -> Line {
        let mut line = Line::new();
        for (index, (label, shortcut, _)) in ACTIONS.iter().enumerate() {
            if index > 0 {
                line.push("  ", Style::PLAIN);
            }
            let fits = self.allowed_now(index);
            let chosen = fits && index == self.button;
            let base = if chosen {
                Style::PLAIN.selected()
            } else if fits {
                Style::of(Tone::Plain)
            } else {
                Style::of(Tone::Dim)
            };
            line.push(if chosen { " " } else { "[" }, base);
            let mut underlined = false;
            for c in label.chars() {
                let mut one = [0u8; 4];
                let text: &str = c.encode_utf8(&mut one);
                if fits && !underlined && c.to_lowercase().eq(shortcut.to_lowercase()) {
                    underlined = true;
                    line.push(text, base.underlined());
                } else {
                    line.push(text, base);
                }
            }
            line.push(if chosen { " " } else { "]" }, base);
        }
        line
    }

    fn row_line(&self, row: &TableRow, chosen: bool, word_cols: usize, caps: &Caps) -> Line {
        let marker = match (chosen, caps.unicode) {
            (true, true) => "❯ ",
            (true, false) => "> ",
            (false, _) => "  ",
        };
        let mut line = if chosen {
            Line::new().then(marker, Style::of(Tone::Key))
        } else {
            Line::plain(marker)
        };
        let body = if chosen {
            Style::PLAIN.selected()
        } else {
            Style::PLAIN
        };
        let state = if chosen {
            Style::of(row.kind.tone()).selected()
        } else {
            Style::of(row.kind.tone())
        };
        let faint = if chosen {
            Style::PLAIN.selected()
        } else {
            Style::of(Tone::Dim)
        };
        let id = format!("%{}{}", row.number, row.mark);
        line.push(&format!("{id:<5}"), body);
        line.push(row.kind.mark(caps.unicode), state);
        line.push(" ", body);
        let pad = word_cols.saturating_sub(columns(&row.word));
        line.push(&row.word, state);
        line.push(&" ".repeat(pad + 2), body);
        line.push(&format!("{:>5}", clock(row.elapsed)), faint);
        line.push("  ", body);
        line.push(&row.text, body);
        line
    }
}

impl Widget for JobsTable {
    type Output = (u32, TableAction);

    fn handle(&mut self, key: Key) -> Step<(u32, TableAction)> {
        self.refresh();
        if let Stage::Kill(number) = self.stage {
            return match self.question.handle(key) {
                Step::Done(0) => Step::Done((number, TableAction::Kill)),
                Step::Done(_) | Step::Cancel => {
                    self.stage = Stage::Table;
                    Step::Stay
                }
                Step::Stay => Step::Stay,
            };
        }
        match key {
            Key::Up => self.move_row(false),
            Key::Down => self.move_row(true),
            Key::Left | Key::BackTab => self.move_button(false),
            Key::Right | Key::Tab => self.move_button(true),
            Key::Enter => {
                if self.allowed_now(self.button) {
                    return self.act(ACTIONS[self.button].2);
                }
            }
            Key::Esc => return Step::Cancel,
            Key::Char(c) => {
                if let Some((_, _, action)) = ACTIONS
                    .iter()
                    .find(|(_, shortcut, _)| shortcut.to_lowercase().eq(c.to_lowercase()))
                {
                    return self.act(*action);
                }
            }
            Key::Backspace => {}
        }
        Step::Stay
    }

    fn render(&self, caps: &Caps, width: u16) -> Vec<Line> {
        let width = width.min(MAX_WIDTH);
        let inner = width.saturating_sub(2);
        let (vertical, arrows) = if caps.unicode {
            ("↑↓", "←→")
        } else {
            ("Up/Down", "Left/Right")
        };

        if let Stage::Kill(number) = self.stage {
            let mut panel = Panel::new("Kill this job?")
                .tone(Tone::Warn)
                .hint(arrows, "move")
                .hint("Enter", "choose")
                .hint("k", "kill")
                .hint("n", "cancel");
            if let Some(row) = self.rows.iter().find(|row| row.number == number) {
                panel =
                    panel.line(Line::new().tone(Tone::Plain, &format!("%{number}  {}", row.text)));
            }
            panel = panel
                .line(Line::new().tone(Tone::Dim, &format!("Sends SIGTERM, as `kill %{number}`.")));
            panel = panel.line(Line::new());
            panel = panel.lines(self.question.render(caps, inner));
            return open(&panel, caps, width);
        }

        let mut panel = Panel::new("Jobs")
            .hint(vertical, "job")
            .hint(arrows, "button")
            .hint("Enter", "do it")
            .hint("Esc", "close");
        if self.rows.is_empty() {
            panel = panel.line(Line::new().tone(Tone::Dim, "No jobs"));
            return open(&panel, caps, width);
        }
        let word_cols = self
            .rows
            .iter()
            .map(|row| columns(&row.word))
            .max()
            .unwrap_or(0);
        let end = (self.top + ROWS).min(self.rows.len());
        if self.top > 0 {
            let arrow = if caps.unicode { "↑" } else { "^" };
            panel = panel.line(Line::new().tone(Tone::Dim, &format!("{arrow} {} more", self.top)));
        }
        for index in self.top..end {
            panel =
                panel.line(self.row_line(&self.rows[index], index == self.cursor, word_cols, caps));
        }
        if end < self.rows.len() {
            let arrow = if caps.unicode { "↓" } else { "v" };
            panel = panel.line(Line::new().tone(
                Tone::Dim,
                &format!("{arrow} {} more", self.rows.len() - end),
            ));
        }
        panel = panel.line(Line::new());
        panel = panel.line(self.button_row());
        open(&panel, caps, width)
    }

    fn animating(&self) -> bool {
        self.rows.iter().any(|row| !row.kind.is_over())
    }

    fn tick(&mut self, since_open: Duration) {
        if since_open >= self.read_at + REFRESH {
            self.read_at = since_open;
            self.refresh();
        }
    }
}

#[cfg(test)]
mod tests;
