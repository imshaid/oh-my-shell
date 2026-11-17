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

fn entry(label: &str, detail: &str, item: &str) -> UndoEntry {
    UndoEntry {
        label: label.to_owned(),
        detail: detail.to_owned(),
        preview: vec![
            Line::new()
                .tone(Tone::Dim, "ran in ")
                .tone(Tone::Path, "/home/me/work"),
            Line::new()
                .tone(Tone::Dim, "- ")
                .tone(Tone::Plain, "bring back ")
                .tone(Tone::Path, item),
        ],
    }
}

fn picker() -> UndoPicker {
    UndoPicker::new(vec![
        entry(
            "rm -rf build",
            "work · 2 items · 5 min ago · #3",
            "/home/me/work/build",
        ),
        entry(
            "rm old.txt",
            "work · 1 item · 2 h ago · #2",
            "/home/me/work/old.txt",
        ),
        entry(
            "rm -r cache",
            "work · 9 items · 1 d ago · #1",
            "/home/me/work/cache",
        ),
    ])
}

fn text(picker: &UndoPicker, width: u16) -> Vec<String> {
    picker
        .render(&caps(), width)
        .iter()
        .map(Line::plain_text)
        .collect()
}

#[test]
fn up_and_down_move_and_the_preview_follows_the_highlighted_row() {
    let mut picker = picker();
    assert!(
        text(&picker, 80)
            .iter()
            .any(|row| row.contains("/home/me/work/build"))
    );
    assert_eq!(picker.handle(Key::Down), Step::Stay);
    let rows = text(&picker, 80);
    assert!(
        rows.iter()
            .any(|row| row.contains("bring back /home/me/work/old.txt")),
        "{rows:?}"
    );
    assert!(
        !rows
            .iter()
            .any(|row| row.contains("bring back /home/me/work/build")),
        "{rows:?}"
    );
    picker.handle(Key::Up);
    assert!(
        text(&picker, 80)
            .iter()
            .any(|row| row.contains("bring back /home/me/work/build"))
    );
}

#[test]
fn enter_on_a_row_restores_nothing_it_asks_with_cancel_chosen() {
    let mut picker = picker();
    assert_eq!(
        picker.handle(Key::Enter),
        Step::Stay,
        "Enter only opens the question"
    );
    assert!(picker.asking());
    let rows = text(&picker, 80);
    assert!(rows[0].contains("Undo this?"), "{rows:?}");
    assert!(
        rows.iter()
            .any(|row| row.contains("[Restore]") && row.contains(" Cancel ")),
        "{rows:?}"
    );
    // Enter again is Enter on Cancel: nothing is restored.
    assert_eq!(picker.handle(Key::Enter), Step::Cancel);
}

#[test]
fn only_y_or_enter_on_restore_restores_and_it_restores_the_chosen_row() {
    let mut picker = picker();
    picker.handle(Key::Down);
    picker.handle(Key::Enter);
    assert_eq!(picker.handle(Key::Char('y')), Step::Done(1));

    let mut picker = self::picker();
    picker.handle(Key::Enter);
    picker.handle(Key::Left);
    assert_eq!(picker.handle(Key::Enter), Step::Done(0));
}

#[test]
fn n_and_esc_cancel_at_the_question_and_esc_cancels_the_list() {
    for key in [Key::Char('n'), Key::Esc] {
        let mut picker = picker();
        picker.handle(Key::Enter);
        assert_eq!(picker.handle(key), Step::Cancel, "{key:?}");
    }
    assert_eq!(picker().handle(Key::Esc), Step::Cancel);
}

#[test]
fn a_letter_filters_the_list_and_the_question_is_for_the_filtered_row() {
    let mut picker = picker();
    for letter in "cache".chars() {
        picker.handle(Key::Char(letter));
    }
    picker.handle(Key::Enter);
    assert_eq!(picker.handle(Key::Char('y')), Step::Done(2));
}

#[test]
fn y_is_a_filter_letter_in_the_list_not_an_answer() {
    let mut picker = picker();
    assert_eq!(picker.handle(Key::Char('y')), Step::Stay);
    assert!(!picker.asking());
}

#[test]
fn the_list_is_an_open_panel_with_the_rail_and_fits_the_width() {
    let rows = text(&picker(), 50);
    assert!(rows[0].starts_with("╭─ Undo "), "{rows:?}");
    assert!(rows.iter().all(|row| width(row) <= 50), "{rows:?}");
    assert!(
        rows[1..rows.len() - 1]
            .iter()
            .all(|row| row.starts_with('│') && !row.ends_with('│'))
    );
}

#[test]
fn hostile_text_in_commands_and_file_names_renders_inert_in_the_list_and_the_question() {
    const HOSTILE: &str = "x\x1b[2J\x1b]0;owned\x07\nline2\rend\u{202e}fdp.exe\u{2066}\u{200f}";
    for unicode in [true, false] {
        let caps = caps_with(unicode, false);
        let mut picker = UndoPicker::new(vec![entry(HOSTILE, HOSTILE, HOSTILE)]);
        for step in 0..2 {
            let rows = render(&picker.render(&caps, 70), &Theme::default(), &caps);
            let joined = rows.join("\n");
            assert!(
                rows.iter().all(|row| !row.contains(['\n', '\r', '\u{7}'])),
                "{joined:?}"
            );
            assert!(
                !joined.contains("\x1b]") && !joined.contains("\x1b[2J"),
                "{joined:?}"
            );
            for bidi in ['\u{202e}', '\u{2066}', '\u{200f}'] {
                assert!(!joined.contains(bidi), "step {step}: {bidi:?}");
            }
            picker.handle(Key::Enter);
        }
    }
}

#[test]
fn bangla_commands_and_paths_keep_their_letters_and_fit() {
    let picker = UndoPicker::new(vec![entry(
        "rm রিপোর্ট.txt ক্ষত্রিয়-তালিকা.md",
        "কাজ · ২ items · ৫ min ago · #3",
        "/home/me/নথি/ছবি.png",
    )]);
    let rows = text(&picker, 44);
    assert!(rows.iter().any(|row| row.contains("রিপোর্ট")), "{rows:?}");
    assert!(rows.iter().all(|row| width(row) <= 44), "{rows:?}");
}

#[test]
fn an_empty_picker_shows_no_match_and_cannot_be_chosen() {
    let mut picker = UndoPicker::new(Vec::new());
    assert_eq!(picker.handle(Key::Enter), Step::Stay);
    assert!(!picker.asking());
}
