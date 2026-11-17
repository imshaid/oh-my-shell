use omsh_config::Icons;

use super::*;
use crate::caps::{CapsFacts, Out};
use crate::text::{render, width};
use crate::theme::Theme;

fn caps_with(utf8: bool, no_color: bool) -> Caps {
    let facts = CapsFacts {
        stdin_tty: true,
        stdout_tty: true,
        stderr_tty: true,
        utf8,
        no_color,
        size: Some((80, 24)),
        ..CapsFacts::default()
    };
    Caps::decide(facts, Out::Stderr, Icons::Auto, None)
}

fn caps() -> Caps {
    caps_with(true, false)
}

fn body() -> Vec<Line> {
    vec![
        Line::new()
            .tone(Tone::Danger, "dangerous")
            .tone(Tone::Plain, "  rm -rf build"),
        Line::plain("Deletes files without asking."),
    ]
}

fn question(trash: bool) -> RunIt {
    RunIt::new(Tone::Danger, body(), trash)
}

#[test]
fn cancel_is_chosen_when_the_panel_opens_and_enter_picks_it() {
    for trash in [true, false] {
        let mut panel = question(trash);
        assert_eq!(panel.chosen(), RunChoice::Cancel, "trash {trash}");
        assert_eq!(panel.handle(Key::Enter), Step::Done(RunChoice::Cancel));
    }
}

#[test]
fn letters_choose_at_once_and_t_needs_the_trash_to_be_offered() {
    assert_eq!(
        question(true).handle(Key::Char('t')),
        Step::Done(RunChoice::Trash)
    );
    assert_eq!(
        question(true).handle(Key::Char('y')),
        Step::Done(RunChoice::Run)
    );
    assert_eq!(
        question(true).handle(Key::Char('N')),
        Step::Done(RunChoice::Cancel)
    );
    assert_eq!(
        question(false).handle(Key::Char('y')),
        Step::Done(RunChoice::Run)
    );
    assert_eq!(question(false).handle(Key::Char('t')), Step::Stay);
}

#[test]
fn left_right_up_down_and_tab_all_move_and_enter_chooses() {
    // From Cancel (the last button) one step back reaches Run anyway, whatever the key.
    for key in [Key::Left, Key::Up, Key::BackTab] {
        let mut panel = question(true);
        assert_eq!(panel.handle(key), Step::Stay);
        assert_eq!(panel.chosen(), RunChoice::Run, "{key:?}");
        assert_eq!(panel.handle(Key::Enter), Step::Done(RunChoice::Run));
    }
    // Forward from Cancel wraps to the first button.
    for key in [Key::Right, Key::Down, Key::Tab] {
        let mut panel = question(true);
        panel.handle(key);
        assert_eq!(panel.chosen(), RunChoice::Trash, "{key:?}");
    }
    let mut two = question(false);
    two.handle(Key::Left);
    assert_eq!(two.chosen(), RunChoice::Run);
}

#[test]
fn esc_cancels_and_an_unknown_letter_changes_nothing() {
    assert_eq!(question(true).handle(Key::Esc), Step::Cancel);
    let mut panel = question(true);
    assert_eq!(panel.handle(Key::Char('x')), Step::Stay);
    assert_eq!(panel.chosen(), RunChoice::Cancel);
}

#[test]
fn the_panel_has_its_buttons_inside_the_rail_and_no_right_edge() {
    let lines = question(true).render(&caps(), 60);
    let text: Vec<String> = lines.iter().map(Line::plain_text).collect();
    assert!(text[0].starts_with("╭─ Run it? "), "{text:?}");
    assert!(text.last().unwrap().starts_with("╰"), "{text:?}");
    let row = text.iter().find(|row| row.contains("Cancel")).unwrap();
    assert!(row.starts_with("│ "), "{row:?}");
    assert!(row.contains("[Move to trash]") && row.contains("[Run anyway]"));
    assert!(row.trim_end().ends_with("Cancel"), "{row:?}");
    // No right border anywhere on a body row.
    assert!(
        text[1..text.len() - 1]
            .iter()
            .all(|row| !row.ends_with('│'))
    );
    assert!(text.iter().all(|row| width(row) <= 60), "{text:?}");
}

#[test]
fn without_the_trash_there_are_two_buttons() {
    let text: Vec<String> = question(false)
        .render(&caps(), 60)
        .iter()
        .map(Line::plain_text)
        .collect();
    let row = text.iter().find(|row| row.contains("Cancel")).unwrap();
    assert!(
        !row.contains("trash") && row.contains("[Run anyway]"),
        "{row:?}"
    );
}

#[test]
fn the_ascii_look_uses_plain_rules_and_words_for_the_keys() {
    let text: Vec<String> = question(true)
        .render(&caps_with(false, false), 60)
        .iter()
        .map(Line::plain_text)
        .collect();
    assert!(text.iter().all(|row| row.is_ascii()), "{text:?}");
    assert!(text[0].starts_with("+- Run it? "), "{text:?}");
    assert!(text.last().unwrap().contains("arrows"), "{text:?}");
}

#[test]
fn colors_follow_the_level_and_no_color_keeps_the_reverse_video() {
    let lines = question(true).render(&caps(), 60);
    let colored = render(&lines, &Theme::default(), &caps()).join("\n");
    assert!(
        colored.contains("\x1b[31m"),
        "the rail and the level word are red"
    );
    let plain = render(&lines, &Theme::default(), &caps_with(true, true)).join("\n");
    assert!(
        !plain.contains("\x1b[31m") && !plain.contains("\x1b[3"),
        "{plain:?}"
    );
    assert!(
        plain.contains("\x1b[7m"),
        "the chosen button is still reversed"
    );
}

#[test]
fn a_narrow_window_puts_one_button_on_each_row_and_never_wraps() {
    let text: Vec<String> = question(true)
        .render(&caps(), 24)
        .iter()
        .map(Line::plain_text)
        .collect();
    assert!(text.iter().all(|row| width(row) <= 24), "{text:?}");
    assert!(text.iter().filter(|row| row.contains("trash")).count() == 1);
}

/// Everything a hostile file name or command could try: a screen clear, a window title (OSC), a
/// newline and a carriage return, a right-to-left override and other bidi controls, a bell.
const HOSTILE: &str =
    "x\x1b[2J\x1b]0;owned\x07\nline2\rend\u{202e}fdp.exe\u{2066}\u{2069}\u{200f}\x08";

#[test]
fn hostile_text_in_the_command_and_the_file_names_renders_inert() {
    for unicode in [true, false] {
        let caps = caps_with(unicode, false);
        let body = vec![
            Line::new()
                .tone(Tone::Danger, "dangerous  ")
                .tone(Tone::Path, HOSTILE),
            Line::new()
                .tone(Tone::Dim, "on: ")
                .tone(Tone::Path, HOSTILE),
        ];
        let lines = RunIt::new(Tone::Danger, body.clone(), true).render(&caps, 70);
        let statik = static_panel(Tone::Danger, body, &caps, 70);
        for lines in [lines, statik] {
            let rows = render(&lines, &Theme::default(), &caps);
            assert!(
                rows.iter()
                    .all(|row| !row.contains(['\n', '\r', '\u{7}', '\u{8}']))
            );
            let joined = rows.join("\n");
            assert!(
                !joined.contains("\x1b]") && !joined.contains("\x1b[2J"),
                "{joined:?}"
            );
            for bidi in ['\u{202e}', '\u{2066}', '\u{2069}', '\u{200f}'] {
                assert!(!joined.contains(bidi), "{bidi:?} reached the terminal");
            }
            // The only escapes left are ours: `ESC [ ... m`.
            let mut rest = joined.as_str();
            while let Some(at) = rest.find('\x1b') {
                rest = &rest[at + 1..];
                assert!(rest.starts_with('['), "{joined:?}");
                let end = rest.find('m').expect("an SGR sequence ends with m");
                assert!(rest[1..end].chars().all(|c| c.is_ascii_digit() || c == ';'));
                rest = &rest[end..];
            }
        }
    }
}

#[test]
fn bangla_file_names_are_cut_not_wrapped_and_keep_their_letters() {
    let body = vec![Line::new().tone(Tone::Path, "ক্ষত্রিয়-তালিকা.md রিপোর্ট.txt")];
    let lines = RunIt::new(Tone::Danger, body, true).render(&caps(), 40);
    let text: Vec<String> = lines.iter().map(Line::plain_text).collect();
    assert!(text[1].contains("ক্ষত্রিয়"), "{text:?}");
    assert!(text.iter().all(|row| width(row) <= 40), "{text:?}");
}

#[test]
fn the_static_panel_has_no_buttons() {
    let text: Vec<String> = static_panel(Tone::Danger, body(), &caps(), 60)
        .iter()
        .map(Line::plain_text)
        .collect();
    assert!(text[0].starts_with("╭─ Run it?"));
    assert!(
        text.iter()
            .all(|row| !row.contains("Cancel") && !row.contains("[Run"))
    );
}
