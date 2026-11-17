//! Snapshots of rendered panels (UI upgrade, U1), in the looks the spec asks for: normal,
//! `NO_COLOR`, ASCII (a locale that is not UTF-8), a pipe (the numbered fallback, no escapes),
//! and Bangla file names. A panel is data; the snapshot shows the text with every color written
//! as `⟨parameters⟩`, so a change in color shows in the diff.
//!
//! Every snapshot is also checked to be palette-only, and the pipe ones to hold no escape.

mod common;

use omsh_config::Icons;
use omsh_ui::boxes::{Panel, full, open};
use omsh_ui::caps::{Caps, CapsFacts, Out};
use omsh_ui::text::{Line, Style, Tone, render};
use omsh_ui::theme::Theme;
use omsh_ui::widgets::buttons::Buttons;
use omsh_ui::widgets::list::{List, ListItem};
use omsh_ui::widgets::{Key, Widget};

const WIDTH: u16 = 56;

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

/// The looks, by the name used in the snapshot files.
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

const LATIN: [&str; 3] = ["build/output.o", "notes final.txt", "report-2026.pdf"];
const BANGLA: [&str; 3] = ["রিপোর্ট.txt", "নথি/ছবি.png", "ক্ষত্রিয়-তালিকা.md"];

fn written(lines: &[Line], caps: &Caps) -> String {
    let text: Vec<String> = render(lines, &Theme::default(), caps);
    let joined = text.join("\n");
    common::assert_palette_only(&joined);
    if !caps.out_tty {
        assert!(!joined.contains('\x1b'), "a pipe got an escape:\n{joined}");
    }
    common::visible(&joined)
}

fn plain_lines(lines: &[String]) -> String {
    lines.join("\n")
}

fn setup_panel() -> Panel {
    Panel::new("Set up AI")
        .tone(Tone::Ai)
        .line(Line::plain(
            "Pick a provider. Your key stays on this machine.",
        ))
        .line(
            Line::new()
                .tone(Tone::Ok, "safe")
                .tone(Tone::Plain, "  nothing is sent yet"),
        )
        .line(
            Line::new()
                .tone(Tone::Warn, "caution")
                .tone(Tone::Plain, "  a paid provider"),
        )
        .hint("Enter", "choose")
        .hint("Esc", "cancel")
}

fn run_it_panel(files: &[&str]) -> Panel {
    let mut panel = Panel::new("Run it?")
        .tone(Tone::Danger)
        .line(
            Line::new()
                .tone(Tone::Danger, "danger")
                .tone(Tone::Plain, "  rm -rf ")
                .then(&files.join(" "), Style::of(Tone::Path)),
        )
        .line(Line::plain("Deletes 3 files, 14 KB. Not in the trash."));
    for file in files {
        panel = panel.line(
            Line::new()
                .tone(Tone::Dim, "  - ")
                .then(file, Style::of(Tone::Path)),
        );
    }
    panel
        .hint("t", "trash")
        .hint("y", "run")
        .hint("n", "cancel")
}

fn run_it_buttons() -> Buttons {
    Buttons::new(
        &[("Move to trash", 't'), ("Run anyway", 'y'), ("Cancel", 'n')],
        2,
    )
}

fn trash_items(files: &[&str]) -> Vec<ListItem> {
    files
        .iter()
        .enumerate()
        .map(|(n, file)| ListItem::new(file, &format!("/home/user/work/{file}  {n} min ago")))
        .collect()
}

#[test]
fn a_full_box_in_each_look() {
    for name in ["normal", "no_color", "ascii", "pipe"] {
        let caps = look(name);
        let lines = full(&setup_panel(), &caps, WIDTH);
        insta::assert_snapshot!(format!("full_box_{name}"), written(&lines, &caps));
    }
}

#[test]
fn an_open_panel_with_buttons_in_each_look_and_both_scripts() {
    for (script, files) in [("latin", LATIN), ("bangla", BANGLA)] {
        for name in ["normal", "no_color", "ascii", "pipe"] {
            let caps = look(name);
            let mut lines = open(&run_it_panel(&files), &caps, WIDTH);
            let bottom = lines.pop().unwrap();
            if caps.interactive() {
                // The buttons sit inside the rail, as the real panel will have them.
                for row in run_it_buttons().render(&caps, WIDTH - 2) {
                    let mut line =
                        Line::new().tone(Tone::Danger, if caps.unicode { "│ " } else { "| " });
                    line.append(&row);
                    lines.push(line);
                }
                lines.push(bottom);
            } else {
                // Nobody can press a key: the same question, numbered, under the panel.
                lines.push(bottom);
                let prompt = format!("Run it? rm -rf {}", files.join(" "));
                for row in run_it_buttons().to_numbered(&prompt).render() {
                    lines.push(Line::plain(&row));
                }
            }
            insta::assert_snapshot!(format!("run_it_{script}_{name}"), written(&lines, &caps));
        }
    }
}

#[test]
fn the_buttons_row_before_and_after_a_move_and_when_it_is_too_narrow() {
    for name in ["normal", "no_color", "ascii"] {
        let caps = look(name);
        let mut buttons = run_it_buttons();
        let first = written(&buttons.render(&caps, WIDTH), &caps);
        buttons.handle(Key::Left);
        let moved = written(&buttons.render(&caps, WIDTH), &caps);
        let narrow = written(&buttons.render(&caps, 24), &caps);
        insta::assert_snapshot!(
            format!("buttons_{name}"),
            format!("default:\n{first}\n\nafter Left:\n{moved}\n\nin 24 columns:\n{narrow}")
        );
    }
}

#[test]
fn a_filterable_list_in_each_look_and_both_scripts() {
    for (script, files) in [("latin", LATIN), ("bangla", BANGLA)] {
        for name in ["normal", "no_color", "ascii"] {
            let caps = look(name);
            let mut list = List::new(trash_items(&files), 2).filterable();
            let top = written(&list.render(&caps, WIDTH), &caps);
            list.handle(Key::Down);
            list.handle(Key::Down);
            let scrolled = written(&list.render(&caps, WIDTH), &caps);
            let mut filtered = List::new(trash_items(&files), 2).filterable();
            filtered.handle(Key::Char('z'));
            filtered.handle(Key::Char('z'));
            let none = written(&filtered.render(&caps, WIDTH), &caps);
            let narrow = written(&list.render(&caps, 20), &caps);
            insta::assert_snapshot!(
                format!("list_{script}_{name}"),
                format!(
                    "top:\n{top}\n\nafter two Down:\n{scrolled}\n\nfilter zz:\n{none}\n\nin 20 columns:\n{narrow}"
                )
            );
        }
    }
}

#[test]
fn without_a_terminal_the_questions_are_numbered_plain_text() {
    let caps = look("pipe");
    assert!(!caps.interactive());
    for (script, files) in [("latin", LATIN), ("bangla", BANGLA)] {
        let question = run_it_buttons().to_numbered(&format!("Run it? rm -rf {}", files.join(" ")));
        let list = List::new(trash_items(&files), 5).to_numbered("Restore which?");
        let text = format!(
            "{}\n\n{}",
            plain_lines(&question.render()),
            plain_lines(&list.render())
        );
        assert!(!text.contains('\x1b'));
        insta::assert_snapshot!(format!("numbered_{script}"), text);
    }
}

#[test]
fn a_hostile_file_name_cannot_reach_the_terminal_through_any_panel() {
    let name = "x\x1b[2J\x1b]0;owned\x07\u{202e}fdp.exe";
    for look_name in ["normal", "ascii"] {
        let caps = look(look_name);
        let panel = Panel::new("Run it?").line(Line::plain(name));
        for lines in [full(&panel, &caps, WIDTH), open(&panel, &caps, WIDTH)] {
            let out = render(&lines, &Theme::default(), &caps).join("\n");
            common::assert_palette_only(&out);
            assert!(!out.contains("\x1b[2J") && !out.contains("\x1b]") && !out.contains('\u{7}'));
            assert!(!out.contains('\u{202e}'));
        }
        let list = List::new(vec![ListItem::new(name, name)], 3);
        let out = render(&list.render(&caps, WIDTH), &Theme::default(), &caps).join("\n");
        common::assert_palette_only(&out);
        let numbered = list.to_numbered(name).render().join("\n");
        assert!(!numbered.contains('\x1b') && !numbered.contains('\u{202e}'));
    }
}
