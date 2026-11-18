//! The plan as the panel with buttons shows it, and the questions around it (UI upgrade, phase 4).
//!
//! Nothing here decides what runs. It turns the same reviewed steps that the text panel prints
//! into [`StepView`]s, shows [`PlanPanel`] and the other panels through
//! [`run_on_terminal`], and maps what the person chose back to the choices the flow already has.
//! Anything but a clear choice is a cancel.

use omsh_exec::{Terminal, hung_up};
use omsh_safety::Level;
use omsh_ui::Paint;
use omsh_ui::inline::Ended;
use omsh_ui::panels::details::Details;
use omsh_ui::panels::menu::{current_caps, run_on_terminal};
use omsh_ui::panels::plan::{PlanChoice, PlanPanel, Risk, StepView};
use omsh_ui::panels::plan_run::{PauseChoice, PausePanel, Progress};
use omsh_ui::panels::step::{StepChoice, StepPanel};
use omsh_ui::panels::{MenuUi, inner_width};
use omsh_ui::text::{Line, Style, Tone, wrap};

use super::{Choice, FAILED, INTERRUPTED, PlanEnv, Reviewed, SOFT_TURN_LIMIT, Settings, step_body};
use crate::ai::usage::{Calls, Record};
use crate::radius::BlastLimits;

/// The risk word for a level.
pub(super) const fn risk_of(level: Level) -> Risk {
    match level {
        Level::Safe => Risk::Safe,
        Level::Caution => Risk::Caution,
        Level::Dangerous => Risk::Danger,
        Level::Critical => Risk::Critical,
    }
}

/// The lines of `text` as panel rows, wrapped to `cols`. Each source line starts with five blanks
/// (the text panel's indent); the first word says what kind of line it is, and that decides its
/// color. Nothing is cut: a long line goes on the next row, indented like the first.
pub(super) fn body_lines(text: &str, cols: usize) -> Vec<Line> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let content = raw.strip_prefix("     ").unwrap_or(raw);
        if content.trim().is_empty() {
            continue;
        }
        let lead = content.len() - content.trim_start().len();
        let rest = content.trim_start();
        let (prefix, tone, prefix_tone) = classify(rest);
        let room = cols.saturating_sub(lead).max(8);
        for (row, piece) in wrap(rest, room).into_iter().enumerate() {
            let mut line = Line::plain(&" ".repeat(lead));
            if row == 0 && !prefix.is_empty() && piece.starts_with(prefix) {
                line.push(prefix, Style::of(prefix_tone));
                line.push(&piece[prefix.len()..], Style::of(tone));
            } else {
                line.push(&piece, Style::of(tone));
            }
            out.push(line);
        }
    }
    out
}

/// The part of a line that is a label, the color of the rest, and the color of the label.
fn classify(rest: &str) -> (&'static str, Tone, Tone) {
    if rest.starts_with("$ ") {
        ("$ ", Tone::Plain, Tone::Dim)
    } else if rest.starts_with("on: ") {
        ("on: ", Tone::Path, Tone::Dim)
    } else if rest.starts_with("for example:") {
        ("", Tone::Dim, Tone::Dim)
    } else if rest.starts_with("- deletes for good") || rest.starts_with("- not undoable") {
        ("- ", Tone::Warn, Tone::Dim)
    } else if rest.starts_with("- ") {
        ("- ", Tone::Plain, Tone::Dim)
    } else if rest.starts_with("needs root:") {
        ("needs root:", Tone::Plain, Tone::Warn)
    } else if rest.starts_with("rm goes to the trash") {
        ("", Tone::Ok, Tone::Ok)
    } else if rest.starts_with('(') {
        ("", Tone::Dim, Tone::Dim)
    } else {
        ("", Tone::Plain, Tone::Plain)
    }
}

/// What a step adds after its risk word.
fn note_of(step: &Reviewed) -> String {
    let rating = step.rating();
    if step.edited {
        "edited".to_owned()
    } else if rating.from_model {
        "the model's estimate".to_owned()
    } else {
        String::new()
    }
}

/// The steps as the panel shows them. The text of each step is the one the text panel prints,
/// made with no colors and then colored by what each line is.
pub(super) fn step_views(
    steps: &[Reviewed],
    blast: BlastLimits,
    journal: bool,
    cols: usize,
) -> Vec<StepView> {
    steps
        .iter()
        .enumerate()
        .map(|(index, step)| {
            let mut text = String::new();
            step_body(&Paint::plain(), index, step, blast, journal, &mut text);
            StepView {
                title: step.title.clone(),
                risk: risk_of(step.rating().level),
                note: note_of(step),
                body: body_lines(&text, cols),
            }
        })
        .collect()
}

fn title(settings: &Settings, turns: usize) -> String {
    let after = match turns {
        0 => String::new(),
        1 => ", after 1 chat turn".to_owned(),
        n => format!(", after {n} chat turns"),
    };
    format!(
        "Plan from {} ({}{after})",
        settings.kind.label(),
        settings.model
    )
}

/// What the panel shows once for the whole plan, under the steps.
fn footer(turns: usize) -> Vec<Line> {
    if turns >= SOFT_TURN_LIMIT {
        vec![
            Line::new(),
            Line::new().tone(
                Tone::Dim,
                &format!(
                    "chat turn {turns}: if only a command needs changing, Edit may be quicker"
                ),
            ),
        ]
    } else {
        Vec::new()
    }
}

/// Where the panel's choices go: the old [`Choice`] of the flow.
fn choice_of(choice: PlanChoice) -> Option<Choice> {
    match choice {
        PlanChoice::RunAll => Some(Choice::Run),
        PlanChoice::StepByStep => Some(Choice::StepByStep),
        PlanChoice::Edit(index) => Some(Choice::Edit(index)),
        PlanChoice::Chat => Some(Choice::Chat),
        PlanChoice::Cancel => Some(Choice::Cancel(FAILED)),
        PlanChoice::View | PlanChoice::Details => None,
    }
}

/// Shows the plan panel (and, on request, what was sent and the details of the call) until the
/// person chooses. `None` when no panel can be shown: the caller asks in words.
#[expect(
    clippy::too_many_arguments,
    reason = "the pieces of the plan flow the panel needs"
)]
pub(super) fn plan_choice(
    env: &mut PlanEnv<'_>,
    ui: &MenuUi,
    calls: &Calls,
    settings: &Settings,
    summary: &str,
    steps: &[Reviewed],
    journal: bool,
    turns: usize,
    record: Option<&Record>,
    expanded: &mut bool,
    view: &str,
) -> Option<Choice> {
    let caps = current_caps(ui);
    let cols = PlanPanel::body_width(&caps);
    let views = step_views(steps, env.blast, journal, cols);
    loop {
        let mut panel = PlanPanel::new(
            &title(settings, turns),
            wrap(summary, inner_width(&caps))
                .into_iter()
                .map(|row| Line::plain(&row))
                .collect(),
            views.clone(),
            footer(turns),
            record.map(|record| calls.call_line(record)),
        )
        .with_expanded(*expanded);
        let ended = run_on_terminal(&mut panel, ui, Box::new(hung_up)).ok()?;
        *expanded = panel.expanded();
        match ended {
            Ended::Chosen(PlanChoice::View) => {
                env.tty.open(env.shell, env.files).err(view);
            }
            Ended::Chosen(PlanChoice::Details) => {
                show_details(ui, calls, record);
            }
            Ended::Chosen(choice) => {
                return choice_of(choice);
            }
            Ended::Cancelled | Ended::Closed => return Some(Choice::Cancel(FAILED)),
            Ended::Interrupted => return Some(Choice::Cancel(INTERRUPTED)),
        }
    }
}

/// The details panel for the calls of this session, starting on the provider of `record`.
pub(super) fn show_details(ui: &MenuUi, calls: &Calls, record: Option<&Record>) {
    let tabs = calls.tabs();
    let selected = record
        .and_then(|record| calls.records().iter().position(|r| r.kind == record.kind))
        .unwrap_or(0);
    let mut details = Details::new(tabs, selected);
    let _ = run_on_terminal(&mut details, ui, Box::new(hung_up));
}

/// What the person chose for one step of a plan that runs step by step.
pub(super) enum StepAnswer {
    Run,
    Skip,
    /// Stop the plan here, with this status.
    Stop(u8),
}

/// The question before step `index` (counting from 0). Anything but a clear Run or Skip stops
/// the plan; so does a panel that cannot be shown.
pub(super) fn step_question(
    ui: &MenuUi,
    index: usize,
    total: usize,
    step: &Reviewed,
    progress: Progress,
) -> StepAnswer {
    let caps = current_caps(ui);
    let cols = PlanPanel::body_width(&caps);
    let mut text = String::new();
    super::block(&mut text, "     $ ", "       ", &step.command);
    if step.edited {
        text.push_str("     (your edit; the model's explanation was for its own command)\n");
    } else if !step.explanation.trim().is_empty() {
        super::block(&mut text, "     ", "     ", &step.explanation);
    }
    let mut panel = StepPanel::new(
        index + 1,
        total,
        &step.title,
        risk_of(step.rating().level),
        body_lines(&text, cols),
    )
    .with_progress(progress);
    match run_on_terminal(&mut panel, ui, Box::new(hung_up)) {
        Ok(Ended::Chosen(StepChoice::Run)) => StepAnswer::Run,
        Ok(Ended::Chosen(StepChoice::Skip)) => StepAnswer::Skip,
        Ok(Ended::Chosen(StepChoice::Stop) | Ended::Cancelled | Ended::Closed) | Err(_) => {
            StepAnswer::Stop(FAILED)
        }
        Ok(Ended::Interrupted) => StepAnswer::Stop(INTERRUPTED),
    }
}

/// What the person chose for a step that Ctrl+Z stopped (phase 6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PauseAnswer {
    Continue,
    Skip,
    Stop,
    Background,
}

/// The question about the step `index` (counting from 0) that Ctrl+Z stopped. Anything but a
/// clear answer leaves the step stopped and ends the plan, as it did before the panel existed;
/// so does a panel that cannot be shown.
pub(super) fn pause_question(
    ui: &MenuUi,
    index: usize,
    total: usize,
    step: &Reviewed,
    progress: Progress,
) -> PauseAnswer {
    let caps = current_caps(ui);
    let cols = PlanPanel::body_width(&caps);
    let mut text = String::new();
    super::block(&mut text, "$ ", "  ", &step.command);
    let mut panel = PausePanel::new(index + 1, total, body_lines(&text, cols), progress);
    match run_on_terminal(&mut panel, ui, Box::new(hung_up)) {
        Ok(Ended::Chosen(PauseChoice::Continue)) => PauseAnswer::Continue,
        Ok(Ended::Chosen(PauseChoice::Skip)) => PauseAnswer::Skip,
        Ok(Ended::Chosen(PauseChoice::Background)) => PauseAnswer::Background,
        Ok(
            Ended::Chosen(PauseChoice::Stop)
            | Ended::Cancelled
            | Ended::Closed
            | Ended::Interrupted,
        )
        | Err(_) => PauseAnswer::Stop,
    }
}

/// A dim line for a step that was skipped.
pub(super) fn skipped_line(paint: &Paint, number: usize, title: &str) -> String {
    let line = format!(
        "{} {number}. {} (skipped)",
        paint.glyphs().skip,
        super::shorten(title)
    );
    format!("  {}\n", paint.token(omsh_ui::Token::Dim, &line))
}

/// `terminal` is only used to tell that the panel can be shown.
pub(super) fn can_show(menu: Option<&MenuUi>, terminal: &dyn Terminal) -> Option<MenuUi> {
    menu.filter(|_| terminal.menu_keys()).cloned()
}
