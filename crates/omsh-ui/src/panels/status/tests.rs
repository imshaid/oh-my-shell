use omsh_config::Motion;

use super::*;
use crate::panels::test_support::caps;

fn rows() -> Vec<Row> {
    vec![
        Row::new("Version", "1.0.0", Tone::Plain),
        Row::new("Build", "unknown (not a release build)", Tone::Dim),
        Row::new(
            "Config file",
            "~/.config/oh-my-shell/config.toml",
            Tone::Path,
        ),
    ]
}

#[test]
fn the_labels_line_up_and_the_panel_has_a_title() {
    let shown: Vec<String> = render(&rows(), &caps(Motion::Off, true), 80)
        .iter()
        .map(Line::plain_text)
        .collect();
    assert!(shown[0].starts_with("╭─ Oh My Shell status"), "{shown:?}");
    assert_eq!(shown[1], "│ Version      1.0.0");
    assert_eq!(shown[2], "│ Build        unknown (not a release build)");
    assert_eq!(shown[3], "│ Config file  ~/.config/oh-my-shell/config.toml");
}

#[test]
fn the_panel_has_no_right_edge_because_it_shows_paths() {
    let shown: Vec<String> = render(&rows(), &caps(Motion::Off, true), 80)
        .iter()
        .map(Line::plain_text)
        .collect();
    let widths: Vec<usize> = shown.iter().map(|row| crate::text::width(row)).collect();
    assert!(widths[1..4].iter().any(|w| *w != widths[1]), "{widths:?}");
    assert!(
        shown[1..4].iter().all(|row| !row.ends_with('│')),
        "{shown:?}"
    );
}

#[test]
fn ascii_is_ascii_and_a_bangla_path_stays_inside_the_window() {
    let mut list = rows();
    list.push(Row::new(
        "State folder",
        format!("/home/{}/state", "বাংলা".repeat(40)),
        Tone::Path,
    ));
    let shown: Vec<String> = render(&list, &caps(Motion::Off, false), 60)
        .iter()
        .map(Line::plain_text)
        .collect();
    assert!(shown[..4].iter().all(|row| row.is_ascii()), "{shown:?}");
    assert!(
        shown.iter().all(|row| crate::text::width(row) <= 60),
        "{shown:?}"
    );
}

#[test]
fn the_plain_form_is_label_colon_value_and_has_no_escape() {
    let lines = plain(&rows());
    assert_eq!(lines[0], "Version: 1.0.0");
    assert_eq!(lines[2], "Config file: ~/.config/oh-my-shell/config.toml");
    let hostile = plain(&[Row::new("Path", "a\x1b[31mred\x07", Tone::Path)]);
    assert!(
        !hostile[0].contains('\x1b') && !hostile[0].contains('\x07'),
        "{hostile:?}"
    );
}
