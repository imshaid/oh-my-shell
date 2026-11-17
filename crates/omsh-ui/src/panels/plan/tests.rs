use omsh_config::Motion;

use super::*;
use crate::panels::test_support::caps;
use crate::text::width;

fn step(title: &str, risk: Risk, command: &str) -> StepView {
    StepView {
        title: title.to_owned(),
        risk,
        note: String::new(),
        body: vec![Line::new().tone(Tone::Dim, "$ ").tone(Tone::Plain, command)],
    }
}

fn steps(n: usize) -> Vec<StepView> {
    (0..n)
        .map(|i| {
            step(
                &format!("Step title {}", i + 1),
                Risk::Safe,
                &format!("echo {}", i + 1),
            )
        })
        .collect()
}

fn call(motion_from: Option<u32>) -> CallLine {
    CallLine {
        facts: CallFacts {
            provider: "Ollama".to_owned(),
            model: "llama3.2".to_owned(),
            time: Some(Duration::from_millis(3200)),
            input: Some(1204),
            output: Some(110),
            thinking: Some(200),
            cached: None,
            total: Some(1514),
            context_used: Some(1514),
            context_limit: Some(32768),
            ..CallFacts::default()
        },
        fields: vec!["time".to_owned(), "in".to_owned(), "out".to_owned()],
        thinking_warn: 4,
        meter: Meter::new(Some(1514), Some(32768), motion_from),
    }
}

fn panel(n: usize, with_call: bool) -> PlanPanel {
    PlanPanel::new(
        "Plan from Ollama (llama3.2)",
        vec![Line::plain("List the files.")],
        steps(n),
        Vec::new(),
        with_call.then(|| call(None)),
    )
}

fn press(panel: &mut PlanPanel, key: Key) -> Step<PlanChoice> {
    panel.handle(key)
}

fn text(panel: &PlanPanel, motion: Motion, unicode: bool) -> Vec<String> {
    panel
        .render(&caps(motion, unicode), 90)
        .iter()
        .map(Line::plain_text)
        .collect()
}

#[test]
fn run_all_is_chosen_when_the_panel_opens_and_enter_runs_the_plan() {
    for n in [1, 2, 3] {
        let mut p = panel(n, false);
        assert_eq!(p.chosen(), PlanChoice::RunAll);
        assert!(matches!(
            press(&mut p, Key::Enter),
            Step::Done(PlanChoice::RunAll)
        ));
    }
}

#[test]
fn the_letters_choose_at_once_and_the_old_keys_still_work() {
    for (key, want) in [
        ('y', PlanChoice::RunAll),
        ('Y', PlanChoice::RunAll),
        ('s', PlanChoice::StepByStep),
        ('c', PlanChoice::Chat),
        ('C', PlanChoice::Chat),
        ('n', PlanChoice::Cancel),
        ('q', PlanChoice::Cancel),
        ('v', PlanChoice::View),
        ('V', PlanChoice::View),
    ] {
        let mut p = panel(2, true);
        match press(&mut p, Key::Char(key)) {
            Step::Done(got) => assert_eq!(got, want, "{key}"),
            _ => panic!("{key} did not choose"),
        }
    }
    let mut p = panel(2, true);
    assert!(matches!(press(&mut p, Key::Esc), Step::Cancel));
}

#[test]
fn a_key_that_means_nothing_changes_nothing() {
    let mut p = panel(2, true);
    for key in [Key::Char('z'), Key::Char('x'), Key::Backspace] {
        assert!(matches!(press(&mut p, key), Step::Stay), "{key:?}");
    }
    assert_eq!(p.chosen(), PlanChoice::RunAll);
}

#[test]
fn a_plan_of_one_step_has_no_step_by_step_button() {
    let one = text(&panel(1, false), Motion::Off, true).join("\n");
    assert!(!one.contains("Step by step"), "{one}");
    let mut p = panel(1, false);
    assert!(matches!(press(&mut p, Key::Char('s')), Step::Stay));
    let two = text(&panel(2, false), Motion::Off, true).join("\n");
    assert!(two.contains("Step by step"), "{two}");
}

#[test]
fn the_arrows_move_between_the_buttons_and_enter_takes_the_chosen_one() {
    let mut p = panel(3, false);
    press(&mut p, Key::Right);
    assert_eq!(p.chosen(), PlanChoice::StepByStep);
    assert!(matches!(
        press(&mut p, Key::Enter),
        Step::Done(PlanChoice::StepByStep)
    ));
    let mut p = panel(3, false);
    press(&mut p, Key::Left);
    assert_eq!(
        p.chosen(),
        PlanChoice::Cancel,
        "Left from the first wraps to Cancel"
    );
}

#[test]
fn edit_goes_straight_to_the_step_when_there_is_one_and_asks_which_when_there_are_more() {
    let mut p = panel(1, false);
    assert!(matches!(
        press(&mut p, Key::Char('e')),
        Step::Done(PlanChoice::Edit(0))
    ));

    let mut p = panel(3, false);
    assert!(matches!(press(&mut p, Key::Char('e')), Step::Stay));
    let shown = text(&p, Motion::Off, true).join("\n");
    assert!(
        shown.contains("Edit which step?") && shown.contains("2. Step title 2"),
        "{shown}"
    );
    assert!(
        matches!(press(&mut p, Key::Char('9')), Step::Stay),
        "no such step"
    );
    assert!(matches!(
        press(&mut p, Key::Char('2')),
        Step::Done(PlanChoice::Edit(1))
    ));

    let mut p = panel(3, false);
    press(&mut p, Key::Char('e'));
    press(&mut p, Key::Down);
    press(&mut p, Key::Down);
    assert!(matches!(
        press(&mut p, Key::Enter),
        Step::Done(PlanChoice::Edit(2))
    ));
}

#[test]
fn esc_in_the_step_question_goes_back_to_the_plan_and_does_not_cancel_it() {
    let mut p = panel(3, false);
    press(&mut p, Key::Char('e'));
    assert!(matches!(press(&mut p, Key::Esc), Step::Stay));
    let shown = text(&p, Motion::Off, true).join("\n");
    assert!(shown.contains("Run all"), "{shown}");
    assert!(
        matches!(press(&mut p, Key::Esc), Step::Cancel),
        "Esc on the plan cancels"
    );
}

#[test]
fn details_need_a_call_and_tab_shows_the_expanded_line() {
    let mut without = panel(2, false);
    assert!(matches!(press(&mut without, Key::Char('i')), Step::Stay));
    let mut p = panel(2, true);
    let collapsed = text(&p, Motion::Off, true).join("\n");
    assert!(
        collapsed.contains("3.2 s · in 1,204 · out 110"),
        "{collapsed}"
    );
    assert!(!collapsed.contains("thinking 200"), "{collapsed}");
    assert!(matches!(press(&mut p, Key::Tab), Step::Stay));
    assert!(p.expanded());
    let expanded = text(&p, Motion::Off, true).join("\n");
    assert!(
        expanded.contains("thinking 200 · cached – · total 1,514"),
        "{expanded}"
    );
    assert!(matches!(
        press(&mut p, Key::Char('i')),
        Step::Done(PlanChoice::Details)
    ));
    press(&mut p, Key::Tab);
    assert!(!p.expanded());
}

#[test]
fn tab_does_not_move_the_buttons() {
    let mut p = panel(3, true);
    press(&mut p, Key::Tab);
    assert_eq!(p.chosen(), PlanChoice::RunAll);
}

#[test]
fn every_step_has_its_risk_as_a_word_with_the_right_color() {
    let p = PlanPanel::new(
        "Plan",
        Vec::new(),
        vec![
            step("a", Risk::Safe, "ls"),
            step("b", Risk::Caution, "mv a b"),
            step("c", Risk::Danger, "rm -r x"),
            StepView {
                note: "edited".to_owned(),
                ..step("d", Risk::Critical, "rm -rf /")
            },
        ],
        Vec::new(),
        None,
    );
    let lines = p.render(&caps(Motion::Off, true), 90);
    let find = |word: &str| {
        lines
            .iter()
            .flat_map(|l| l.spans())
            .find(|s| s.text() == word)
            .unwrap_or_else(|| panic!("no span {word}"))
            .style
            .tone
    };
    assert_eq!(find("safe"), Tone::Ok);
    assert_eq!(find("caution"), Tone::Warn);
    assert_eq!(find("danger"), Tone::Danger);
    assert_eq!(find("critical"), Tone::Danger);
    let all = lines
        .iter()
        .map(Line::plain_text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(all.contains("critical, edited"), "{all}");
}

#[test]
fn the_panel_has_the_rail_the_hints_and_fits_the_window() {
    let rows = text(&panel(3, true), Motion::Off, true);
    assert!(
        rows[0].starts_with("╭─ Plan from Ollama (llama3.2)"),
        "{}",
        rows[0]
    );
    assert!(
        rows.last().unwrap().contains("Esc cancel"),
        "{}",
        rows.last().unwrap()
    );
    for row in &rows {
        assert!(width(row) <= 90, "{row:?}");
    }
    let ascii = text(&panel(3, true), Motion::Off, false);
    assert!(ascii.iter().all(|r| r.is_ascii()), "{ascii:?}");
}

#[test]
fn hostile_and_bangla_text_stays_inside_the_panel() {
    let p = PlanPanel::new(
        "Plan \x1b[31mred\u{202e}",
        vec![Line::plain("summary\nline")],
        vec![step(
            "রিপোর্ট মুছুন",
            Risk::Danger,
            "rm রিপোর্ট.txt \x1b]0;x\x07",
        )],
        Vec::new(),
        None,
    );
    let rows = text(&p, Motion::Off, true);
    for row in &rows {
        assert!(
            !row.contains('\x1b') && !row.contains('\u{202e}') && !row.contains('\x07'),
            "{row:?}"
        );
        assert!(width(row) <= 90, "{row:?}");
    }
    assert!(rows.join("\n").contains("রিপোর্ট"));
}

#[test]
fn the_meter_moves_only_with_full_motion_and_a_key_never_waits_for_it() {
    let moving = |motion| {
        let mut p = PlanPanel::new(
            "Plan",
            Vec::new(),
            steps(2),
            Vec::new(),
            Some(call(Some(0))),
        );
        p.tick(Duration::from_millis(100));
        let _ = p.render(&caps(motion, true), 90);
        p
    };
    assert!(moving(Motion::Full).animating());
    assert!(!moving(Motion::Reduced).animating());
    assert!(!moving(Motion::Off).animating());
    // In the middle of the easing, Enter still runs the plan at once.
    let mut p = moving(Motion::Full);
    assert!(p.animating());
    assert!(matches!(
        press(&mut p, Key::Enter),
        Step::Done(PlanChoice::RunAll)
    ));
    let mut p = moving(Motion::Full);
    assert!(matches!(press(&mut p, Key::Esc), Step::Cancel));
    // Once the easing is over there is nothing left to redraw.
    let mut done = moving(Motion::Full);
    done.tick(Duration::from_secs(2));
    assert!(!done.animating());
}
