//! Drawing the settings view into a ratatui frame.
//!
//! ```text
//! Settings                                                  26 of 26
//! > editor.hints                    false   file     ● next start     (the list)
//!   editor.mode                     emacs   default
//! Gray suggestions from history as you type. Enter changes it.        (the chosen one)
//! Changes are saved to config.toml and apply when a new shell starts. (the last result)
//! > hint                                                              (the query)
//! Enter next  Shift+Tab previous  Esc close  ↑↓ move
//! ```

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::model::{Model, Status};
use crate::view::draw::{
    MIN_HEIGHT, MIN_WIDTH, Styles, fit_text, footer_line, group, query_row, spans_with_matches,
    title_row,
};
use omsh_config::settings::Row;

/// The fixed rows around the list: title, detail, status, query, footer.
const FRAME_ROWS: u16 = 5;

/// The longest key is `safety.blast_radius_max_entries` (31 characters).
const KEY_WIDTH: usize = 32;

/// The most and the least columns the value gets.
const VALUE_WIDTH: usize = 24;
const MIN_VALUE: usize = 4;

/// The standing note, and what it says without a file.
const STANDING: &str = "Changes are saved to config.toml and apply when a new shell starts.";
const STANDING_READ_ONLY: &str = "No configuration file in this shell (--no-config): view only.";

pub fn draw(frame: &mut Frame<'_>, model: &mut Model, styles: &Styles) {
    let area = frame.area();
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        frame.render_widget(Paragraph::new("window too small"), area);
        return;
    }
    let width = area.width;
    let list_rows = area.height - FRAME_ROWS;
    model.set_page(usize::from(list_rows));
    let row = |index: u16, height: u16| Rect::new(area.x, area.y + index, width, height);

    let right = format!("{} of {}", group(model.matches()), group(model.total()));
    frame.render_widget(
        Paragraph::new(title_row(styles, "Settings", &right, width)),
        row(0, 1),
    );

    for slot in 0..usize::from(list_rows) {
        let n = model.offset() + slot;
        let (Some(setting), Some(haystack)) = (model.row(n), model.haystack(n)) else {
            break;
        };
        let selected = n == model.selected();
        let places = model.query().positions(haystack);
        let line = setting_line(setting, &places, selected, styles, width);
        let base = if selected {
            styles.selected
        } else {
            styles.plain
        };
        let y = area.y + 1 + u16::try_from(slot).unwrap_or(0);
        frame.render_widget(
            Paragraph::new(line).style(base),
            Rect::new(area.x, y, width, 1),
        );
    }
    if model.matches() == 0 {
        frame.render_widget(
            Paragraph::new(Span::styled("No such setting.", styles.dim)),
            row(1, 1),
        );
    }

    let detail = model.current().map(detail_text).unwrap_or_default();
    frame.render_widget(
        Paragraph::new(Span::styled(
            fit_text(&detail, usize::from(width), styles),
            styles.dim,
        )),
        row(1 + list_rows, 1),
    );
    let (status, style) = status_line(model, styles);
    frame.render_widget(
        Paragraph::new(Span::styled(
            fit_text(&status, usize::from(width), styles),
            style,
        )),
        row(2 + list_rows, 1),
    );
    let (query_line, x) = query_row(styles, model.typed(), model.cursor(), width);
    frame.render_widget(Paragraph::new(query_line), row(3 + list_rows, 1));
    frame.render_widget(
        Paragraph::new(footer_line(
            styles,
            &[
                ("Enter", "next"),
                ("Shift+Tab", "previous"),
                ("Esc", "close"),
                (styles.glyph("↑↓", "Up/Down"), "move"),
            ],
        )),
        row(4 + list_rows, 1),
    );
    frame.set_cursor_position((area.x + x, area.y + 3 + list_rows));
}

fn detail_text(row: &Row) -> String {
    let how = if row.setting.editable {
        "Enter changes it.".to_owned()
    } else {
        row.setting.elsewhere().to_owned()
    };
    let mut text = format!("{} {how}", row.setting.description);
    if row.pending() {
        text.push_str(&format!(" Running now: {}.", row.running));
    }
    text
}

fn status_line(model: &Model, styles: &Styles) -> (String, Style) {
    match model.status() {
        Status::Standing => (
            if model.read_only() {
                STANDING_READ_ONLY
            } else {
                STANDING
            }
            .to_owned(),
            styles.dim,
        ),
        Status::Saved { key, value } => (
            format!("Saved: {key} = {value}. It applies when a new shell starts."),
            styles.plain,
        ),
        Status::Note(text) => (text.clone(), styles.warning),
        Status::Failed(why) => (format!("Not saved: {why}"), styles.danger),
    }
}

/// A row: marker, name, the value the file gives, where it comes from, and a sign when the
/// running shell still has another value.
fn setting_line(
    row: &Row,
    places: &[usize],
    selected: bool,
    styles: &Styles,
    width: u16,
) -> Line<'static> {
    let mark = if selected { "> " } else { "  " };
    let width = usize::from(width);
    let source = if row.in_file { "file" } else { "default" };
    let sign = if row.pending() {
        format!("{} next start", styles.glyph("●", "*"))
    } else {
        String::new()
    };
    // What the source and the sign take at the right edge; the name and the value share the rest.
    let tail = 1
        + 7
        + if sign.is_empty() {
            0
        } else {
            1 + sign.chars().count()
        };
    let key_width = KEY_WIDTH
        .min(width.saturating_sub(2 + tail + MIN_VALUE))
        .max(8);
    let name = spans_with_matches(row.setting.key, places, key_width.saturating_sub(1), styles);
    let used: usize = name.iter().map(Span::width).sum();
    let value_room = width
        .saturating_sub(2 + key_width + tail)
        .clamp(MIN_VALUE, VALUE_WIDTH);
    let value = fit_text(&row.saved, value_room, styles);
    let value_style = if row.setting.editable {
        styles.plain
    } else {
        styles.secondary(selected)
    };

    let mut spans = vec![Span::styled(mark, styles.accent)];
    spans.extend(name);
    spans.push(Span::raw(" ".repeat(key_width.saturating_sub(used))));
    let shown = Span::raw(value.clone()).width();
    spans.push(Span::styled(value, value_style));
    spans.push(Span::raw(" ".repeat(value_room.saturating_sub(shown) + 1)));
    spans.push(Span::styled(
        format!("{source:<7}"),
        styles.secondary(selected),
    ));
    if !sign.is_empty() {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(sign, styles.warning));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Modifier;

    use super::super::model::tests::memory;
    use super::*;
    use crate::look::Look;
    use crate::theme::Theme;
    use omsh_config::Config;

    fn styles(color: bool, unicode: bool) -> Styles {
        Styles::new(
            &Theme::default(),
            Look {
                terminal: true,
                color,
                unicode,
                motion: omsh_config::Motion::Off,
            },
        )
    }

    fn model(text: &str) -> Model {
        let (store, _, _) = memory(text);
        Model::new(Config::default(), Some(Box::new(store)))
    }

    fn screen(
        model: &mut Model,
        styles: &Styles,
        width: u16,
        height: u16,
    ) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, model, styles)).unwrap();
        terminal
    }

    fn rows(terminal: &Terminal<TestBackend>) -> Vec<String> {
        let buffer = terminal.backend().buffer();
        let area = buffer.area;
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buffer[(x, y)].symbol().to_owned())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    fn press(model: &mut Model, code: KeyCode) {
        model.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    #[test]
    fn the_layout_at_80_by_14() {
        let mut model = model("");
        model.paste("hints");
        let terminal = screen(&mut model, &styles(true, true), 80, 14);
        let rows = rows(&terminal);
        assert!(rows[0].starts_with("Settings"));
        assert!(rows[0].ends_with("1 of 28"), "{:?}", rows[0]);
        // Marker, name in 32 columns, value in 24, then where it comes from.
        assert_eq!(
            rows[1],
            format!("> {:<32}{:<24} default", "editor.hints", "true")
        );
        // The detail of the chosen one, the standing note, the query and the keys.
        assert_eq!(
            rows[10],
            "Gray suggestions from history as you type. Enter changes it."
        );
        assert_eq!(
            rows[11],
            "Changes are saved to config.toml and apply when a new shell starts."
        );
        assert_eq!(rows[12], "> hints");
        assert_eq!(
            rows[13],
            "Enter next  Shift+Tab previous  Esc close  ↑↓ move"
        );
    }

    #[test]
    fn with_nothing_typed_the_rows_are_in_the_order_of_the_file() {
        let mut model = model("");
        let terminal = screen(&mut model, &styles(true, true), 80, 14);
        let rows = rows(&terminal);
        assert!(rows[0].ends_with("28 of 28"), "{:?}", rows[0]);
        let keys: Vec<&str> = rows[1..10]
            .iter()
            .map(|row| row[2..].split_whitespace().next().unwrap())
            .collect();
        assert_eq!(
            keys,
            [
                "ui.theme",
                "ui.motion",
                "ui.icons",
                "editor.mode",
                "editor.hints",
                "editor.abbreviations",
                "prompt.left",
                "prompt.right",
                "prompt.transient"
            ]
        );
    }

    #[test]
    fn a_saved_change_is_marked_as_waiting_for_the_next_start() {
        let mut model = model("");
        model.paste("hints");
        press(&mut model, KeyCode::Enter);
        let terminal = screen(&mut model, &styles(true, true), 80, 14);
        let rows = rows(&terminal);
        assert!(
            rows[1].contains("false") && rows[1].contains("file"),
            "{:?}",
            rows[1]
        );
        assert!(rows[1].ends_with("● next start"), "{:?}", rows[1]);
        assert!(rows[10].ends_with("Running now: true."), "{:?}", rows[10]);
        assert_eq!(
            rows[11],
            "Saved: editor.hints = false. It applies when a new shell starts."
        );
    }

    #[test]
    fn ascii_when_the_terminal_has_no_unicode() {
        let mut model = model("");
        model.paste("hints");
        press(&mut model, KeyCode::Enter);
        let terminal = screen(&mut model, &styles(true, false), 80, 14);
        let rows = rows(&terminal);
        assert!(rows[1].ends_with("* next start"), "{:?}", rows[1]);
        assert!(rows.iter().all(|row| row.is_ascii()), "{rows:?}");
    }

    #[test]
    fn a_setting_the_view_does_not_change_is_dim_and_says_where_to_change_it() {
        let mut model = model("");
        model.paste("blast_radius");
        let terminal = screen(&mut model, &styles(true, true), 80, 14);
        let rows = rows(&terminal);
        assert!(rows[1].contains("blast_radius"), "{:?}", rows[1]);
        assert!(
            rows[10].ends_with("Edit config.toml to change it."),
            "{:?}",
            rows[10]
        );
        let buffer = terminal.backend().buffer();
        // The value (after the 2 + 32 columns of marker and name) is dim on a row that is not
        // chosen (the chosen row is in reverse video and has no dim text, see below).
        assert!(rows[2].contains("blast_radius"), "{:?}", rows[2]);
        assert!(buffer[(34, 2)].modifier.contains(Modifier::DIM));
        assert!(!buffer[(34, 1)].modifier.contains(Modifier::DIM));
    }

    #[test]
    fn a_failure_is_shown_in_the_danger_color() {
        let (store, _, fail) = memory("");
        *fail.lock().unwrap() = Some("the file has a syntax error".into());
        let mut model = Model::new(Config::default(), Some(Box::new(store)));
        model.paste("hints");
        press(&mut model, KeyCode::Enter);
        let terminal = screen(&mut model, &styles(true, true), 80, 14);
        assert_eq!(
            rows(&terminal)[11],
            "Not saved: the file has a syntax error"
        );
        let danger = styles(true, true).danger.fg;
        assert_eq!(terminal.backend().buffer()[(0, 11)].fg, danger.unwrap());
    }

    #[test]
    fn without_a_file_the_standing_note_says_view_only() {
        let mut model = Model::new(Config::default(), None);
        let terminal = screen(&mut model, &styles(true, true), 80, 14);
        assert_eq!(
            rows(&terminal)[11],
            "No configuration file in this shell (--no-config): view only."
        );
    }

    #[test]
    fn the_address_of_a_server_is_not_drawn() {
        let mut config = Config::default();
        config.ai.base_url = Some("http://user:secret@host".into());
        let mut model = Model::new(config, None);
        model.paste("base_url");
        let terminal = screen(&mut model, &styles(true, true), 80, 14);
        let all = rows(&terminal).join("\n");
        assert!(!all.contains("secret"));
    }

    #[test]
    fn nothing_found_says_so() {
        let mut model = model("");
        model.paste("zzzzzz");
        let terminal = screen(&mut model, &styles(true, true), 80, 14);
        assert_eq!(rows(&terminal)[1], "No such setting.");
    }

    #[test]
    fn a_small_window_only_says_so() {
        let mut model = model("");
        let terminal = screen(&mut model, &styles(true, true), 29, 14);
        assert_eq!(rows(&terminal)[0], "window too small");
        let terminal = screen(&mut model, &styles(true, true), 80, 6);
        assert_eq!(rows(&terminal)[0], "window too small");
        let terminal = screen(&mut model, &styles(true, true), 30, 7);
        assert!(rows(&terminal)[0].starts_with("Settings"));
    }

    #[test]
    fn a_narrow_screen_cuts_names_and_values_not_the_row() {
        let mut model = model("");
        model.paste("blast");
        let terminal = screen(&mut model, &styles(true, true), 40, 12);
        for row in rows(&terminal) {
            assert!(row.chars().count() <= 40, "{row:?}");
        }
    }

    #[test]
    fn on_a_narrow_screen_the_name_gives_way_and_the_sign_stays() {
        let mut model = model("");
        model.paste("hints");
        press(&mut model, KeyCode::Enter);
        let terminal = screen(&mut model, &styles(true, true), 44, 12);
        let row = rows(&terminal)[1].clone();
        assert!(row.ends_with("● next start"), "{row:?}");
        assert!(row.chars().count() <= 44, "{row:?}");
        assert!(row.starts_with("> editor.hints"), "{row:?}");
    }

    #[test]
    fn without_color_the_reverse_and_dim_attributes_stay() {
        let mut model = model("");
        let terminal = screen(&mut model, &styles(false, true), 80, 14);
        let buffer = terminal.backend().buffer();
        assert!(buffer[(0, 1)].modifier.contains(Modifier::REVERSED));
        assert_eq!(buffer[(0, 1)].fg, ratatui::style::Color::Reset);
    }

    /// Dim text on the chosen row (reverse video) is hard to read, so the chosen row has none,
    /// also the source column and the value of a setting that cannot be changed here.
    #[test]
    fn the_chosen_row_has_no_dim_text_and_the_others_keep_it() {
        for color in [true, false] {
            let mut model = model("");
            let terminal = screen(&mut model, &styles(color, true), 80, 14);
            let buffer = terminal.backend().buffer();
            assert!(buffer[(0, 1)].modifier.contains(Modifier::REVERSED));
            assert!(
                (0..80).all(|x| !buffer[(x, 1)].modifier.contains(Modifier::DIM)),
                "the chosen row has dim cells (color {color})"
            );
            // Another row has its source column in dim.
            assert!(
                (0..80).any(|x| buffer[(x, 2)].modifier.contains(Modifier::DIM)),
                "no dim cell on an unchosen row"
            );
        }
    }

    #[test]
    fn the_cursor_is_after_the_typed_text() {
        let mut model = model("");
        model.paste("ui");
        let mut terminal = screen(&mut model, &styles(true, true), 80, 14);
        let position = terminal.get_cursor_position().unwrap();
        assert_eq!((position.x, position.y), (4, 12));
    }
}
