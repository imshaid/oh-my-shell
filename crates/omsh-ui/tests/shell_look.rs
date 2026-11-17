//! Snapshots of the pieces of the shell look (UI upgrade, phase 2) in the four looks the spec
//! asks for: normal, `NO_COLOR`, ASCII (a locale that is not UTF-8) and a pipe (no escapes at
//! all). Every color is written as `⟨parameters⟩`, so a change of color shows in the diff, and
//! every snapshot is checked to be palette-only.

mod common;

use std::time::Duration;

use omsh_config::Icons;
use omsh_ui::caps::{Caps, CapsFacts, Out};
use omsh_ui::shell_look::{AiHint, ai_tag, chip, hint_bar, marks, rule};
use omsh_ui::text::{Line, Tone, render_line};
use omsh_ui::theme::Theme;

const GOOD: CapsFacts = CapsFacts {
    stdin_tty: true,
    stdout_tty: true,
    stderr_tty: true,
    dumb: false,
    no_color: false,
    utf8: true,
    ci: false,
    ssh: false,
    tmux: false,
    env_motion: None,
    size: Some((60, 24)),
};

fn look(name: &str) -> Caps {
    let facts = match name {
        "normal" => GOOD,
        "no_color" => CapsFacts {
            no_color: true,
            ..GOOD
        },
        "ascii" => CapsFacts {
            utf8: false,
            ..GOOD
        },
        "pipe" => CapsFacts {
            stdin_tty: false,
            stdout_tty: false,
            stderr_tty: false,
            ..GOOD
        },
        other => panic!("no look called {other}"),
    };
    Caps::decide(facts, Out::Stderr, Icons::Auto, None)
}

fn written(lines: &[Line], caps: &Caps) -> String {
    let text: Vec<String> = lines
        .iter()
        .map(|line| render_line(line, &Theme::default(), caps))
        .collect();
    let joined = text.join("\n");
    common::assert_palette_only(&joined);
    if !caps.out_tty {
        assert!(!joined.contains('\x1b'), "a pipe got an escape:\n{joined}");
    }
    common::visible(&joined)
}

/// What the shell prints around one prompt, as data.
fn scene(caps: &Caps) -> Vec<Line> {
    let unicode = caps.unicode;
    let slow = Duration::from_millis(3200);
    // A chip that is missing would be a line missing from the snapshot.
    let chips = [
        chip(0, Some(Duration::from_millis(4)), unicode),
        chip(0, Some(slow), unicode),
        chip(127, Some(Duration::from_millis(3)), unicode),
        chip(2, Some(slow), unicode),
    ];
    let mut lines = vec![rule(24, unicode)];
    lines.extend(chips.into_iter().flatten());
    lines.push(ai_tag());
    for ai in [AiHint::Ready, AiHint::NotSetUp, AiHint::Off] {
        lines.push(hint_bar(ai, unicode));
    }
    lines
}

#[test]
fn the_pieces_in_each_look() {
    for name in ["normal", "no_color", "ascii", "pipe"] {
        let caps = look(name);
        insta::assert_snapshot!(format!("shell_look_{name}"), written(&scene(&caps), &caps));
    }
}

/// A header the way the prompt joins it: a Bangla folder (blue), a branch (dim), then the
/// chip. The columns are not measured here (the line editor places the right side); this shows
/// that the pieces stay whole and in order.
#[test]
fn a_bangla_folder_beside_the_chip() {
    for name in ["normal", "ascii"] {
        let caps = look(name);
        let header = Line::new()
            .tone(Tone::Info, "~/প্রকল্প/ক্ষত্রিয়")
            .tone(Tone::Dim, " (main)");
        // A chip that is missing would be a line missing from the snapshot.
        let status = chip(1, Some(Duration::from_millis(40)), caps.unicode);
        let lines: Vec<Line> = std::iter::once(header).chain(status).collect();
        insta::assert_snapshot!(format!("shell_look_bangla_{name}"), written(&lines, &caps));
    }
}

#[test]
fn the_marks_are_exactly_these_bytes() {
    let all = format!(
        "{}|{}|{}",
        marks::PROMPT_START,
        marks::OUTPUT_START,
        marks::command_end(3)
    );
    insta::assert_snapshot!("shell_look_marks", all.replace('\x1b', "⟨ESC⟩"));
}
