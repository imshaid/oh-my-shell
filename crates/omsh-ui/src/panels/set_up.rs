//! "AI is not set up": what a person sees when they type `? question` before they ran
//! `omsh ai setup`. Two buttons, **Set up AI now** and **Not now**, with Not now chosen: `Enter`
//! alone never starts a setup that asks for a key. Nothing was sent, and the panel says so.
//!
//! It is an open-right panel, because the question the person typed is user text; the panel does
//! not repeat it.

use crate::boxes::{Panel, open};
use crate::caps::Caps;
use crate::panels::MAX_WIDTH;
use crate::text::{Line, Tone};
use crate::widgets::buttons::Buttons;
use crate::widgets::{Key, Step, Widget};

/// What the person chose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetUpChoice {
    /// Run the setup now, then answer the question.
    SetUp,
    /// Do nothing.
    NotNow,
}

/// The panel.
pub struct NotSetUp {
    buttons: Buttons,
}

impl NotSetUp {
    #[must_use]
    pub fn new() -> Self {
        Self {
            buttons: Buttons::new(&[("Set up AI now", 's'), ("Not now", 'n')], 1),
        }
    }

    /// The button that is chosen now.
    #[must_use]
    pub fn chosen(&self) -> SetUpChoice {
        choice_of(self.buttons.chosen())
    }
}

impl Default for NotSetUp {
    fn default() -> Self {
        Self::new()
    }
}

fn choice_of(index: usize) -> SetUpChoice {
    if index == 0 {
        SetUpChoice::SetUp
    } else {
        SetUpChoice::NotNow
    }
}

impl Widget for NotSetUp {
    type Output = SetUpChoice;

    fn handle(&mut self, key: Key) -> Step<SetUpChoice> {
        match self.buttons.handle(key) {
            Step::Stay => Step::Stay,
            Step::Done(index) => Step::Done(choice_of(index)),
            Step::Cancel => Step::Done(SetUpChoice::NotNow),
        }
    }

    fn render(&self, caps: &Caps, width: u16) -> Vec<Line> {
        let width = width.min(MAX_WIDTH);
        let arrows = if caps.unicode { "←→" } else { "arrows" };
        let panel = Panel::new("AI is not set up")
            .tone(Tone::Ai)
            .hint(arrows, "move")
            .hint("Enter", "choose")
            .hint("Esc", "not now")
            .line(Line::plain(
                "Nothing was sent: there is no model to ask yet.",
            ))
            .line(Line::plain(
                "Setup asks for a provider and a model, checks them with one small",
            ))
            .line(Line::plain("request, then answers your question."))
            .line(Line::new())
            .lines(self.buttons.render(caps, width.saturating_sub(2)));
        open(&panel, caps, width)
    }
}

#[cfg(test)]
mod tests;
