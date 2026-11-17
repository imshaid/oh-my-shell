//! Snapshots of the real safety panels (UI upgrade, phase 3): the "Run it?" question and the
//! `undo` picker, in the looks the spec asks for (normal, `NO_COLOR`, ASCII) and with Latin and
//! Bangla file names. The question for a pipe or a dumb terminal is not a panel: it is the text
//! question of `omsh-cli`, which has its own tests.
//!
//! Every snapshot is checked to be palette-only, and no row is wider than the window.

mod common;

use omsh_config::Icons;
use omsh_ui::caps::{Caps, CapsFacts, Out};
use omsh_ui::panels::run_it::{RunIt, static_panel};
use omsh_ui::panels::undo_picker::{UndoEntry, UndoPicker};
use omsh_ui::text::{Line, Tone, render, width};
use omsh_ui::theme::Theme;
use omsh_ui::widgets::{Key, Widget};

const WIDTH: u16 = 70;

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
    Caps::decide(facts, Out::Stderr, Icons::Auto, None)
}

const LOOKS: [&str; 3] = ["normal", "no_color", "ascii"];

const LATIN: [&str; 2] = ["build/output.o", "notes final.txt"];
const BANGLA: [&str; 2] = ["রিপোর্ট.txt", "ক্ষত্রিয়-তালিকা.md"];

fn written(lines: &[Line], caps: &Caps) -> String {
    let rows = render(lines, &Theme::default(), caps);
    let joined = rows.join("\n");
    common::assert_palette_only(&joined);
    assert!(!joined.contains('\n') || rows.len() > 1);
    let seen = common::visible(&joined);
    for row in lines {
        assert!(
            width(&row.plain_text()) <= usize::from(WIDTH),
            "a row is wider than the panel: {:?}",
            row.plain_text()
        );
    }
    seen
}

fn run_it_body(files: &[&str]) -> Vec<Line> {
    let mut body = vec![
        Line::new()
            .tone(Tone::Plain, "this command is ")
            .tone(Tone::Danger, "dangerous")
            .tone(Tone::Plain, ":"),
        Line::new().tone(Tone::Plain, &format!("rm -rf {}", files.join(" "))),
        Line::new().tone(Tone::Dim, "- ").tone(
            Tone::Plain,
            "Deletes files without asking, and folders with everything in",
        ),
        Line::new().tone(Tone::Dim, "  ").tone(
            Tone::Plain,
            "them. This cannot be undone, unless you choose Move to trash.",
        ),
    ];
    for file in files {
        body.push(
            Line::new()
                .tone(Tone::Dim, "    on: ")
                .tone(Tone::Path, file),
        );
    }
    body.push(Line::new().tone(Tone::Plain, "    ").tone(
        Tone::Plain,
        "touches 3 files and 1 folder (14 KiB of file data)",
    ));
    body
}

#[test]
fn the_run_it_panel_in_each_look_and_both_scripts() {
    for (script, files) in [("latin", LATIN), ("bangla", BANGLA)] {
        for name in LOOKS {
            let caps = look(name);
            let mut with_trash = RunIt::new(Tone::Danger, run_it_body(&files), true);
            let first = written(&with_trash.render(&caps, WIDTH), &caps);
            with_trash.handle(Key::Left);
            let moved = written(&with_trash.render(&caps, WIDTH), &caps);
            let without_trash = RunIt::new(Tone::Danger, run_it_body(&files), false);
            let two = written(&without_trash.render(&caps, WIDTH), &caps);
            let narrow = written(
                &RunIt::new(Tone::Danger, run_it_body(&files), true).render(&caps, 36),
                &caps,
            );
            insta::assert_snapshot!(
                format!("safety_run_it_{script}_{name}"),
                format!(
                    "when it opens (Cancel chosen):\n{first}\n\nafter Left:\n{moved}\n\nno trash offered:\n{two}\n\nin 36 columns:\n{narrow}"
                )
            );
        }
    }
}

#[test]
fn a_critical_line_gets_the_panel_without_buttons() {
    for name in LOOKS {
        let caps = look(name);
        let mut body = run_it_body(&["/"]);
        body[0] = Line::new()
            .tone(Tone::Plain, "this command is ")
            .tone(Tone::Danger, "critical")
            .tone(Tone::Plain, ":");
        body[2] = Line::new().tone(Tone::Dim, "- ").tone(
            Tone::Plain,
            "Deletes the system, your home folder, or everything in one of",
        );
        body[3] = Line::new()
            .tone(Tone::Dim, "  ")
            .tone(Tone::Plain, "them. This cannot be undone.");
        let lines = static_panel(Tone::Danger, body, &caps, WIDTH);
        insta::assert_snapshot!(format!("safety_critical_{name}"), written(&lines, &caps));
    }
}

fn entries(files: &[&str]) -> Vec<UndoEntry> {
    let entry = |command: &str, detail: &str, items: &[String]| UndoEntry {
        label: command.to_owned(),
        detail: detail.to_owned(),
        preview: {
            let mut preview = vec![
                Line::new()
                    .tone(Tone::Dim, "ran in ")
                    .tone(Tone::Path, "/home/me/work")
                    .tone(Tone::Dim, "  2026-11-17 09:12"),
            ];
            for item in items {
                preview.push(
                    Line::new()
                        .tone(Tone::Dim, "- ")
                        .tone(Tone::Plain, "bring back ")
                        .tone(Tone::Path, item),
                );
            }
            preview
        },
    };
    let paths: Vec<String> = files.iter().map(|f| format!("/home/me/work/{f}")).collect();
    vec![
        entry(
            &format!("rm -rf {}", files.join(" ")),
            "2 items · 5 min ago · #3",
            &paths,
        ),
        entry("rm old.txt", "1 item · 2 h ago · #2", &paths[..1]),
    ]
}

#[test]
fn the_undo_picker_in_each_look_and_both_scripts() {
    for (script, files) in [("latin", LATIN), ("bangla", BANGLA)] {
        for name in LOOKS {
            let caps = look(name);
            let mut picker = UndoPicker::new(entries(&files));
            let listing = written(&picker.render(&caps, WIDTH), &caps);
            picker.handle(Key::Down);
            let second = written(&picker.render(&caps, WIDTH), &caps);
            picker.handle(Key::Up);
            picker.handle(Key::Enter);
            let asking = written(&picker.render(&caps, WIDTH), &caps);
            insta::assert_snapshot!(
                format!("safety_undo_{script}_{name}"),
                format!(
                    "the list:\n{listing}\n\nafter Down:\n{second}\n\nafter Enter (Cancel chosen):\n{asking}"
                )
            );
        }
    }
}
