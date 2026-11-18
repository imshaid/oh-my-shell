//! `omsh status` (UI upgrade, phase 5): what this installation is and what it found out about
//! its terminal, as rows of `label  value`.
//!
//! It is not a widget: nothing in it takes a key. [`render`] draws it as an open-right panel for a
//! terminal (the values hold paths, which are user text), and [`plain`] gives the lines of
//! `label: value` that a pipe or a script gets, with no escapes. A value that is not known says
//! so in words (`unknown`, `not asked yet`): nothing here is ever a guess.

use crate::boxes::{Panel, open};
use crate::caps::Caps;
use crate::panels::MAX_WIDTH;
use crate::text::{Line, Tone};

/// One row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub label: &'static str,
    pub value: String,
    /// How the value is colored; the words say the same, so the color is only a help.
    pub tone: Tone,
}

impl Row {
    #[must_use]
    pub fn new(label: &'static str, value: impl Into<String>, tone: Tone) -> Self {
        Self {
            label,
            value: value.into(),
            tone,
        }
    }
}

/// The width of the label column.
fn label_width(rows: &[Row]) -> usize {
    rows.iter()
        .map(|row| crate::text::width(row.label))
        .max()
        .unwrap_or(0)
}

/// The panel for a terminal.
#[must_use]
pub fn render(rows: &[Row], caps: &Caps, width: u16) -> Vec<Line> {
    let width = width.min(MAX_WIDTH);
    let labels = label_width(rows);
    let mut panel = Panel::new("Oh My Shell status").tone(Tone::Info);
    for row in rows {
        let pad = labels - crate::text::width(row.label);
        panel = panel.line(
            Line::new()
                .tone(Tone::Muted, row.label)
                .tone(Tone::Plain, &" ".repeat(pad + 2))
                .tone(row.tone, &crate::text::harmless(&row.value)),
        );
    }
    open(&panel, caps, width)
}

/// The lines a pipe gets: `label: value`, one row each.
#[must_use]
pub fn plain(rows: &[Row]) -> Vec<String> {
    rows.iter()
        .map(|row| format!("{}: {}", row.label, crate::text::harmless(&row.value)))
        .collect()
}

#[cfg(test)]
mod tests;
