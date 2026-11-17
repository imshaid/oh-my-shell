//! The "Run it?" question: an open-right panel (it shows commands and file names) with the
//! reasons, and a row of buttons: **Move to trash** (only when the trash can take the `rm`),
//! **Run anyway**, **Cancel**. Cancel is chosen when the panel opens, so `Enter` at once does
//! nothing dangerous. `←→`, `↑↓` and `Tab` move, `Enter` chooses, `t`, `y` and `n` choose at
//! once, `Esc` cancels (and Ctrl+C, which the menu driver reports on its own).
//!
//! A critical line has no buttons: it is not answered with one key. The caller draws
//! [`static_panel`] and asks for `yes` or the target to be typed, as before.

use crate::boxes::{Panel, open};
use crate::caps::Caps;
use crate::panels::MAX_WIDTH;
use crate::text::{Line, Tone};
use crate::widgets::buttons::Buttons;
use crate::widgets::{Key, Step, Widget};

/// What the person chose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunChoice {
    /// Run the line with its `rm`s moved to the trash.
    Trash,
    /// Run it as it is.
    Run,
    /// Do not run it.
    Cancel,
}

/// The question, with its buttons.
#[derive(Clone, Debug)]
pub struct RunIt {
    tone: Tone,
    body: Vec<Line>,
    trash: bool,
    buttons: Buttons,
}

impl RunIt {
    /// A question about the lines of `body` (the level, the command, the reasons). `tone` is the
    /// color of the rail (`Danger` or `Warn`). With `offer_trash` the first button moves the
    /// `rm`s to the trash.
    #[must_use]
    pub fn new(tone: Tone, body: Vec<Line>, offer_trash: bool) -> Self {
        let buttons = if offer_trash {
            Buttons::new(
                &[("Move to trash", 't'), ("Run anyway", 'y'), ("Cancel", 'n')],
                2,
            )
        } else {
            Buttons::new(&[("Run anyway", 'y'), ("Cancel", 'n')], 1)
        };
        Self {
            tone,
            body,
            trash: offer_trash,
            buttons,
        }
    }

    /// The button that is chosen now.
    #[must_use]
    pub fn chosen(&self) -> RunChoice {
        self.choice_of(self.buttons.chosen())
    }

    fn choice_of(&self, index: usize) -> RunChoice {
        match (self.trash, index) {
            (true, 0) => RunChoice::Trash,
            (true, 1) | (false, 0) => RunChoice::Run,
            _ => RunChoice::Cancel,
        }
    }

    fn panel(&self, caps: &Caps) -> Panel {
        // Left and right, up and down all move: one hint for the four of them.
        let arrows = if caps.unicode {
            "←→↑↓"
        } else {
            "arrows"
        };
        Panel::new("Run it?")
            .tone(self.tone)
            .hint(arrows, "move")
            .hint("Enter", "choose")
            .hint("Esc", "cancel")
    }
}

impl Widget for RunIt {
    type Output = RunChoice;

    fn handle(&mut self, key: Key) -> Step<RunChoice> {
        match self.buttons.handle(key) {
            Step::Stay => Step::Stay,
            Step::Done(index) => Step::Done(self.choice_of(index)),
            Step::Cancel => Step::Cancel,
        }
    }

    fn render(&self, caps: &Caps, width: u16) -> Vec<Line> {
        let width = width.min(MAX_WIDTH);
        let mut panel = self.panel(caps).lines(self.body.iter().cloned());
        panel = panel.line(Line::new());
        // Inside the rail: two columns less than the panel.
        panel = panel.lines(self.buttons.render(caps, width.saturating_sub(2)));
        open(&panel, caps, width)
    }
}

/// The panel of a question that is not answered with a button (a critical line): the same box,
/// with no buttons, to be drawn once above the typed question.
#[must_use]
pub fn static_panel(tone: Tone, body: Vec<Line>, caps: &Caps, width: u16) -> Vec<Line> {
    let panel = Panel::new("Run it?").tone(tone).lines(body);
    open(&panel, caps, width.min(MAX_WIDTH))
}

#[cfg(test)]
mod tests;
