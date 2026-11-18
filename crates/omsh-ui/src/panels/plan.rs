//! The AI plan panel (spec section 5): the steps the model proposes, each with a risk word, the
//! info line of the call, and the buttons **Run all** / **Step by step** / **Edit** / **Chat** /
//! **Cancel**. An open-right panel, because it shows commands and file names.
//!
//! Run all is chosen when the panel opens, so `Enter` runs the plan, as it always did. That is
//! safe because nothing here decides what is allowed: after `Enter` the shell still analyzes every
//! step and asks again for a dangerous one (the "Run it?" panel of phase 3, or the typed `yes` of
//! a critical one).
//!
//! Keys (the old ones keep working): `Enter` runs, `y` runs all, `s` step by step, `e` edit, `c`
//! chat, `v` view what was sent, `i` details of the call, `n`, `q` and `Esc` cancel. `Tab` shows or
//! hides the expanded info line; `←→↑↓` move between the buttons. With several steps `e` first
//! asks which step (a digit or `↑↓` and `Enter`; `Esc` goes back).
//!
//! The panel is plain data. The lines of each step are made by the caller, wrapped to
//! [`PlanPanel::body_width`], so that nothing is cut.

use std::cell::Cell;
use std::time::Duration;

use omsh_config::Motion;

use crate::boxes::{Panel, open};
use crate::caps::Caps;
use crate::panels::facts::CallFacts;
use crate::panels::info::{InfoOptions, info_lines};
use crate::panels::meter::Meter;
use crate::panels::{MAX_WIDTH, inner_width};
use crate::text::{Line, Style, Tone};
use crate::widgets::buttons::Buttons;
use crate::widgets::list::{List, ListItem};
use crate::widgets::{Key, Step, Widget};

/// How risky a step is, in words. The word is always written; the color only adds to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Risk {
    Safe,
    Caution,
    Danger,
    Critical,
}

impl Risk {
    /// The word on the panel.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Safe => "safe",
            Self::Caution => "caution",
            Self::Danger => "danger",
            Self::Critical => "critical",
        }
    }

    /// Its color.
    #[must_use]
    pub const fn tone(self) -> Tone {
        match self {
            Self::Safe => Tone::Ok,
            Self::Caution => Tone::Warn,
            Self::Danger | Self::Critical => Tone::Danger,
        }
    }
}

/// One step as the panel shows it.
#[derive(Clone, Debug)]
pub struct StepView {
    pub title: String,
    pub risk: Risk,
    /// After the risk word, dim: `edited`, `the model's estimate`. May be empty.
    pub note: String,
    /// The lines under the title: the command, the explanation, the findings. Already wrapped.
    pub body: Vec<Line>,
}

/// What the person chose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanChoice {
    /// Run every step in order.
    RunAll,
    /// Ask before each step.
    StepByStep,
    /// Change this step (counting from 0).
    Edit(usize),
    /// Talk to the model about the plan.
    Chat,
    /// Show what was sent to the model; the panel comes back after.
    View,
    /// Show the details of the call; the panel comes back after.
    Details,
    Cancel,
}

/// The facts of the call and how to show them under the plan.
#[derive(Clone, Debug)]
pub struct CallLine {
    pub facts: CallFacts,
    pub fields: Vec<String>,
    pub thinking_warn: u32,
    pub meter: Meter,
}

#[derive(Clone, Copy)]
enum Kind {
    RunAll,
    StepByStep,
    Edit,
    Chat,
    Cancel,
}

enum Stage {
    Main,
    /// Which step to edit.
    Pick(List),
}

/// The plan panel.
pub struct PlanPanel {
    title: String,
    summary: Vec<Line>,
    steps: Vec<StepView>,
    footer: Vec<Line>,
    call: Option<CallLine>,
    buttons: Buttons,
    kinds: Vec<Kind>,
    stage: Stage,
    expanded: bool,
    since_open: Duration,
    /// The motion of the last drawing: `animating` is asked after a drawing, and the panel does
    /// not otherwise know the caps.
    motion: Cell<Motion>,
}

/// The columns the step text is indented by inside the rail.
const INDENT: usize = 3;

impl PlanPanel {
    /// A panel for `steps`. `summary` is what the model said the plan is for; `footer` is a note
    /// under the steps (for example the soft limit on chat turns). `call` is what the call cost.
    #[must_use]
    pub fn new(
        title: &str,
        summary: Vec<Line>,
        steps: Vec<StepView>,
        footer: Vec<Line>,
        call: Option<CallLine>,
    ) -> Self {
        let mut labels = vec![("Run all", 'y')];
        let mut kinds = vec![Kind::RunAll];
        if steps.len() > 1 {
            labels.push(("Step by step", 's'));
            kinds.push(Kind::StepByStep);
        }
        labels.extend([("Edit", 'e'), ("Chat", 'c'), ("Cancel", 'n')]);
        kinds.extend([Kind::Edit, Kind::Chat, Kind::Cancel]);
        Self {
            title: title.to_owned(),
            summary,
            steps,
            footer,
            call,
            buttons: Buttons::new(&labels, 0),
            kinds,
            stage: Stage::Main,
            expanded: false,
            since_open: Duration::ZERO,
            motion: Cell::new(Motion::Off),
        }
    }

    /// The columns the lines of a step may take, so that the caller can wrap to it.
    #[must_use]
    pub fn body_width(caps: &Caps) -> usize {
        inner_width(caps).saturating_sub(INDENT).max(8)
    }

    /// Whether the expanded info line is shown.
    #[must_use]
    pub const fn expanded(&self) -> bool {
        self.expanded
    }

    /// This panel with the expanded info line shown (when the panel comes back after `v`).
    #[must_use]
    pub const fn with_expanded(mut self, expanded: bool) -> Self {
        self.expanded = expanded;
        self
    }

    /// The button that is chosen now, as a choice.
    #[must_use]
    pub fn chosen(&self) -> PlanChoice {
        self.choice_of(self.buttons.chosen())
    }

    fn choice_of(&self, index: usize) -> PlanChoice {
        match self.kinds.get(index).copied().unwrap_or(Kind::Cancel) {
            Kind::RunAll => PlanChoice::RunAll,
            Kind::StepByStep => PlanChoice::StepByStep,
            Kind::Edit => PlanChoice::Edit(0),
            Kind::Chat => PlanChoice::Chat,
            Kind::Cancel => PlanChoice::Cancel,
        }
    }

    fn start_edit(&mut self) -> Step<PlanChoice> {
        if self.steps.len() <= 1 {
            return Step::Done(PlanChoice::Edit(0));
        }
        let items = self
            .steps
            .iter()
            .enumerate()
            .map(|(i, step)| ListItem::new(&format!("{}. {}", i + 1, step.title), step.risk.word()))
            .collect();
        self.stage = Stage::Pick(List::new(items, 8));
        Step::Stay
    }

    fn main_key(&mut self, key: Key) -> Step<PlanChoice> {
        match key {
            Key::Tab => {
                self.expanded = !self.expanded;
                Step::Stay
            }
            Key::Char('v' | 'V') => Step::Done(PlanChoice::View),
            Key::Char('i' | 'I') if self.call.is_some() => Step::Done(PlanChoice::Details),
            Key::Char('q' | 'Q') => Step::Done(PlanChoice::Cancel),
            other => match self.buttons.handle(other) {
                Step::Stay => Step::Stay,
                Step::Cancel => Step::Cancel,
                Step::Done(index) => match self.kinds.get(index) {
                    Some(Kind::Edit) => self.start_edit(),
                    _ => Step::Done(self.choice_of(index)),
                },
            },
        }
    }

    fn meter_line(&self, caps: &Caps) -> Option<Line> {
        self.call
            .as_ref()
            .filter(|call| !call.meter.is_empty())
            .map(|call| call.meter.line(caps, self.since_open))
    }
}

impl Widget for PlanPanel {
    type Output = PlanChoice;

    fn handle(&mut self, key: Key) -> Step<PlanChoice> {
        let count = self.steps.len();
        match &mut self.stage {
            Stage::Main => self.main_key(key),
            Stage::Pick(list) => match key {
                Key::Esc => {
                    self.stage = Stage::Main;
                    Step::Stay
                }
                Key::Char(c) if c.is_ascii_digit() => {
                    let number = c.to_digit(10).map_or(0, |d| d as usize);
                    if number >= 1 && number <= count {
                        Step::Done(PlanChoice::Edit(number - 1))
                    } else {
                        Step::Stay
                    }
                }
                other => match list.handle(other) {
                    Step::Done(index) => Step::Done(PlanChoice::Edit(index)),
                    Step::Stay | Step::Cancel => Step::Stay,
                },
            },
        }
    }

    fn animating(&self) -> bool {
        self.call
            .as_ref()
            .is_some_and(|call| call.meter.moving(self.since_open, self.motion.get()))
    }

    fn tick(&mut self, since_open: Duration) {
        self.since_open = since_open;
    }

    fn render(&self, caps: &Caps, width: u16) -> Vec<Line> {
        self.motion.set(caps.motion);
        let width = width.min(MAX_WIDTH);
        let inner = width.saturating_sub(2);
        let arrows = if caps.unicode { "←→" } else { "arrows" };
        if let Stage::Pick(list) = &self.stage {
            let panel = Panel::new("Edit which step?")
                .tone(Tone::Ai)
                .hint("1-9", "pick")
                .hint("Enter", "choose")
                .hint("Esc", "back")
                .lines(list.render(caps, inner));
            return open(&panel, caps, width);
        }
        let mut panel = Panel::new(&self.title).tone(Tone::Ai);
        if let Some(meter) = self.meter_line(caps) {
            panel = panel.line(meter).line(Line::new());
        }
        panel = panel.lines(self.summary.iter().cloned());
        for (index, step) in self.steps.iter().enumerate() {
            panel = panel.line(Line::new());
            let mut head = Line::new()
                .tone(Tone::Plain, &format!("{}. {}  ", index + 1, step.title))
                .then(step.risk.word(), Style::of(step.risk.tone()));
            if !step.note.is_empty() {
                head.push(&format!(", {}", step.note), Style::of(Tone::Muted));
            }
            panel = panel.line(head);
            for row in &step.body {
                let mut indented = Line::plain(&" ".repeat(INDENT));
                indented.append(row);
                panel = panel.line(indented);
            }
        }
        panel = panel.lines(self.footer.iter().cloned());
        if let Some(call) = &self.call {
            let options = InfoOptions {
                fields: &call.fields,
                thinking_warn: call.thinking_warn,
                expanded: self.expanded,
            };
            let lines = info_lines(&call.facts, &options, &call.meter, caps);
            if !lines.is_empty() {
                panel = panel.line(Line::new()).lines(lines);
            }
        }
        panel = panel.line(Line::new());
        panel = panel.lines(self.buttons.render(caps, inner));
        panel = panel
            .hint("Enter", "run")
            .hint(arrows, "move")
            .hint("v", "context");
        if self.call.is_some() {
            panel = panel.hint("i", "details").hint("Tab", "more");
        }
        panel = panel.hint("Esc", "cancel");
        open(&panel, caps, width)
    }
}

#[cfg(test)]
mod tests;
