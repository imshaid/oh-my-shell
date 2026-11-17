//! The `undo` picker: a filterable list of what can be undone, one row for each action (one
//! typed line, which may have moved many files), with a preview of the highlighted action.
//!
//! **`Enter` on a row does not restore anything.** It opens the question for that action: its
//! preview and two buttons, **Restore** and **Cancel**, with Cancel chosen. That is the same
//! strength as the `Undo it? [y/N]` of the plain `undo`: only `y`, or `Enter` on Restore after
//! moving there, restores. `Esc` or `n` cancels.
//!
//! The rows are plain data made by the caller; this file never reads the journal.

use crate::boxes::{Panel, open};
use crate::caps::Caps;
use crate::panels::MAX_WIDTH;
use crate::text::{Line, Tone};
use crate::widgets::buttons::Buttons;
use crate::widgets::list::{List, ListItem};
use crate::widgets::{Key, Step, Widget};

/// One action that can be undone.
#[derive(Clone, Debug)]
pub struct UndoEntry {
    /// The command that was run (one line, already cut).
    pub label: String,
    /// Where, how many items, how long ago, the number.
    pub detail: String,
    /// What undoing it would do: the lines of the preview.
    pub preview: Vec<Line>,
}

enum Stage {
    /// Choosing a row.
    Pick,
    /// Asking about this entry.
    Confirm(usize),
}

/// The picker. Its answer is the index of the entry to restore.
pub struct UndoPicker {
    entries: Vec<UndoEntry>,
    list: List,
    stage: Stage,
    buttons: Buttons,
}

/// Rows of the list shown at once.
const ROWS: usize = 6;

fn question() -> Buttons {
    Buttons::new(&[("Restore", 'y'), ("Cancel", 'n')], 1)
}

impl UndoPicker {
    /// A picker over `entries`, newest first.
    #[must_use]
    pub fn new(entries: Vec<UndoEntry>) -> Self {
        let items = entries
            .iter()
            .map(|entry| ListItem::new(&entry.label, &entry.detail))
            .collect();
        Self {
            entries,
            list: List::new(items, ROWS).filterable(),
            stage: Stage::Pick,
            buttons: question(),
        }
    }

    /// True while the question about one action is on screen.
    #[must_use]
    pub fn asking(&self) -> bool {
        matches!(self.stage, Stage::Confirm(_))
    }
}

impl Widget for UndoPicker {
    type Output = usize;

    fn handle(&mut self, key: Key) -> Step<usize> {
        match self.stage {
            Stage::Pick => match self.list.handle(key) {
                Step::Done(index) => {
                    self.stage = Stage::Confirm(index);
                    self.buttons = question();
                    Step::Stay
                }
                Step::Stay => Step::Stay,
                Step::Cancel => Step::Cancel,
            },
            Stage::Confirm(index) => match self.buttons.handle(key) {
                Step::Done(0) => Step::Done(index),
                Step::Done(_) | Step::Cancel => Step::Cancel,
                Step::Stay => Step::Stay,
            },
        }
    }

    fn render(&self, caps: &Caps, width: u16) -> Vec<Line> {
        let width = width.min(MAX_WIDTH);
        let inner = width.saturating_sub(2);
        let (arrows, vertical) = if caps.unicode {
            ("←→", "↑↓")
        } else {
            ("Left/Right", "Up/Down")
        };
        match self.stage {
            Stage::Pick => {
                let mut panel = Panel::new("Undo")
                    .hint(vertical, "move")
                    .hint("Enter", "review")
                    .hint("Esc", "cancel")
                    .lines(self.list.render(caps, inner));
                if let Some(entry) = self.list.chosen().and_then(|index| self.entries.get(index)) {
                    panel = panel.line(Line::new()).lines(entry.preview.iter().cloned());
                }
                open(&panel, caps, width)
            }
            Stage::Confirm(index) => {
                let mut panel = Panel::new("Undo this?")
                    .tone(Tone::Warn)
                    .hint(arrows, "move")
                    .hint("Enter", "choose")
                    .hint("y", "restore")
                    .hint("n", "cancel");
                if let Some(entry) = self.entries.get(index) {
                    panel = panel.lines(entry.preview.iter().cloned());
                }
                panel = panel.line(Line::new());
                panel = panel.lines(self.buttons.render(caps, inner));
                open(&panel, caps, width)
            }
        }
    }
}

#[cfg(test)]
mod tests;
