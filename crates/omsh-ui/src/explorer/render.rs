//! Drawing the history explorer into a ratatui frame.
//!
//! ```text
//! History                                  all commands · 312 of 4,210
//!   docker compose up -d                                  (list: best match at the bottom,
//!   docker ps -a                                           older and worse ones above it)
//! > ✗   3m  docker ps -a
//! in /home/me/proj · 3m ago · status 1 · 4 runs           (the chosen command)
//! Looks at the newest 10,000 runs only; older ones are found with history search.
//! > dock                                                  (the query, with the cursor)
//! Enter use  Esc cancel  Ctrl+R scope  ↑↓ move
//! ```

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::model::{Item, Model};
pub use crate::view::draw::{MIN_HEIGHT, MIN_WIDTH, Styles, group};
use crate::view::draw::{clean, footer_line, query_row, spans_with_matches, title_row};

/// The fixed rows around the list: title, detail, note, query, footer.
const FRAME_ROWS: u16 = 5;

/// What the drawing needs besides the model.
#[derive(Clone, Debug)]
pub struct Context {
    pub styles: Styles,
    /// How many of the newest runs the list was made from (the number in the note).
    pub window: usize,
    /// The history could not be read: the empty list says so.
    pub unavailable: bool,
    pub now_ms: i64,
}

pub fn draw(frame: &mut Frame<'_>, model: &mut Model, context: &Context) {
    let area = frame.area();
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        frame.render_widget(Paragraph::new("window too small"), area);
        return;
    }
    let styles = &context.styles;
    let width = area.width;
    let list_rows = area.height - FRAME_ROWS;
    model.set_page(usize::from(list_rows));
    let row = |index: u16, height: u16| Rect::new(area.x, area.y + index, width, height);
    let title = row(0, 1);
    let list = row(1, list_rows);
    let detail = row(1 + list_rows, 1);
    let note = row(2 + list_rows, 1);
    let query = row(3 + list_rows, 1);
    let footer_area = row(4 + list_rows, 1);

    frame.render_widget(Paragraph::new(title_line(model, styles, width)), title);

    // The best match at the bottom, right above the detail line; older ones go up.
    for slot in 0..usize::from(list_rows) {
        let n = model.offset() + slot;
        let Some(item) = model.row(n) else {
            break;
        };
        let y = list.y + list_rows - 1 - u16::try_from(slot).unwrap_or(0);
        let selected = n == model.selected();
        let places = model.query().positions(item.haystack());
        let line = row_line(item, &places, selected, context, width);
        let base = if selected {
            styles.selected
        } else {
            styles.plain
        };
        frame.render_widget(
            Paragraph::new(line).style(base),
            Rect::new(area.x, y, width, 1),
        );
    }
    if model.matches() == 0 {
        let message = if context.unavailable {
            "The history cannot be read right now."
        } else if model.total() == 0 {
            "No commands in the history yet."
        } else {
            "No matching commands."
        };
        let y = list.y + list_rows - 1;
        frame.render_widget(
            Paragraph::new(Span::styled(message, styles.dim)),
            Rect::new(area.x, y, width, 1),
        );
    }

    let detail_text = model
        .current()
        .map(|item| detail_line(item, context))
        .unwrap_or_default();
    frame.render_widget(
        Paragraph::new(Span::styled(detail_text, styles.dim)),
        detail,
    );
    frame.render_widget(
        Paragraph::new(Span::styled(
            note_for_width(context.window, usize::from(width)),
            styles.dim,
        )),
        note,
    );

    let (query_line, x) = query_row(styles, model.typed(), model.cursor(), width);
    frame.render_widget(Paragraph::new(query_line), query);
    frame.render_widget(Paragraph::new(footer(styles)), footer_area);
    frame.set_cursor_position((area.x + x, query.y));
}

/// The one line that says what the list is made of. It always says it, whether or not there is
/// anything older: the limit is a fact about this view.
#[must_use]
pub fn note_text(window: usize) -> String {
    format!(
        "Looks at the newest {} runs only; older ones are found with history search.",
        group(window)
    )
}

/// The note in the longest wording that fits `width` columns; on a narrow terminal a shorter
/// wording that says the same, so that the line is never cut in the middle of a sentence.
#[must_use]
pub fn note_for_width(window: usize, width: usize) -> String {
    let full = note_text(window);
    if full.chars().count() <= width {
        return full;
    }
    format!("Newest {} runs; older: history search", group(window))
}

fn title_line(model: &Model, styles: &Styles, width: u16) -> Line<'static> {
    let right = format!(
        "{} {} {} of {}",
        model.scope().label(),
        styles.glyph("·", "-"),
        group(model.matches()),
        group(model.in_scope())
    );
    title_row(styles, "History", &right, width)
}

fn footer(styles: &Styles) -> Line<'static> {
    footer_line(
        styles,
        &[
            ("Enter", "use"),
            ("Esc", "cancel"),
            ("Ctrl+R", "scope"),
            (styles.glyph("↑↓", "Up/Down"), "move"),
        ],
    )
}

fn detail_line(item: &Item, context: &Context) -> String {
    let styles = &context.styles;
    let status = match item.status {
        Some(0) => "status 0".to_owned(),
        Some(status) => format!("status {status}"),
        None => "no status".to_owned(),
    };
    let runs = if item.runs == 1 {
        "1 run".to_owned()
    } else {
        format!(
            "{} runs",
            group(usize::try_from(item.runs).unwrap_or(usize::MAX))
        )
    };
    let separator = format!(" {} ", styles.glyph("·", "-"));
    [
        format!("in {}", clean(&item.cwd, styles)),
        ago(context.now_ms - item.started_ms),
        status,
        runs,
    ]
    .join(&separator)
}

/// A row of the list: marker, failure sign, age, then the command with its matched letters.
fn row_line(
    item: &Item,
    places: &[usize],
    selected: bool,
    context: &Context,
    width: u16,
) -> Line<'static> {
    let styles = &context.styles;
    let mark = if selected { "> " } else { "  " };
    let sign = if item.failed() {
        format!("{} ", styles.glyph("✗", "x"))
    } else {
        "  ".to_owned()
    };
    let age = format!("{:>4}  ", age(context.now_ms - item.started_ms));
    let prefix_width = mark.chars().count() + 2 + age.chars().count();
    let room = usize::from(width).saturating_sub(prefix_width);

    let mut spans = vec![
        Span::styled(mark, styles.accent),
        Span::styled(sign, styles.danger),
        Span::styled(age, styles.secondary(selected)),
    ];
    spans.extend(spans_with_matches(&item.command, places, room, styles));
    Line::from(spans)
}

/// "3m ago", or "just now" when it is under a second (never "now ago").
fn ago(milliseconds: i64) -> String {
    match age(milliseconds).as_str() {
        "now" => "just now".to_owned(),
        short => format!("{short} ago"),
    }
}

/// How long ago, in the shortest honest form: `now`, `42s`, `7m`, `3h`, `12d`, `2y`.
#[must_use]
pub fn age(milliseconds: i64) -> String {
    let seconds = (milliseconds / 1000).max(0);
    match seconds {
        0 => "now".to_owned(),
        1..=59 => format!("{seconds}s"),
        60..=3599 => format!("{}m", seconds / 60),
        3600..=86_399 => format!("{}h", seconds / 3600),
        86_400..=31_535_999 => format!("{}d", seconds / 86_400),
        _ => format!("{}y", seconds / 31_536_000),
    }
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Modifier;

    use super::*;
    use crate::look::Look;
    use crate::theme::Theme;

    const NOW: i64 = 1_000_000_000_000;

    fn context(color: bool, unicode: bool) -> Context {
        let look = Look {
            terminal: true,
            color,
            unicode,
            motion: omsh_config::Motion::Off,
        };
        Context {
            styles: Styles::new(&Theme::default(), look),
            window: 10_000,
            unavailable: false,
            now_ms: NOW,
        }
    }

    fn item(command: &str, cwd: &str, status: Option<i32>, ago_s: i64, runs: u64) -> Item {
        Item::new(command.into(), cwd.into(), status, NOW - ago_s * 1000, runs)
    }

    fn sample() -> Model {
        Model::new(
            vec![
                item("docker ps -a", "/home/me/proj", Some(1), 180, 4),
                item("git push", "/home/me/proj", Some(0), 7200, 1),
                item("ls", "/tmp", Some(0), 90_000, 2),
            ],
            "/home/me/proj".into(),
            "",
        )
    }

    /// Draws into a screen of `width` x `height` and returns the terminal.
    fn screen(
        model: &mut Model,
        context: &Context,
        width: u16,
        height: u16,
    ) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, model, context)).unwrap();
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
    fn the_layout_at_80_by_24() {
        let mut model = sample();
        let terminal = screen(&mut model, &context(true, true), 80, 24);
        let rows = rows(&terminal);
        assert_eq!(rows.len(), 24);
        assert!(rows[0].starts_with("History"));
        assert!(rows[0].ends_with("all commands · 3 of 3"), "{:?}", rows[0]);
        // The best match is at the bottom of the list, marked and with its failure sign.
        assert_eq!(rows[19], "> ✗   3m  docker ps -a");
        assert_eq!(rows[18], "      2h  git push");
        assert_eq!(rows[17], "      1d  ls");
        assert_eq!(rows[20], "in /home/me/proj · 3m ago · status 1 · 4 runs");
        assert_eq!(rows[22], ">");
        assert!(rows[23].starts_with("Enter use"), "{:?}", rows[23]);
        // Nothing above the list.
        assert!(rows[1..17].iter().all(String::is_empty));
    }

    #[test]
    fn the_note_about_the_window_is_always_there() {
        let mut model = sample();
        let terminal = screen(&mut model, &context(true, true), 80, 24);
        assert_eq!(
            rows(&terminal)[21],
            "Looks at the newest 10,000 runs only; older ones are found with history search."
        );
        // It follows the number the list was made with, and shows on a small screen too.
        let mut small = context(true, true);
        small.window = 250;
        let terminal = screen(&mut model, &small, 80, 8);
        assert!(rows(&terminal)[5].contains("newest 250 runs only"));
        assert_eq!(
            note_text(1_234_567),
            "Looks at the newest 1,234,567 runs only; older ones are found with history search."
        );
    }

    #[test]
    fn on_a_narrow_screen_the_note_is_worded_shorter_and_not_cut() {
        let mut model = sample();
        let terminal = screen(&mut model, &context(true, true), 44, 24);
        assert_eq!(
            rows(&terminal)[21],
            "Newest 10,000 runs; older: history search"
        );
        // A standard 80 columns hold the full sentence (it is 79 characters); with one column
        // less than it needs, the shorter wording is used.
        let full = note_text(10_000);
        assert_eq!(full.chars().count(), 79);
        assert_eq!(note_for_width(10_000, 79), full);
        assert_ne!(note_for_width(10_000, 78), full);
    }

    #[test]
    fn the_cursor_is_in_the_query() {
        let mut model = sample();
        model.paste("dock");
        let mut terminal = screen(&mut model, &context(true, true), 80, 24);
        let cursor = terminal.get_cursor_position().unwrap();
        assert_eq!((cursor.x, cursor.y), (6, 22));
        assert_eq!(rows(&terminal)[22], "> dock");
    }

    #[test]
    fn matched_letters_have_the_accent_color_and_the_chosen_row_is_reversed() {
        let mut model = sample();
        model.paste("dkps");
        let context = context(true, true);
        let terminal = screen(&mut model, &context, 80, 24);
        let buffer = terminal.backend().buffer();
        // "docker ps -a" starts at column 12; its letters d, k, p, s are matched.
        let d = &buffer[(10, 19)];
        assert_eq!(d.symbol(), "d");
        assert_eq!(d.fg, context.styles.accent.fg.unwrap());
        assert!(d.modifier.contains(Modifier::REVERSED));
        let o = &buffer[(11, 19)];
        assert_eq!(o.symbol(), "o");
        assert_ne!(o.fg, context.styles.accent.fg.unwrap());
        assert!(o.modifier.contains(Modifier::REVERSED));
    }

    /// Dim text on the chosen row (reverse video) is hard to read, so the chosen row has none.
    #[test]
    fn the_chosen_row_has_no_dim_text_and_the_others_keep_it() {
        for color in [true, false] {
            let mut model = sample();
            let terminal = screen(&mut model, &context(color, true), 80, 24);
            let buffer = terminal.backend().buffer();
            assert!(buffer[(12, 19)].modifier.contains(Modifier::REVERSED));
            assert!(
                (0..80).all(|x| !buffer[(x, 19)].modifier.contains(Modifier::DIM)),
                "the chosen row has dim cells (color {color})"
            );
            assert!(buffer[(5, 18)].modifier.contains(Modifier::DIM));
        }
    }

    #[test]
    fn without_color_the_attributes_stay() {
        let mut model = sample();
        let context = context(false, true);
        let terminal = screen(&mut model, &context, 80, 24);
        let buffer = terminal.backend().buffer();
        for y in 0..24 {
            for x in 0..80 {
                let cell = &buffer[(x, y)];
                assert_eq!(cell.fg, ratatui::style::Color::Reset, "color at {x},{y}");
                assert_eq!(cell.bg, ratatui::style::Color::Reset);
            }
        }
        // Reverse video still shows the chosen row, and dim still marks the age.
        assert!(buffer[(12, 19)].modifier.contains(Modifier::REVERSED));
        assert!(buffer[(5, 18)].modifier.contains(Modifier::DIM));
    }

    #[test]
    fn ascii_glyphs_are_used_when_unicode_is_off() {
        let mut model = Model::new(
            vec![
                item("echo a\nb", "/x", Some(2), 5, 1),
                item("ls", "/x", Some(0), 5, 1),
            ],
            "/x".into(),
            "",
        );
        let terminal = screen(&mut model, &context(true, false), 80, 24);
        let rows = rows(&terminal);
        assert!(rows[0].ends_with("all commands - 2 of 2"), "{:?}", rows[0]);
        assert_eq!(rows[19], "> x   5s  echo a\\nb");
        assert!(
            rows[20].contains(" - 5s ago - status 2 - "),
            "{:?}",
            rows[20]
        );
        assert!(rows[23].contains("Up/Down move"));
        assert!(rows.iter().all(|row| row.is_ascii()), "{rows:?}");
    }

    #[test]
    fn control_and_text_direction_characters_are_not_drawn() {
        let nasty = "echo \x1b[2J\x07 a\u{202E}b\u{200B}c\tz";
        let mut model = Model::new(
            vec![item(nasty, "/x\x1b[31m", Some(1), 5, 1)],
            "/x".into(),
            "",
        );
        let terminal = screen(&mut model, &context(true, true), 80, 24);
        let rows = rows(&terminal);
        assert_eq!(rows[19], "> ✗   5s  echo ?[2J? a?b?c z");
        assert!(rows[20].starts_with("in /x?[31m"), "{:?}", rows[20]);
        // Not one control character reached the screen.
        assert!(rows.iter().all(|row| !row.chars().any(char::is_control)));
    }

    #[test]
    fn a_long_command_is_cut_with_an_ellipsis_inside_the_screen() {
        let long = "x".repeat(200);
        let mut model = Model::new(vec![item(&long, "/x", Some(0), 5, 1)], "/x".into(), "");
        let terminal = screen(&mut model, &context(true, true), 60, 24);
        let row = &rows(&terminal)[19];
        assert_eq!(row.chars().count(), 60);
        assert!(row.ends_with("x…"), "{row:?}");
    }

    #[test]
    fn wide_characters_are_counted_by_their_width() {
        let wide = "界".repeat(100);
        let mut model = Model::new(vec![item(&wide, "/x", Some(0), 5, 1)], "/x".into(), "");
        let terminal = screen(&mut model, &context(true, true), 60, 24);
        let buffer = terminal.backend().buffer();
        // The row ends with the ellipsis, right after the last whole character that fits (a wide
        // character is never cut in half), and never runs past the last column.
        assert_eq!(buffer[(58, 19)].symbol(), "…");
        assert_eq!(buffer[(59, 19)].symbol(), " ");
    }

    #[test]
    fn the_empty_list_says_why() {
        let mut empty = Model::new(Vec::new(), "/x".into(), "");
        let terminal = screen(&mut empty, &context(true, true), 80, 24);
        assert_eq!(rows(&terminal)[19], "No commands in the history yet.");
        let mut unreadable = context(true, true);
        unreadable.unavailable = true;
        let terminal = screen(&mut empty, &unreadable, 80, 24);
        assert_eq!(rows(&terminal)[19], "The history cannot be read right now.");
        let mut model = sample();
        model.paste("zzzz");
        let terminal = screen(&mut model, &context(true, true), 80, 24);
        assert_eq!(rows(&terminal)[19], "No matching commands.");
    }

    #[test]
    fn a_window_that_is_too_small_only_says_so() {
        let mut model = sample();
        for (width, height) in [(29, 24), (80, 6), (10, 3)] {
            let terminal = screen(&mut model, &context(true, true), width, height);
            let rows = rows(&terminal);
            let expected: String = "window too small"
                .chars()
                .take(usize::from(width))
                .collect();
            assert_eq!(rows[0], expected);
            assert!(rows[1..].iter().all(String::is_empty), "{rows:?}");
        }
    }

    #[test]
    fn scrolling_follows_the_selection() {
        let items: Vec<Item> = (0..30)
            .map(|n| item(&format!("cmd-{n:02}"), "/x", Some(0), 5, 1))
            .collect();
        let mut model = Model::new(items, "/x".into(), "");
        // 80x12: five fixed rows leave seven for the list.
        for _ in 0..20 {
            model.handle_key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Up,
                crossterm::event::KeyModifiers::NONE,
            ));
        }
        let terminal = screen(&mut model, &context(true, true), 80, 12);
        let rows = rows(&terminal);
        // The chosen row (the 21st best) is at the top of the seven rows shown, and the
        // window starts at the 15th best at the bottom.
        assert!(
            rows[1].starts_with("> ") && rows[1].ends_with("cmd-20"),
            "{:?}",
            rows[1]
        );
        assert!(rows[7].ends_with("cmd-14"), "{:?}", rows[7]);
    }

    #[test]
    fn the_detail_line_never_says_now_ago() {
        assert_eq!(ago(100), "just now");
        assert_eq!(ago(-5), "just now");
        assert_eq!(ago(42_000), "42s ago");
        assert_eq!(ago(180_000), "3m ago");
    }

    #[test]
    fn ages_and_counts() {
        assert_eq!(age(-5), "now");
        assert_eq!(age(400), "now");
        assert_eq!(age(42_000), "42s");
        assert_eq!(age(60_000), "1m");
        assert_eq!(age(3_599_000), "59m");
        assert_eq!(age(3_600_000), "1h");
        assert_eq!(age(86_400_000), "1d");
        assert_eq!(age(400 * 86_400_000), "1y");
        assert_eq!(group(0), "0");
        assert_eq!(group(999), "999");
        assert_eq!(group(1000), "1,000");
        assert_eq!(group(4210), "4,210");
        assert_eq!(group(1_234_567), "1,234,567");
    }
}
