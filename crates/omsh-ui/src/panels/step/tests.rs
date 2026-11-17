use omsh_config::Motion;

use super::*;
use crate::panels::test_support::caps;

fn panel() -> StepPanel {
    StepPanel::new(
        2,
        3,
        "Remove the build folder",
        Risk::Danger,
        vec![Line::plain("$ rm -r build")],
    )
}

#[test]
fn run_step_is_chosen_when_the_panel_opens() {
    let mut p = panel();
    assert_eq!(p.chosen(), StepChoice::Run);
    assert!(matches!(p.handle(Key::Enter), Step::Done(StepChoice::Run)));
}

#[test]
fn the_letters_and_esc_choose_at_once() {
    for (key, want) in [
        (Key::Char('y'), StepChoice::Run),
        (Key::Char('s'), StepChoice::Skip),
        (Key::Char('n'), StepChoice::Stop),
        (Key::Esc, StepChoice::Stop),
    ] {
        match panel().handle(key) {
            Step::Done(got) => assert_eq!(got, want, "{key:?}"),
            _ => panic!("{key:?}"),
        }
    }
}

#[test]
fn the_arrows_move_and_enter_takes_the_chosen_button() {
    let mut p = panel();
    p.handle(Key::Right);
    assert_eq!(p.chosen(), StepChoice::Skip);
    assert!(matches!(p.handle(Key::Enter), Step::Done(StepChoice::Skip)));
}

#[test]
fn the_panel_says_which_step_and_how_risky() {
    let rows: Vec<String> = panel()
        .render(&caps(Motion::Off, true), 80)
        .iter()
        .map(Line::plain_text)
        .collect();
    assert!(rows[0].starts_with("╭─ Step 2 of 3"), "{}", rows[0]);
    let all = rows.join("\n");
    assert!(all.contains("Remove the build folder  danger"), "{all}");
    assert!(
        all.contains("$ rm -r build") && all.contains("Run step") && all.contains("Skip"),
        "{all}"
    );
}
