//! Snapshots of the first-start panels and of `omsh status` (UI upgrade, phase 5), in the looks the
//! spec asks for (normal, `NO_COLOR`, ASCII), with a Latin and a Bangla path, and the numbered form
//! that a pipe or a dumb terminal would get for each question.
//!
//! Every snapshot is checked to be palette-only (no RGB, no 256-color index, no background), and
//! no row is wider than the window.

mod common;

use omsh_config::{Icons, Motion};
use omsh_ui::caps::{Caps, CapsFacts, Out};
use omsh_ui::panels::first_run::{StayUpdated, UseEza, Welcome};
use omsh_ui::panels::status::{Row, render as render_status};
use omsh_ui::text::{Line, Tone, render, width};
use omsh_ui::theme::Theme;
use omsh_ui::widgets::Widget;

const WIDTH: u16 = 78;

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
    size: Some((80, 24)),
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
        other => panic!("no look called {other}"),
    };
    Caps::decide(facts, Out::Stderr, Icons::Auto, Some(Motion::Off))
}

const LOOKS: [&str; 3] = ["normal", "no_color", "ascii"];
const LATIN: &str = "~/.config/oh-my-shell/config.toml";
const BANGLA: &str = "~/কনফিগ/রিপোর্ট/config.toml";

fn written(lines: &[Line], caps: &Caps) -> String {
    let rows = render(lines, &Theme::default(), caps);
    let joined = rows.join("\n");
    common::assert_palette_only(&joined);
    for row in lines {
        assert!(
            width(&row.plain_text()) <= usize::from(WIDTH),
            "a row is wider than the window: {:?}",
            row.plain_text()
        );
    }
    common::visible(&joined)
}

#[test]
fn the_welcome() {
    for name in LOOKS {
        let caps = look(name);
        for (which, path) in [("latin", LATIN), ("bangla", BANGLA)] {
            let panel = Welcome::new(true, Some(path.to_owned()));
            insta::assert_snapshot!(
                format!("first_run_welcome_{which}_{name}"),
                written(&panel.render(&caps, WIDTH), &caps)
            );
        }
    }
}

#[test]
fn the_welcome_without_ai_and_with_the_tour_chosen() {
    let caps = look("normal");
    insta::assert_snapshot!(
        "first_run_welcome_no_ai",
        written(&Welcome::new(false, None).render(&caps, WIDTH), &caps)
    );
}

#[test]
fn stay_up_to_date() {
    for name in LOOKS {
        let caps = look(name);
        insta::assert_snapshot!(
            format!("first_run_update_{name}"),
            written(&StayUpdated::new().render(&caps, WIDTH), &caps)
        );
    }
}

#[test]
fn stay_up_to_date_with_yes_chosen() {
    let caps = look("normal");
    let mut panel = StayUpdated::new();
    panel.handle(omsh_ui::widgets::Key::Left);
    insta::assert_snapshot!(
        "first_run_update_yes_chosen",
        written(&panel.render(&caps, WIDTH), &caps)
    );
}

#[test]
fn use_eza_for_ls() {
    for name in LOOKS {
        let caps = look(name);
        insta::assert_snapshot!(
            format!("first_run_eza_{name}"),
            written(&UseEza::new().render(&caps, WIDTH), &caps)
        );
    }
}

fn rows(path: &str) -> Vec<Row> {
    vec![
        Row::new("Version", "1.1.0", Tone::Plain),
        Row::new(
            "Build",
            "2026-09-01, 77 days old (old: omsh update --check)",
            Tone::Warn,
        ),
        Row::new("AI", "ready: gemini, gemini-2.5-flash", Tone::Ai),
        Row::new("Update check", "on, once a day", Tone::Ok),
        Row::new(
            "Last look",
            "2026-11-16 12:00, newest is 1.1.0",
            Tone::Plain,
        ),
        Row::new("Motion", "full (automatic)", Tone::Plain),
        Row::new("Terminal", "xterm-256color, 80x24", Tone::Plain),
        Row::new("Config file", path, Tone::Path),
    ]
}

#[test]
fn the_status_page() {
    for name in LOOKS {
        let caps = look(name);
        for (which, path) in [("latin", LATIN), ("bangla", BANGLA)] {
            insta::assert_snapshot!(
                format!("status_{which}_{name}"),
                written(&render_status(&rows(path), &caps, WIDTH), &caps)
            );
        }
    }
}

/// A pipe or a dumb terminal gets the same questions as numbered text, with the same default.
#[test]
fn the_numbered_forms() {
    let mut text = String::new();
    // (title, buttons, the chosen one at the start): the safe answer of each question.
    for (title, buttons, default) in [
        (
            "Welcome",
            vec![
                ("Take the tour", 't'),
                ("Set up AI now", 'a'),
                ("Skip", 's'),
            ],
            2,
        ),
        (
            "Stay up to date?",
            vec![
                ("Yes, check daily", 'y'),
                ("No, I'll run omsh update --check myself", 'n'),
            ],
            1,
        ),
        (
            "Use eza for ls?",
            vec![("Use eza for ls", 'u'), ("Not now", 'n')],
            1,
        ),
    ] {
        let list = omsh_ui::widgets::buttons::Buttons::new(&buttons, default);
        text.push_str(&list.to_numbered(title).render().join("\n"));
        text.push_str("\n\n");
    }
    insta::assert_snapshot!("first_run_numbered", text);
}
