use omsh_config::Motion;

use super::*;
use crate::panels::test_support::caps;

fn rows(widget: &impl Widget, unicode: bool, width: u16) -> Vec<String> {
    widget
        .render(&caps(Motion::Off, unicode), width)
        .iter()
        .map(Line::plain_text)
        .collect()
}

// -- the welcome ------------------------------------------------------------------------------

#[test]
fn the_welcome_starts_on_skip_so_enter_and_esc_change_nothing() {
    for offer_ai in [true, false] {
        let mut welcome = Welcome::new(offer_ai, None);
        assert_eq!(welcome.chosen(), WelcomeChoice::Skip);
        assert!(matches!(
            welcome.handle(Key::Enter),
            Step::Done(WelcomeChoice::Skip)
        ));
        assert!(matches!(
            Welcome::new(offer_ai, None).handle(Key::Esc),
            Step::Done(WelcomeChoice::Skip)
        ));
    }
}

#[test]
fn the_letters_and_the_arrows_pick_the_tour_and_the_setup() {
    assert!(matches!(
        Welcome::new(true, None).handle(Key::Char('t')),
        Step::Done(WelcomeChoice::Tour)
    ));
    assert!(matches!(
        Welcome::new(true, None).handle(Key::Char('a')),
        Step::Done(WelcomeChoice::SetUpAi)
    ));
    let mut welcome = Welcome::new(true, None);
    welcome.handle(Key::Left);
    assert_eq!(welcome.chosen(), WelcomeChoice::SetUpAi);
    welcome.handle(Key::Left);
    assert_eq!(welcome.chosen(), WelcomeChoice::Tour);
}

#[test]
fn without_the_ai_the_welcome_has_no_setup_button_and_its_letter_does_nothing() {
    let shown = rows(&Welcome::new(false, None), true, 80).join("\n");
    assert!(
        shown.contains("Take the tour") && shown.contains("Skip"),
        "{shown}"
    );
    assert!(!shown.contains("Set up AI now"), "{shown}");
    assert!(matches!(
        Welcome::new(false, None).handle(Key::Char('a')),
        Step::Stay
    ));
}

#[test]
fn the_settings_path_is_under_the_box_and_never_inside_it() {
    let shown = rows(
        &Welcome::new(true, Some("~/.config/oh-my-shell/config.toml".to_owned())),
        true,
        80,
    );
    let last = shown.last().unwrap();
    assert_eq!(last, "Settings: ~/.config/oh-my-shell/config.toml");
    assert!(
        shown[0].starts_with("╭─ Welcome to Oh My Shell"),
        "{}",
        shown[0]
    );
    assert!(
        shown[..shown.len() - 1]
            .iter()
            .all(|row| !row.contains("config.toml")),
        "{shown:?}"
    );
    // Every row of the box has the same width: the text in it is ours.
    let widths: Vec<usize> = shown[..shown.len() - 1]
        .iter()
        .map(|row| crate::text::width(row))
        .collect();
    assert!(widths.iter().all(|w| *w == widths[0]), "{widths:?}");
}

#[test]
fn a_bangla_path_is_cut_to_the_window_and_not_the_box() {
    let path = format!("~/{}/config.toml", "বাংলা".repeat(30));
    let shown = rows(&Welcome::new(false, Some(path)), true, 60);
    let last = shown.last().unwrap();
    assert!(crate::text::width(last) <= 60, "{last}");
}

// -- stay up to date --------------------------------------------------------------------------

#[test]
fn no_is_chosen_so_enter_alone_never_turns_the_network_check_on() {
    let mut panel = StayUpdated::new();
    assert!(!panel.chosen());
    assert!(matches!(panel.handle(Key::Enter), Step::Done(false)));
    assert!(matches!(
        StayUpdated::new().handle(Key::Esc),
        Step::Done(false)
    ));
    assert!(matches!(
        StayUpdated::new().handle(Key::Char('n')),
        Step::Done(false)
    ));
}

#[test]
fn only_yes_turns_it_on() {
    assert!(matches!(
        StayUpdated::new().handle(Key::Char('y')),
        Step::Done(true)
    ));
    let mut panel = StayUpdated::new();
    panel.handle(Key::Left);
    assert!(panel.chosen());
    assert!(matches!(panel.handle(Key::Enter), Step::Done(true)));
}

#[test]
fn the_panel_says_exactly_what_the_check_does() {
    let shown = rows(&StayUpdated::new(), true, 80).join("\n");
    for words in [
        "Once a day",
        "one request for a release name",
        "Nothing is downloaded or installed",
        "update.check",
        "Yes, check daily",
        "No, I'll run omsh update --check myself",
    ] {
        assert!(shown.contains(words), "{words}: {shown}");
    }
}

// -- eza --------------------------------------------------------------------------------------

#[test]
fn not_now_is_chosen_so_enter_alone_never_changes_what_ls_runs() {
    let mut panel = UseEza::new();
    assert!(!panel.chosen());
    assert!(matches!(panel.handle(Key::Enter), Step::Done(false)));
    assert!(matches!(UseEza::new().handle(Key::Esc), Step::Done(false)));
    assert!(matches!(
        UseEza::new().handle(Key::Char('n')),
        Step::Done(false)
    ));
    assert!(matches!(
        UseEza::new().handle(Key::Char('u')),
        Step::Done(true)
    ));
}

#[test]
fn the_eza_panel_names_the_setting_and_how_to_undo_it() {
    let shown = rows(&UseEza::new(), true, 90).join("\n");
    for words in [
        "does not read the aliases of fish or bash",
        "ui.ls = \"eza\"",
        "Settings, ui.ls",
        "unalias ls",
        "Use eza for ls",
        "Not now",
    ] {
        assert!(shown.contains(words), "{words}: {shown}");
    }
}

// -- all three --------------------------------------------------------------------------------

#[test]
fn in_ascii_every_row_is_ascii_and_no_row_is_wider_than_the_window() {
    let welcome = Welcome::new(true, Some("/home/x/config.toml".to_owned()));
    for width in [40, 60, 80, 100] {
        for shown in [
            rows(&welcome, false, width),
            rows(&StayUpdated::new(), false, width),
            rows(&UseEza::new(), false, width),
        ] {
            assert!(shown.iter().all(|row| row.is_ascii()), "{shown:?}");
            assert!(
                shown
                    .iter()
                    .all(|row| crate::text::width(row) <= usize::from(width)),
                "{width}: {shown:?}"
            );
        }
    }
}
