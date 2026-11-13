//! Drawing the command palette into a ratatui frame.
//!
//! ```text
//! Command palette                                          14 of 58
//! > action   Settings                                              (the list, best match first)
//!   action   History explorer                                Ctrl+R
//!   alias    gs   git status
//! Opens the settings view.                                         (what the chosen entry does)
//! > se                                                             (the query, with the cursor)
//! Enter use  Esc cancel  ↑↓ move
//! ```

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::model::{Effect, Entry, Model};
use crate::view::draw::{
    MIN_HEIGHT, MIN_WIDTH, Styles, fit_text, footer_line, group, query_row, spans_with_matches,
    title_row,
};

/// The fixed rows around the list: title, detail, query, footer.
const FRAME_ROWS: u16 = 4;

/// The width of the tag column: the longest tag and a blank.
const TAG_WIDTH: usize = 8;

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
        Paragraph::new(title_row(styles, "Command palette", &right, width)),
        row(0, 1),
    );

    for slot in 0..usize::from(list_rows) {
        let n = model.offset() + slot;
        let Some(entry) = model.row(n) else {
            break;
        };
        let selected = n == model.selected();
        let places = model.query().positions(entry.haystack());
        let line = entry_line(entry, &places, selected, styles, width);
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
            Paragraph::new(Span::styled("Nothing matches.", styles.dim)),
            row(1, 1),
        );
    }

    let detail = model
        .current()
        .map(|entry| what_it_does(entry, styles))
        .unwrap_or_default();
    frame.render_widget(
        Paragraph::new(Span::styled(
            fit_text(&detail, usize::from(width), styles),
            styles.dim,
        )),
        row(1 + list_rows, 1),
    );
    let (query_line, x) = query_row(styles, model.typed(), model.cursor(), width);
    frame.render_widget(Paragraph::new(query_line), row(2 + list_rows, 1));
    frame.render_widget(
        Paragraph::new(footer_line(
            styles,
            &[
                ("Enter", "use"),
                ("Esc", "cancel"),
                (styles.glyph("↑↓", "Up/Down"), "move"),
            ],
        )),
        row(3 + list_rows, 1),
    );
    frame.set_cursor_position((area.x + x, area.y + 2 + list_rows));
}

/// What choosing the entry does, in a sentence.
fn what_it_does(entry: &Entry, styles: &Styles) -> String {
    match &entry.effect {
        Effect::Insert(text) => format!(
            "Puts {}{}{} in the line; nothing runs.",
            styles.glyph("“", "\""),
            crate::view::draw::clean(text, styles),
            styles.glyph("”", "\"")
        ),
        Effect::OpenSettings | Effect::OpenHistory | Effect::ExplainLast => entry.detail.clone(),
    }
}

/// A row: marker, tag, name with its matched letters, detail, and the key on the right.
fn entry_line(
    entry: &Entry,
    places: &[usize],
    selected: bool,
    styles: &Styles,
    width: u16,
) -> Line<'static> {
    let mark = if selected { "> " } else { "  " };
    let tag = format!("{:<TAG_WIDTH$}", entry.kind.tag());
    let key = entry.key.as_deref().unwrap_or_default();
    let key_width = key.chars().count();
    let fixed = mark.chars().count() + TAG_WIDTH;
    // A gap before the key, if there is one.
    let room = usize::from(width)
        .saturating_sub(fixed)
        .saturating_sub(if key_width > 0 { key_width + 2 } else { 0 });

    let name_places: Vec<usize> = places
        .iter()
        .copied()
        .filter(|place| *place < entry.name_len())
        .collect();
    let name_room = room.min(entry.name_len().max(1));
    let name_spans = spans_with_matches(&entry.name, &name_places, name_room, styles);
    let name_width: usize = name_spans.iter().map(Span::width).sum();

    let mut spans = vec![
        Span::styled(mark, styles.accent),
        Span::styled(tag, styles.secondary(selected)),
    ];
    spans.extend(name_spans);
    let mut used = name_width;
    let detail_room = room.saturating_sub(used + 2);
    if detail_room > 3 && !entry.detail.is_empty() {
        let detail = fit_text(&entry.detail, detail_room, styles);
        used += 2 + Span::raw(detail.clone()).width();
        spans.push(Span::raw("  "));
        spans.push(Span::styled(detail, styles.secondary(selected)));
    }
    if key_width > 0 {
        let pad = room.saturating_sub(used) + 2;
        spans.push(Span::raw(" ".repeat(pad)));
        spans.push(Span::styled(key.to_owned(), styles.accent));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Modifier;

    use super::*;
    use crate::look::Look;
    use crate::palette::model::Kind;
    use crate::theme::Theme;

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

    fn sample() -> Model {
        Model::new(vec![
            Entry::new(
                Kind::Action,
                "Settings",
                "Open the settings view.",
                None,
                Effect::OpenSettings,
            ),
            Entry::new(
                Kind::Action,
                "History explorer",
                "Search every command.",
                Some("Ctrl+R"),
                Effect::OpenHistory,
            ),
            Entry::new(
                Kind::Alias,
                "gs",
                "git status",
                None,
                Effect::Insert("gs".into()),
            ),
            Entry::new(
                Kind::Builtin,
                "cd",
                "Change the current folder.",
                None,
                Effect::Insert("cd".into()),
            ),
        ])
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

    #[test]
    fn the_layout_at_80_by_12() {
        let mut model = sample();
        let terminal = screen(&mut model, &styles(true, true), 80, 12);
        let rows = rows(&terminal);
        assert_eq!(rows.len(), 12);
        assert!(rows[0].starts_with("Command palette"));
        assert!(rows[0].ends_with("4 of 4"), "{:?}", rows[0]);
        assert_eq!(rows[1], "> action  Settings  Open the settings view.");
        assert!(rows[2].starts_with("  action  History explorer  Search every command."));
        assert!(rows[2].ends_with("Ctrl+R"), "{:?}", rows[2]);
        assert_eq!(rows[3], "  alias   gs  git status");
        assert_eq!(rows[4], "  builtin cd  Change the current folder.");
        // What choosing it does, the query and the keys.
        assert_eq!(rows[9], "Open the settings view.");
        assert_eq!(rows[10], ">");
        assert_eq!(rows[11], "Enter use  Esc cancel  ↑↓ move");
    }

    #[test]
    fn an_entry_that_inserts_says_so() {
        let mut model = sample();
        for _ in 0..2 {
            model.handle_key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Down,
                crossterm::event::KeyModifiers::NONE,
            ));
        }
        let terminal = screen(&mut model, &styles(true, true), 80, 12);
        assert_eq!(rows(&terminal)[9], "Puts “gs” in the line; nothing runs.");
    }

    #[test]
    fn ascii_when_the_terminal_has_no_unicode() {
        let mut model = sample();
        model.paste("gs");
        let terminal = screen(&mut model, &styles(true, false), 80, 12);
        let rows = rows(&terminal);
        assert_eq!(rows[9], "Puts \"gs\" in the line; nothing runs.");
        assert_eq!(rows[11], "Enter use  Esc cancel  Up/Down move");
        assert!(rows.iter().all(|row| row.is_ascii()), "{rows:?}");
    }

    #[test]
    fn matched_letters_are_in_the_accent_color_and_the_chosen_row_is_reversed() {
        let mut model = sample();
        model.paste("hist");
        let terminal = screen(&mut model, &styles(true, true), 80, 12);
        let buffer = terminal.backend().buffer();
        // "History explorer": the H of the name is a matched letter on the chosen row.
        let first = &buffer[(10, 1)];
        assert_eq!(first.symbol(), "H");
        assert!(first.modifier.contains(Modifier::REVERSED));
        assert_ne!(first.fg, ratatui::style::Color::Reset);
    }

    #[test]
    fn without_color_the_reverse_and_dim_attributes_stay() {
        let mut model = sample();
        let terminal = screen(&mut model, &styles(false, true), 80, 12);
        let buffer = terminal.backend().buffer();
        assert!(buffer[(0, 1)].modifier.contains(Modifier::REVERSED));
        assert_eq!(buffer[(0, 1)].fg, ratatui::style::Color::Reset);
        // The tag of an unchosen row is dim.
        assert!(buffer[(2, 3)].modifier.contains(Modifier::DIM));
    }

    /// Dim text on the chosen row (reverse video) is hard to read, so the chosen row has none.
    #[test]
    fn the_chosen_row_has_no_dim_text_and_the_others_keep_it() {
        for color in [true, false] {
            let mut model = sample();
            let terminal = screen(&mut model, &styles(color, true), 80, 12);
            let buffer = terminal.backend().buffer();
            assert!(buffer[(0, 1)].modifier.contains(Modifier::REVERSED));
            assert!(
                (0..80).all(|x| !buffer[(x, 1)].modifier.contains(Modifier::DIM)),
                "the chosen row has dim cells (color {color})"
            );
            assert!(buffer[(2, 3)].modifier.contains(Modifier::DIM));
        }
    }

    #[test]
    fn nothing_matches_says_so() {
        let mut model = sample();
        model.paste("zzzz");
        let terminal = screen(&mut model, &styles(true, true), 80, 12);
        let rows = rows(&terminal);
        assert_eq!(rows[1], "Nothing matches.");
        assert!(rows[0].ends_with("0 of 4"));
    }

    #[test]
    fn a_small_window_only_says_so() {
        let mut model = sample();
        let terminal = screen(&mut model, &styles(true, true), 29, 12);
        assert_eq!(rows(&terminal)[0], "window too small");
        let terminal = screen(&mut model, &styles(true, true), 80, 6);
        assert_eq!(rows(&terminal)[0], "window too small");
        let terminal = screen(&mut model, &styles(true, true), 30, 7);
        assert!(rows(&terminal)[0].starts_with("Command palette"));
    }

    #[test]
    fn a_long_detail_is_cut_with_an_ellipsis_and_the_key_stays_on_the_right() {
        let mut model = Model::new(vec![Entry::new(
            Kind::Action,
            "History explorer",
            "A very long description that cannot possibly fit on this narrow screen at all",
            Some("Ctrl+R"),
            Effect::OpenHistory,
        )]);
        let terminal = screen(&mut model, &styles(true, true), 50, 8);
        let row = rows(&terminal)[1].clone();
        assert!(
            row.ends_with("…  Ctrl+R") || row.contains("…") && row.ends_with("Ctrl+R"),
            "{row:?}"
        );
        assert!(row.chars().count() <= 50);
    }

    #[test]
    fn stored_control_and_direction_characters_are_not_drawn() {
        let mut model = Model::new(vec![Entry::new(
            Kind::Alias,
            "x\u{202e}y",
            "a\x1b[31mb\nc",
            None,
            Effect::Insert("x\u{202e}y\x1b".into()),
        )]);
        let terminal = screen(&mut model, &styles(true, true), 80, 8);
        let all = rows(&terminal).join("\n");
        assert!(!all.contains('\u{202e}') && !all.contains('\x1b'));
        assert!(all.contains("x?y") && all.contains("a?[31mb↵c"), "{all}");
    }

    #[test]
    fn the_cursor_is_after_the_typed_text() {
        let mut model = sample();
        model.paste("se");
        let mut terminal = screen(&mut model, &styles(true, true), 80, 12);
        let position = terminal.get_cursor_position().unwrap();
        assert_eq!((position.x, position.y), (4, 10));
    }
}
