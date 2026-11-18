//! The question before one step of a plan run step by step: the step, its risk, and the buttons
//! **Run step** / **Skip** / **Stop**. Run step is chosen when it opens, as the plan panel's
//! **Run all** is; the shell still analyzes the step at its turn and asks again if it is risky.
//! `y`, `s` and `n` choose at once, `Enter` takes the chosen button, `Esc` stops.

use crate::boxes::{Panel, open};
use crate::caps::Caps;
use crate::panels::MAX_WIDTH;
use crate::panels::plan::Risk;
use crate::panels::plan_run::Progress;
use crate::text::{Line, Style, Tone};
use crate::widgets::buttons::Buttons;
use crate::widgets::{Key, Step, Widget};

/// What the person chose for this step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepChoice {
    Run,
    Skip,
    /// End the plan here; the steps that are left do not run.
    Stop,
}

/// The question for step `number` of `total`.
pub struct StepPanel {
    number: usize,
    total: usize,
    title: String,
    risk: Risk,
    body: Vec<Line>,
    buttons: Buttons,
    /// The steps of the plan and where each is, shown above the step (phase 6).
    progress: Option<Progress>,
}

impl StepPanel {
    #[must_use]
    pub fn new(number: usize, total: usize, title: &str, risk: Risk, body: Vec<Line>) -> Self {
        Self {
            number,
            total,
            title: title.to_owned(),
            risk,
            body,
            buttons: Buttons::new(&[("Run step", 'y'), ("Skip", 's'), ("Stop", 'n')], 0),
            progress: None,
        }
    }

    /// The same question with the steps of the plan, a progress bar and the time so far above
    /// it. Without this the panel is drawn as it was before phase 6.
    #[must_use]
    pub fn with_progress(mut self, progress: Progress) -> Self {
        self.progress = Some(progress);
        self
    }

    /// The button that is chosen now.
    #[must_use]
    pub fn chosen(&self) -> StepChoice {
        choice_of(self.buttons.chosen())
    }
}

fn choice_of(index: usize) -> StepChoice {
    match index {
        0 => StepChoice::Run,
        1 => StepChoice::Skip,
        _ => StepChoice::Stop,
    }
}

impl Widget for StepPanel {
    type Output = StepChoice;

    fn handle(&mut self, key: Key) -> Step<StepChoice> {
        match self.buttons.handle(key) {
            Step::Stay => Step::Stay,
            Step::Done(index) => Step::Done(choice_of(index)),
            Step::Cancel => Step::Done(StepChoice::Stop),
        }
    }

    fn render(&self, caps: &Caps, width: u16) -> Vec<Line> {
        let width = width.min(MAX_WIDTH);
        let arrows = if caps.unicode { "←→" } else { "arrows" };
        let mut panel = Panel::new(&format!("Step {} of {}", self.number, self.total))
            .tone(Tone::Ai)
            .hint(arrows, "move")
            .hint("Enter", "choose")
            .hint("Esc", "stop");
        if let Some(progress) = &self.progress {
            panel = panel.lines(progress.lines(caps));
            panel = panel.line(Line::new());
        }
        panel = panel.line(
            Line::new()
                .tone(Tone::Plain, &format!("{}  ", self.title))
                .then(self.risk.word(), Style::of(self.risk.tone())),
        );
        panel = panel.lines(self.body.iter().cloned());
        panel = panel.line(Line::new());
        panel = panel.lines(self.buttons.render(caps, width.saturating_sub(2)));
        open(&panel, caps, width)
    }
}

#[cfg(test)]
mod tests;
