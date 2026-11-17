use omsh_config::Motion;

use super::*;
use crate::panels::test_support::caps;

#[test]
fn not_now_is_chosen_so_enter_alone_never_starts_a_setup() {
    let mut p = NotSetUp::new();
    assert_eq!(p.chosen(), SetUpChoice::NotNow);
    assert!(matches!(
        p.handle(Key::Enter),
        Step::Done(SetUpChoice::NotNow)
    ));
}

#[test]
fn s_starts_the_setup_and_the_other_keys_do_not() {
    assert!(matches!(
        NotSetUp::new().handle(Key::Char('s')),
        Step::Done(SetUpChoice::SetUp)
    ));
    for key in [Key::Char('n'), Key::Esc] {
        assert!(matches!(
            NotSetUp::new().handle(key),
            Step::Done(SetUpChoice::NotNow)
        ));
    }
    let mut p = NotSetUp::new();
    p.handle(Key::Left);
    assert_eq!(p.chosen(), SetUpChoice::SetUp);
    assert!(matches!(
        p.handle(Key::Enter),
        Step::Done(SetUpChoice::SetUp)
    ));
}

#[test]
fn the_panel_says_nothing_was_sent_and_has_both_buttons() {
    let rows: Vec<String> = NotSetUp::new()
        .render(&caps(Motion::Off, true), 80)
        .iter()
        .map(Line::plain_text)
        .collect();
    let all = rows.join("\n");
    assert!(rows[0].starts_with("╭─ AI is not set up"), "{}", rows[0]);
    assert!(
        all.contains("Nothing was sent")
            && all.contains("Set up AI now")
            && all.contains("Not now"),
        "{all}"
    );
    let ascii: Vec<String> = NotSetUp::new()
        .render(&caps(Motion::Off, false), 80)
        .iter()
        .map(Line::plain_text)
        .collect();
    assert!(ascii.iter().all(|r| r.is_ascii()), "{ascii:?}");
}
