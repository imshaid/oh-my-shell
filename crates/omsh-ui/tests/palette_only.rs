//! The colors come from the terminal's 16-color palette and nothing else (UI upgrade, rule 1):
//! no RGB, no 256-color index, no background fill. Two guards:
//!
//! 1. no Rust or shell source in the workspace contains an extended-color escape or a ratatui
//!    `Rgb` or `Indexed` color;
//! 2. everything the output layer writes, for every tone, every attribute, every look and a theme
//!    that uses each of the 16 colors, carries only SGR parameters from the palette.
//!
//! The patterns are built at run time, so that this file does not contain them itself.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use omsh_config::Icons;
use omsh_ui::caps::{Caps, CapsFacts, Out};
use omsh_ui::text::{Line, Style, Tone, render_line};
use omsh_ui::theme::{AnsiColor, RoleStyle, Theme, Token};

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn sources(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        if path.is_dir() {
            if name != "target" && name != ".git" {
                sources(&path, found);
            }
        } else if matches!(path.extension().and_then(|e| e.to_str()), Some("rs" | "sh")) {
            found.push(path);
        }
    }
}

fn forbidden() -> Vec<String> {
    let mut patterns = Vec::new();
    for base in ["38", "48", "58"] {
        for kind in ["2", "5"] {
            patterns.push(format!("{base};{kind}"));
        }
    }
    for color in ["Rgb", "Indexed"] {
        patterns.push(format!("Color::{color}"));
        patterns.push(format!("Style::default().fg({color}"));
    }
    patterns
}

#[test]
fn no_source_file_writes_an_rgb_or_256_color() {
    let mut files = Vec::new();
    sources(&workspace().join("crates"), &mut files);
    sources(&workspace().join("scripts"), &mut files);
    assert!(
        files.len() > 100,
        "the scan found only {} files",
        files.len()
    );
    let patterns = forbidden();
    let mut hits = Vec::new();
    for file in &files {
        let Ok(text) = fs::read_to_string(file) else {
            continue;
        };
        for (number, line) in text.lines().enumerate() {
            for pattern in &patterns {
                if line.contains(pattern.as_str()) {
                    hits.push(format!("{}:{}: {pattern}", file.display(), number + 1));
                }
            }
        }
    }
    assert!(
        hits.is_empty(),
        "extended colors in the source:\n{}",
        hits.join("\n")
    );
}

fn caps_of(facts: CapsFacts) -> Caps {
    Caps::decide(facts, Out::Stderr, Icons::Auto, None)
}

fn looks() -> Vec<Caps> {
    let good = CapsFacts {
        stdin_tty: true,
        stdout_tty: true,
        stderr_tty: true,
        utf8: true,
        size: Some((80, 24)),
        ..CapsFacts::default()
    };
    vec![
        caps_of(good),
        caps_of(CapsFacts {
            no_color: true,
            ..good
        }),
        caps_of(CapsFacts { dumb: true, ..good }),
        caps_of(CapsFacts {
            utf8: false,
            ..good
        }),
        caps_of(CapsFacts {
            stderr_tty: false,
            ..good
        }),
    ]
}

/// A theme that uses every one of the 16 colors somewhere.
fn every_color_theme() -> Vec<Theme> {
    AnsiColor::ALL
        .iter()
        .map(|color| {
            let mut theme = Theme::default();
            for token in Token::ALL {
                theme.set_token(
                    token,
                    RoleStyle {
                        color: Some(*color),
                        ..RoleStyle::PLAIN
                    },
                );
            }
            theme
        })
        .collect()
}

fn every_style() -> Vec<Style> {
    let mut styles = Vec::new();
    for tone in Tone::ALL {
        for selected in [false, true] {
            for underline in [false, true] {
                styles.push(Style {
                    tone,
                    selected,
                    underline,
                });
            }
        }
    }
    styles
}

#[test]
fn everything_the_output_layer_writes_is_palette_only() {
    let mut themes = every_color_theme();
    themes.push(Theme::default());
    let mut written = 0;
    for theme in &themes {
        for caps in looks() {
            for style in every_style() {
                let line = Line::new()
                    .then("text", style)
                    .then(" more", Style::of(Tone::Dim));
                let text = render_line(&line, theme, &caps);
                common::assert_palette_only(&text);
                written += 1;
            }
        }
    }
    assert!(written > 1000);
}

#[test]
fn the_check_itself_catches_what_it_is_for() {
    for bad in [
        format!("\x1b[{};{};255;0;0mred\x1b[0m", "38", "2"),
        format!("\x1b[{};{};208mx\x1b[0m", "38", "5"),
        format!("\x1b[{};{};1;2;3mx", "48", "2"),
        "\x1b[1mbold\x1b[0m".to_owned(),
        "\x1b[41mbackground\x1b[0m".to_owned(),
        "\x1b[2Jclear".to_owned(),
    ] {
        let caught = std::panic::catch_unwind(|| common::assert_palette_only(&bad));
        assert!(caught.is_err(), "not caught: {bad:?}");
    }
    common::assert_palette_only("\x1b[2;90mdim\x1b[0m \x1b[7;36msel\x1b[0m \x1b[4mu\x1b[m");
}

#[test]
fn no_color_removes_every_color_but_keeps_the_attributes() {
    let no_color = looks().remove(1);
    for style in every_style() {
        let line = Line::new().then("text", style);
        let text = render_line(&line, &Theme::default(), &no_color);
        for parameter in common::sgr_parameters(&text).unwrap() {
            let number: u32 = parameter.parse().unwrap();
            assert!(
                matches!(number, 0 | 2 | 4 | 7),
                "NO_COLOR left the color {number} in {text:?}"
            );
        }
    }
}

#[test]
fn faint_text_may_hold_no_letter_or_digit() {
    // Decoration passes: a rule, a dot, the empty part of a bar.
    common::assert_faint_is_decoration("\x1b[2m────\x1b[0m \x1b[2m · \x1b[0mword");
    // A reset by an empty list also ends it.
    common::assert_faint_is_decoration("\x1b[2m·\x1b[mword");
}

#[test]
#[should_panic(expected = "faint text holds")]
fn faint_letters_are_refused() {
    common::assert_faint_is_decoration("\x1b[2mhint\x1b[0m");
}

#[test]
#[should_panic(expected = "faint text holds")]
fn faint_digits_are_refused_also_with_bright_black() {
    common::assert_faint_is_decoration("\x1b[2;90m55 of 55\x1b[0m");
}

/// With the default theme no tone is faint and bright black at the same time, and only the
/// `Dim` tone is faint at all.
#[test]
fn only_the_dim_tone_is_faint_and_never_with_bright_black() {
    let theme = Theme::default();
    for tone in Tone::ALL {
        let written = render_line(&Line::new().tone(tone, "x"), &theme, &looks()[0]);
        let parameters = common::sgr_parameters(&written).unwrap();
        assert!(
            !parameters.contains(&"90".to_owned()),
            "{tone:?}: {written:?}"
        );
        let faint = parameters.contains(&"2".to_owned());
        assert_eq!(faint, tone == Tone::Dim, "{tone:?}: {written:?}");
    }
    let muted = render_line(&Line::new().tone(Tone::Muted, "x"), &theme, &looks()[0]);
    assert_eq!(muted, "x", "muted text has no escape at all");
}
