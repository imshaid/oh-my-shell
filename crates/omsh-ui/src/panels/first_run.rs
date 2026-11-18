//! The questions of the first start (UI upgrade, phase 5): the welcome, "Stay up to date?" and
//! "Use eza for ls?". They are shown once, on the start that wrote `config.toml`, to a person at
//! a terminal with keys. Every one of them starts on the answer that changes nothing, and `Esc`
//! is that answer, so a person who presses `Enter` or `Esc` at each of them has changed nothing.
//!
//! All three are full boxes: every word in them is ours. A path (the settings file) is user
//! text, so the welcome shows it on a line of its own **under** the box.

use crate::boxes::{Panel, full};
use crate::caps::Caps;
use crate::panels::MAX_WIDTH;
use crate::text::{Line, Tone, fit, wrap};
use crate::widgets::buttons::Buttons;
use crate::widgets::{Key, Step, Widget};

/// What the welcome ended with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WelcomeChoice {
    /// Show `omsh tour`.
    Tour,
    /// Start `omsh ai setup`.
    SetUpAi,
    /// Nothing.
    Skip,
}

/// The hint row of the three panels: the arrows are named when the terminal cannot draw them.
fn arrows(caps: &Caps) -> &'static str {
    if caps.unicode { "←→" } else { "arrows" }
}

/// A full box: `paragraphs` wrapped to the box, a blank row, then the buttons.
fn boxed(
    title: &str,
    tone: Tone,
    last_hint: (&str, &str),
    paragraphs: &[&str],
    buttons: &Buttons,
    caps: &Caps,
    width: u16,
) -> Vec<Line> {
    let width = width.min(MAX_WIDTH);
    let inner = usize::from(width).saturating_sub(4).max(8);
    let mut panel = Panel::new(title)
        .tone(tone)
        .hint(arrows(caps), "move")
        .hint("Enter", "choose")
        .hint(last_hint.0, last_hint.1);
    for paragraph in paragraphs {
        for row in wrap(paragraph, inner) {
            panel = panel.line(Line::plain(&row));
        }
    }
    panel = panel
        .line(Line::new())
        .lines(buttons.render(caps, u16::try_from(inner).unwrap_or(u16::MAX)));
    full(&panel, caps, width)
}

// -- the welcome ----------------------------------------------------------------------------

/// "Welcome to Oh My Shell": the tour, the AI setup, or nothing. **Skip** is chosen.
pub struct Welcome {
    choices: Vec<WelcomeChoice>,
    buttons: Buttons,
    /// Where `config.toml` is, shown under the box.
    settings: Option<String>,
}

impl Welcome {
    /// `offer_ai` is false when the AI is switched off (the flag or the configuration), so that
    /// the welcome does not offer what cannot be done.
    #[must_use]
    pub fn new(offer_ai: bool, settings: Option<String>) -> Self {
        let mut choices = vec![WelcomeChoice::Tour];
        let mut items = vec![("Take the tour", 't')];
        if offer_ai {
            choices.push(WelcomeChoice::SetUpAi);
            items.push(("Set up AI now", 'a'));
        }
        choices.push(WelcomeChoice::Skip);
        items.push(("Skip", 's'));
        let default = choices.len() - 1;
        Self {
            choices,
            buttons: Buttons::new(&items, default),
            settings,
        }
    }

    /// The button that is chosen now.
    #[must_use]
    pub fn chosen(&self) -> WelcomeChoice {
        self.choices[self.buttons.chosen()]
    }
}

impl Widget for Welcome {
    type Output = WelcomeChoice;

    fn handle(&mut self, key: Key) -> Step<WelcomeChoice> {
        match self.buttons.handle(key) {
            Step::Stay => Step::Stay,
            Step::Done(index) => Step::Done(self.choices[index]),
            Step::Cancel => Step::Done(WelcomeChoice::Skip),
        }
    }

    fn render(&self, caps: &Caps, width: u16) -> Vec<Line> {
        let mut rows = boxed(
            "Welcome to Oh My Shell",
            Tone::Info,
            ("Esc", "skip"),
            &[
                "Ctrl+R searches history, Ctrl+P opens the palette, and `undo` restores deletions.",
                "Nothing is sent anywhere unless you set up AI or turn on the update check.",
                "This is shown once.",
            ],
            &self.buttons,
            caps,
            width,
        );
        if let Some(path) = &self.settings {
            let room = usize::from(width.min(MAX_WIDTH)).saturating_sub(10).max(8);
            rows.push(
                Line::new()
                    .tone(Tone::Muted, "Settings: ")
                    .tone(Tone::Path, &fit(path, room, caps.unicode)),
            );
        }
        rows
    }
}

// -- stay up to date ------------------------------------------------------------------------

/// "Stay up to date?": the daily check of the update. **No** is chosen: the check uses the
/// network, so it starts only when a person moves to Yes.
pub struct StayUpdated {
    buttons: Buttons,
}

impl StayUpdated {
    #[must_use]
    pub fn new() -> Self {
        Self {
            buttons: Buttons::new(
                &[
                    ("Yes, check daily", 'y'),
                    ("No, I'll run omsh update --check myself", 'n'),
                ],
                1,
            ),
        }
    }

    /// True when "Yes" is chosen now.
    #[must_use]
    pub fn chosen(&self) -> bool {
        self.buttons.chosen() == 0
    }
}

impl Default for StayUpdated {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for StayUpdated {
    /// True: check daily.
    type Output = bool;

    fn handle(&mut self, key: Key) -> Step<bool> {
        match self.buttons.handle(key) {
            Step::Stay => Step::Stay,
            Step::Done(index) => Step::Done(index == 0),
            Step::Cancel => Step::Done(false),
        }
    }

    fn render(&self, caps: &Caps, width: u16) -> Vec<Line> {
        boxed(
            "Stay up to date?",
            Tone::Info,
            ("Esc", "no"),
            &[
                "Once a day, a moment after the prompt, Oh My Shell can ask GitHub which release is the newest. That is one request for a release name.",
                "Nothing is downloaded or installed, and you see one line when there is news. `omsh update` installs it, and checks the file first.",
                "You can change this later: Ctrl+P, Settings, update.check.",
            ],
            &self.buttons,
            caps,
            width,
        )
    }
}

// -- eza ------------------------------------------------------------------------------------

/// "Use eza for ls?": shown only when `eza` is on `PATH`. **Not now** is chosen.
pub struct UseEza {
    buttons: Buttons,
}

impl UseEza {
    #[must_use]
    pub fn new() -> Self {
        Self {
            buttons: Buttons::new(&[("Use eza for ls", 'u'), ("Not now", 'n')], 1),
        }
    }

    /// True when "Use eza" is chosen now.
    #[must_use]
    pub fn chosen(&self) -> bool {
        self.buttons.chosen() == 0
    }
}

impl Default for UseEza {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for UseEza {
    /// True: `ls` runs `eza`.
    type Output = bool;

    fn handle(&mut self, key: Key) -> Step<bool> {
        match self.buttons.handle(key) {
            Step::Stay => Step::Stay,
            Step::Done(index) => Step::Done(index == 0),
            Step::Cancel => Step::Done(false),
        }
    }

    fn render(&self, caps: &Caps, width: u16) -> Vec<Line> {
        boxed(
            "Use eza for ls?",
            Tone::Info,
            ("Esc", "not now"),
            &[
                "Oh My Shell does not read the aliases of fish or bash, so `ls` runs the system ls. eza is on your PATH.",
                "Choosing it writes ui.ls = \"eza\" to config.toml, and `ls` runs eza from now on. To undo it: Ctrl+P, Settings, ui.ls, or `unalias ls` for one session.",
            ],
            &self.buttons,
            caps,
            width,
        )
    }
}

#[cfg(test)]
mod tests;
