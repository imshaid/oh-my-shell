//! Styles and drawing helpers shared by the full-screen views.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::look::Look;
use crate::style::style;
use crate::theme::{Role, Theme, Token};

/// Narrower or lower than this and a view only says so.
pub const MIN_WIDTH: u16 = 30;
pub const MIN_HEIGHT: u16 = 7;

/// How a view looks: colors from the theme, glyphs from the look.
#[derive(Clone, Debug)]
pub struct Styles {
    pub plain: Style,
    pub dim: Style,
    pub accent: Style,
    pub danger: Style,
    pub warning: Style,
    pub selected: Style,
    pub unicode: bool,
}

impl Styles {
    /// Without color in `look`, colors are dropped and the attributes (reverse, dim) stay.
    #[must_use]
    pub fn new(theme: &Theme, look: Look) -> Self {
        let theme = if look.color {
            theme.clone()
        } else {
            theme.without_color()
        };
        Self {
            plain: Style::default(),
            dim: style(theme.token(Token::Dim)),
            accent: style(theme.token(Token::Accent)),
            danger: style(theme.token(Token::Danger)),
            warning: style(theme.token(Token::Warning)),
            selected: style(theme.get(Role::Selection)),
            unicode: look.unicode,
        }
    }

    /// `unicode` with a terminal that takes it, `ascii` otherwise.
    #[must_use]
    pub fn glyph(&self, unicode: &'static str, ascii: &'static str) -> &'static str {
        if self.unicode { unicode } else { ascii }
    }
}

/// The keys line at the bottom: `Enter use  Esc cancel ...`, keys in the accent style.
#[must_use]
pub fn footer_line(styles: &Styles, keys: &[(&str, &str)]) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, (key, what)) in keys.iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled((*key).to_owned(), styles.accent));
        spans.push(Span::styled(format!(" {what}"), styles.dim));
    }
    Line::from(spans)
}

/// The title row: `left` in the accent style, `right` dim and pushed to the right edge.
#[must_use]
pub fn title_row(styles: &Styles, left: &str, right: &str, width: u16) -> Line<'static> {
    let used = left.chars().count() + right.chars().count();
    let pad = usize::from(width).saturating_sub(used).max(1);
    Line::from(vec![
        Span::styled(left.to_owned(), styles.accent),
        Span::raw(" ".repeat(pad)),
        Span::styled(right.to_owned(), styles.dim),
    ])
}

/// `text` as spans: control and invisible characters made harmless, the characters at `places`
/// (indexes into `text`, sorted) in the accent style, cut with an ellipsis if it does not fit in
/// `room` columns.
#[must_use]
pub fn spans_with_matches(
    text: &str,
    places: &[usize],
    room: usize,
    styles: &Styles,
) -> Vec<Span<'static>> {
    let shown: Vec<(String, bool)> = text
        .chars()
        .enumerate()
        .map(|(index, c)| (show(c, styles), places.binary_search(&index).is_ok()))
        .collect();
    let total: usize = shown
        .iter()
        .map(|(text, _)| Span::raw(text.clone()).width())
        .sum();
    let ellipsis = styles.glyph("…", "~");
    let (limit, cut) = if total <= room {
        (room, false)
    } else {
        (room.saturating_sub(1), true)
    };

    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    let mut run_matched = false;
    let mut used = 0;
    let flush = |spans: &mut Vec<Span<'static>>, run: &mut String, matched: bool| {
        if !run.is_empty() {
            let look = if matched { styles.accent } else { styles.plain };
            spans.push(Span::styled(std::mem::take(run), look));
        }
    };
    for (text, matched) in &shown {
        let width = Span::raw(text.clone()).width();
        if used + width > limit {
            break;
        }
        if *matched != run_matched {
            flush(&mut spans, &mut run, run_matched);
            run_matched = *matched;
        }
        run.push_str(text);
        used += width;
    }
    flush(&mut spans, &mut run, run_matched);
    if cut {
        spans.push(Span::styled(ellipsis, styles.dim));
    }
    spans
}

/// What is drawn for one character of a stored text: itself, unless it could change what the
/// person reads (control characters, line ends, text-direction and zero-width characters).
#[must_use]
pub fn show(c: char, styles: &Styles) -> String {
    match c {
        '\n' => styles.glyph("↵", "\\n").to_owned(),
        '\t' => " ".to_owned(),
        c if c.is_control() || is_invisible(c) => "?".to_owned(),
        c => c.to_string(),
    }
}

fn is_invisible(c: char) -> bool {
    matches!(c,
        '\u{061C}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}'
        | '\u{2066}'..='\u{2069}' | '\u{FEFF}')
}

/// A text for one line: every character made harmless.
#[must_use]
pub fn clean(text: &str, styles: &Styles) -> String {
    text.chars().map(|c| show(c, styles)).collect()
}

/// `4210` as `4,210`.
#[must_use]
pub fn group(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (index, c) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The query row: `> text`, and the column the cursor goes to (a count of columns from the left
/// edge of the row), after `cursor` characters of `typed`.
#[must_use]
pub fn query_row(styles: &Styles, typed: &str, cursor: usize, width: u16) -> (Line<'static>, u16) {
    let line = Line::from(vec![
        Span::styled("> ", styles.accent),
        Span::raw(clean(typed, styles)),
    ]);
    let before: String = clean(&typed.chars().take(cursor).collect::<String>(), styles);
    let x = (2 + Span::raw(before).width()).min(usize::from(width.saturating_sub(1)));
    (line, u16::try_from(x).unwrap_or(0))
}

/// `text` made harmless and cut to `room` columns, with an ellipsis when it was cut.
#[must_use]
pub fn fit_text(text: &str, room: usize, styles: &Styles) -> String {
    let shown = clean(text, styles);
    if Span::raw(shown.clone()).width() <= room {
        return shown;
    }
    let ellipsis = styles.glyph("…", "~");
    let mut out = String::new();
    let mut used = 0;
    for c in shown.chars() {
        let width = Span::raw(c.to_string()).width();
        if used + width + 1 > room {
            break;
        }
        out.push(c);
        used += width;
    }
    if room > 0 {
        out.push_str(ellipsis);
    }
    out
}
