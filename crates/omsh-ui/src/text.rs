//! Styled text and the one place that turns it into escape sequences (UI upgrade, U1).
//!
//! Panels are built as data: a [`Line`] is a list of [`Span`]s, and a span is text with a
//! semantic [`Style`] (`Danger`, `Ok`, `Key`, ...). Nothing in a panel says "red". [`render_line`]
//! looks the style up in the [`Theme`] (which holds only the 16 ANSI color names) and writes the
//! SGR sequence. So a panel can be tested as plain data, and "no RGB, no 256-color" is true by
//! construction: this file is the only writer of escapes and the theme has no such color.
//!
//! **User text is made harmless when a span is made** ([`harmless`]): a file name that contains
//! `ESC` must not be able to move the cursor or recolor the panel, and a right-to-left override
//! must not reorder what the person reads. The zero-width joiners that Bangla uses are kept.
//!
//! Widths are display columns ([`width`], [`fit`]), measured with `unicode-width`. Terminals do
//! not agree on every Bangla conjunct, which is why panels that show user text have no right
//! edge to line up (see [`crate::boxes`]).

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::caps::Caps;
use crate::theme::{RoleStyle, Theme, Token};

const RESET: &str = "\x1b[0m";

/// What a piece of text means. Panel code uses these; only [`render_line`] knows colors.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Tone {
    /// The terminal's own text.
    #[default]
    Plain,
    /// Danger, errors (red).
    Danger,
    /// Done, safe (green).
    Ok,
    /// Caution (yellow).
    Warn,
    /// Anything that has to do with the AI (magenta).
    Ai,
    /// A key a person can press (cyan).
    Key,
    /// Information, neutral borders (blue).
    Info,
    /// A path (cyan).
    Path,
    /// Hints and secondary text (dim).
    Dim,
}

impl Tone {
    /// Every tone.
    pub const ALL: [Tone; 9] = [
        Tone::Plain,
        Tone::Danger,
        Tone::Ok,
        Tone::Warn,
        Tone::Ai,
        Tone::Key,
        Tone::Info,
        Tone::Path,
        Tone::Dim,
    ];

    /// The theme token that gives this tone its look; `None` for plain text.
    #[must_use]
    pub fn token(self) -> Option<Token> {
        match self {
            Tone::Plain => None,
            Tone::Danger => Some(Token::Danger),
            Tone::Ok => Some(Token::Success),
            Tone::Warn => Some(Token::Warning),
            Tone::Ai => Some(Token::Ai),
            Tone::Key => Some(Token::Key),
            Tone::Info => Some(Token::Accent),
            Tone::Path => Some(Token::Path),
            Tone::Dim => Some(Token::Dim),
        }
    }
}

/// A tone plus the two attributes a widget adds: selection (reverse video) and underline.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Style {
    /// What the text means.
    pub tone: Tone,
    /// Reverse video: the chosen button or row.
    pub selected: bool,
    /// Underline.
    pub underline: bool,
}

impl Style {
    /// Plain text.
    pub const PLAIN: Style = Style {
        tone: Tone::Plain,
        selected: false,
        underline: false,
    };

    /// A style of this tone.
    #[must_use]
    pub const fn of(tone: Tone) -> Self {
        Style {
            tone,
            selected: false,
            underline: false,
        }
    }

    /// The same, reversed.
    #[must_use]
    pub const fn selected(self) -> Self {
        Style {
            selected: true,
            ..self
        }
    }

    /// The same, underlined.
    #[must_use]
    pub const fn underlined(self) -> Self {
        Style {
            underline: true,
            ..self
        }
    }
}

/// Text in one style. The text has no control characters (see [`harmless`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    text: String,
    /// How it looks.
    pub style: Style,
}

impl Span {
    /// A span of `text`, made harmless.
    #[must_use]
    pub fn new(text: &str, style: Style) -> Self {
        Span {
            text: harmless(text),
            style,
        }
    }

    /// The text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// One line of a panel: spans, no line break.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Line {
    spans: Vec<Span>,
}

impl Line {
    /// An empty line.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A line of plain text.
    #[must_use]
    pub fn plain(text: &str) -> Self {
        Self::new().then(text, Style::PLAIN)
    }

    /// This line with `text` added in `style`.
    #[must_use]
    pub fn then(mut self, text: &str, style: Style) -> Self {
        self.push(text, style);
        self
    }

    /// This line with `text` added in `tone`.
    #[must_use]
    pub fn tone(self, tone: Tone, text: &str) -> Self {
        self.then(text, Style::of(tone))
    }

    /// Adds `text` in `style`. Empty text adds nothing, and text in the style of the last span
    /// joins it (so that one color is written once, not once for each character).
    pub fn push(&mut self, text: &str, style: Style) {
        if text.is_empty() {
            return;
        }
        let text = harmless(text);
        match self.spans.last_mut() {
            Some(last) if last.style == style => last.text.push_str(&text),
            _ => self.spans.push(Span { text, style }),
        }
    }

    /// Adds every span of `other`.
    pub fn append(&mut self, other: &Line) {
        for span in &other.spans {
            self.push(&span.text, span.style);
        }
    }

    /// The spans.
    #[must_use]
    pub fn spans(&self) -> &[Span] {
        &self.spans
    }

    /// True if there is no text.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }

    /// The text without any style: what a pipe sees.
    #[must_use]
    pub fn plain_text(&self) -> String {
        self.spans.iter().map(|span| span.text.as_str()).collect()
    }

    /// Display columns.
    #[must_use]
    pub fn width(&self) -> usize {
        self.spans.iter().map(|span| width(&span.text)).sum()
    }

    /// This line with blanks added at the end to be `cols` wide. Unchanged if it is wider.
    #[must_use]
    pub fn padded_to(mut self, cols: usize) -> Self {
        let now = self.width();
        if now < cols {
            self.push(&" ".repeat(cols - now), Style::PLAIN);
        }
        self
    }

    /// This line cut to at most `cols` columns, with an ellipsis (`…`, or `...` in ASCII) at the
    /// cut. The ellipsis has the style of the span that was cut. Unchanged if it fits.
    #[must_use]
    pub fn fit(&self, cols: usize, unicode: bool) -> Line {
        if self.width() <= cols {
            return self.clone();
        }
        let ellipsis = ellipsis(unicode);
        let ellipsis_width = width(ellipsis);
        if cols < ellipsis_width {
            // No room for a mark: cut hard.
            return self.cut(cols, None);
        }
        self.cut(cols - ellipsis_width, Some(ellipsis))
    }

    fn cut(&self, room: usize, mark: Option<&str>) -> Line {
        let mut out = Line::new();
        let mut left = room;
        let mut last_style = Style::PLAIN;
        for span in &self.spans {
            last_style = span.style;
            let span_width = width(&span.text);
            if span_width <= left {
                out.spans.push(span.clone());
                left -= span_width;
                continue;
            }
            let part = cut_to(&span.text, left);
            out.push(&part, span.style);
            break;
        }
        if let Some(mark) = mark {
            out.push(mark, last_style);
        }
        out
    }
}

/// The mark at a cut.
#[must_use]
pub fn ellipsis(unicode: bool) -> &'static str {
    if unicode { "…" } else { "..." }
}

/// Display columns of `text`.
#[must_use]
pub fn width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

/// `text` cut to at most `cols` columns, with an ellipsis at the cut. Unchanged if it fits.
#[must_use]
pub fn fit(text: &str, cols: usize, unicode: bool) -> String {
    Line::plain(text).fit(cols, unicode).plain_text()
}

/// The longest start of `text` that is at most `room` columns wide. Characters of no width
/// (combining marks) that follow the last one that fits are kept with it.
fn cut_to(text: &str, room: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = UnicodeWidthChar::width(c).unwrap_or(0);
        if used + w > room {
            break;
        }
        used += w;
        out.push(c);
    }
    // `str` width is not always the sum of the characters (some sequences join): never end up
    // wider than asked.
    while width(&out) > room {
        out.pop();
    }
    // A cut right after a virama or a joiner would leave a letter that asks for a next one
    // (`ক্` instead of `ক`).
    while out.chars().last().is_some_and(asks_for_a_next_letter) {
        out.pop();
    }
    out
}

/// A zero-width joiner or non-joiner, or the virama (halant) of an Indic script: all of them
/// join the letter before them to the letter after them.
fn asks_for_a_next_letter(c: char) -> bool {
    matches!(
        c,
        '\u{200C}' | '\u{200D}'
        | '\u{094D}' // devanagari
        | '\u{09CD}' // bengali
        | '\u{0A4D}' // gurmukhi
        | '\u{0ACD}' // gujarati
        | '\u{0B4D}' // odia
        | '\u{0BCD}' // tamil
        | '\u{0C4D}' // telugu
        | '\u{0CCD}' // kannada
        | '\u{0D4D}' // malayalam
    )
}

/// `text` with everything removed that could act on the terminal or reorder what is read:
/// control characters (including `ESC`, tab, newline and the C1 range) and the bidirectional
/// and direction marks. Each becomes `?`. Zero-width joiners and non-joiners stay: Bangla needs
/// them.
#[must_use]
pub fn harmless(text: &str) -> String {
    text.chars()
        .map(|c| if is_unsafe(c) { '?' } else { c })
        .collect()
}

fn is_unsafe(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{061C}' // arabic letter mark
            | '\u{200E}' | '\u{200F}' // left-to-right and right-to-left marks
            | '\u{202A}'..='\u{202E}' // embeddings and overrides
            | '\u{2066}'..='\u{2069}' // isolates
        )
}

/// The look of `style` under `theme` and `caps`: the tone's token, its color dropped when colors
/// are off (attributes stay: a dim hint is still dim under `NO_COLOR`), plus reverse and
/// underline.
#[must_use]
pub fn role_style(style: Style, theme: &Theme, caps: &Caps) -> RoleStyle {
    let mut look = style
        .tone
        .token()
        .map_or(RoleStyle::PLAIN, |token| theme.token(token));
    if !caps.color {
        look = look.without_color();
    }
    look.reverse |= style.selected;
    look.underline |= style.underline;
    look
}

/// Writes `line` for `caps`. When the output is not a terminal, or is `dumb`, the result is the
/// text alone. Otherwise each span is wrapped in its SGR sequence and a reset.
#[must_use]
pub fn render_line(line: &Line, theme: &Theme, caps: &Caps) -> String {
    if !(caps.out_tty && !caps.dumb) {
        return line.plain_text();
    }
    let mut out = String::new();
    for span in line.spans() {
        match role_style(span.style, theme, caps).sgr_parameters() {
            Some(parameters) => {
                out.push_str("\x1b[");
                out.push_str(&parameters);
                out.push('m');
                out.push_str(&span.text);
                out.push_str(RESET);
            }
            None => out.push_str(&span.text),
        }
    }
    out
}

/// Writes every line, one string each, with no line break.
#[must_use]
pub fn render(lines: &[Line], theme: &Theme, caps: &Caps) -> Vec<String> {
    lines
        .iter()
        .map(|line| render_line(line, theme, caps))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caps::{CapsFacts, Out};
    use omsh_config::Icons;

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

    fn caps(facts: CapsFacts) -> Caps {
        Caps::decide(facts, Out::Stderr, Icons::Auto, None)
    }

    fn theme() -> Theme {
        Theme::default()
    }

    #[test]
    fn each_tone_has_the_token_the_spec_names() {
        let color = |tone: Tone| tone.token().and_then(|t| theme().token(t).color);
        use crate::theme::AnsiColor::*;
        assert_eq!(color(Tone::Danger), Some(Red));
        assert_eq!(color(Tone::Ok), Some(Green));
        assert_eq!(color(Tone::Warn), Some(Yellow));
        assert_eq!(color(Tone::Key), Some(Cyan));
        assert_eq!(color(Tone::Ai), Some(Magenta));
        assert_eq!(color(Tone::Info), Some(Blue));
        assert_eq!(color(Tone::Plain), None);
    }

    #[test]
    fn a_tone_is_wrapped_in_its_color() {
        let line = Line::new()
            .tone(Tone::Danger, "bad")
            .tone(Tone::Plain, " ok");
        assert_eq!(
            render_line(&line, &theme(), &caps(GOOD)),
            "\x1b[31mbad\x1b[0m ok"
        );
    }

    #[test]
    fn selection_is_reverse_video_and_underline_is_added() {
        let line = Line::new()
            .then("pick", Style::of(Tone::Key).selected())
            .then("path", Style::of(Tone::Path).underlined());
        assert_eq!(
            render_line(&line, &theme(), &caps(GOOD)),
            "\x1b[7;36mpick\x1b[0m\x1b[4;36mpath\x1b[0m"
        );
    }

    #[test]
    fn no_color_drops_the_color_and_keeps_dim_reverse_and_underline() {
        let c = caps(CapsFacts {
            no_color: true,
            ..GOOD
        });
        let line = Line::new()
            .tone(Tone::Danger, "bad")
            .tone(Tone::Dim, "hint")
            .then("sel", Style::of(Tone::Key).selected())
            .then("under", Style::PLAIN.underlined());
        assert_eq!(
            render_line(&line, &theme(), &c),
            "bad\x1b[2mhint\x1b[0m\x1b[7msel\x1b[0m\x1b[4munder\x1b[0m"
        );
    }

    #[test]
    fn a_pipe_and_a_dumb_terminal_get_text_only() {
        let line = Line::new()
            .tone(Tone::Danger, "bad")
            .then("sel", Style::PLAIN.selected());
        for facts in [
            CapsFacts {
                stderr_tty: false,
                ..GOOD
            },
            CapsFacts { dumb: true, ..GOOD },
        ] {
            assert_eq!(render_line(&line, &theme(), &caps(facts)), "badsel");
        }
    }

    #[test]
    fn a_custom_theme_changes_the_bytes_and_still_only_ansi_colors() {
        let mut custom = theme();
        custom.set_token(Token::Key, RoleStyle::color(crate::AnsiColor::BrightYellow));
        let line = Line::new().tone(Tone::Key, "k");
        assert_eq!(render_line(&line, &custom, &caps(GOOD)), "\x1b[93mk\x1b[0m");
    }

    #[test]
    fn neighbouring_text_in_one_style_is_one_span_and_one_escape() {
        let mut line = Line::new();
        for c in ["C", "a", "n", "c", "e", "l"] {
            line.push(c, Style::PLAIN.selected());
        }
        line.push("!", Style::of(Tone::Danger));
        line.push("?", Style::PLAIN.selected());
        assert_eq!(line.spans().len(), 3);
        assert_eq!(line.plain_text(), "Cancel!?");
        let mut joined = Line::plain("a");
        joined.append(&Line::plain("b").tone(Tone::Key, "c"));
        assert_eq!(joined.spans().len(), 2, "ab, then c");
        assert_eq!(
            render_line(&line, &theme(), &caps(GOOD)),
            "\x1b[7mCancel\x1b[0m\x1b[31m!\x1b[0m\x1b[7m?\x1b[0m"
        );
    }

    #[test]
    fn empty_text_makes_no_span() {
        let line = Line::new().tone(Tone::Danger, "");
        assert!(line.is_empty());
        assert_eq!(render_line(&line, &theme(), &caps(GOOD)), "");
    }

    #[test]
    fn user_text_cannot_carry_an_escape_or_a_reordering_mark() {
        let nasty = "a\x1b[31mb\rc\nd\te\u{9b}f\u{202e}g\u{2066}h\u{200f}i\u{7f}j";
        let clean = harmless(nasty);
        assert_eq!(clean, "a?[31mb?c?d?e?f?g?h?i?j");
        let line = Line::new().tone(Tone::Path, nasty);
        let written = render_line(&line, &theme(), &caps(GOOD));
        assert_eq!(
            written.matches('\x1b').count(),
            2,
            "only our own two: {written:?}"
        );
    }

    #[test]
    fn the_joiners_that_bangla_needs_are_kept() {
        let text = "র\u{200d}্যাব \u{200c}x";
        assert_eq!(harmless(text), text);
    }

    // What `unicode-width` says about Bangla. Terminals do not all agree (a conjunct may be drawn
    // as one cell or two), which is why panels that show user text have no right edge.
    #[test]
    fn bangla_widths_are_what_unicode_width_says() {
        assert_eq!(width("বাংলা"), 3, "the aa-kar has no width");
        assert_eq!(
            width("ক্ষ"),
            2,
            "the virama has no width, a conjunct is two letters"
        );
        assert_eq!(width("কি"), 2, "the i-kar has a width of its own");
        assert_eq!(width("নথি/ছবি.png"), 11);
        assert_eq!(width("র\u{200d}্যাব"), 3, "the joiner has no width");
    }

    #[test]
    fn a_bangla_name_is_cut_inside_the_room_and_never_after_a_virama() {
        let names = ["রিপোর্ট.txt", "নথি/ছবি.png", "ক্ষত্রিয়", "র\u{200d}্যাব", "বাংলা"];
        for name in names {
            for cols in 0..14 {
                for unicode in [true, false] {
                    let cut = fit(name, cols, unicode);
                    assert!(width(&cut) <= cols, "{name:?} in {cols}: {cut:?}");
                    let without_mark = cut.trim_end_matches(['…', '.']);
                    assert!(
                        !without_mark
                            .chars()
                            .last()
                            .is_some_and(asks_for_a_next_letter),
                        "{name:?} in {cols}: {cut:?}"
                    );
                }
            }
        }
        assert_eq!(
            fit("ক্ষ", 1, false),
            "ক",
            "the virama is dropped with the cut"
        );
        assert_eq!(fit("ক্ষত্র", 3, true), "ক্ষ…");
    }

    #[test]
    fn width_counts_columns() {
        assert_eq!(width("abc"), 3);
        assert_eq!(width(""), 0);
        assert_eq!(width("日本"), 4, "wide characters take two");
        assert_eq!(width("e\u{301}"), 1, "a combining accent takes none");
    }

    #[test]
    fn fit_leaves_text_that_fits_and_cuts_with_an_ellipsis() {
        assert_eq!(fit("hello", 5, true), "hello");
        assert_eq!(fit("hello", 4, true), "hel…");
        assert_eq!(fit("hello", 4, false), "h...");
        assert_eq!(fit("hello", 1, true), "…");
        assert_eq!(
            fit("hello", 2, false),
            "he",
            "no room for three dots: cut hard"
        );
        assert_eq!(fit("hello", 0, true), "");
    }

    #[test]
    fn fit_never_splits_a_wide_character_and_stays_inside_the_room() {
        for cols in 0..12 {
            for text in [
                "日本語のファイル.txt",
                "abc日本def",
                "e\u{301}e\u{301}e\u{301}x",
            ] {
                for unicode in [true, false] {
                    let cut = fit(text, cols, unicode);
                    assert!(width(&cut) <= cols, "{text:?} in {cols}: {cut:?}");
                }
            }
        }
        assert_eq!(fit("日本語", 5, true), "日本…");
    }

    #[test]
    fn a_line_is_cut_across_spans_and_the_mark_takes_the_style_of_the_cut_span() {
        let line = Line::new()
            .tone(Tone::Key, "Enter")
            .tone(Tone::Danger, " delete everything");
        let cut = line.fit(12, true);
        assert_eq!(cut.plain_text(), "Enter delet…");
        assert_eq!(cut.width(), 12);
        let last = cut.spans().last().map(|s| s.style.tone);
        assert_eq!(last, Some(Tone::Danger));
        assert_eq!(line.fit(100, true), line);
    }

    #[test]
    fn padding_reaches_the_width_and_never_cuts() {
        assert_eq!(Line::plain("ab").padded_to(5).plain_text(), "ab   ");
        assert_eq!(Line::plain("日").padded_to(4).width(), 4);
        assert_eq!(Line::plain("abcdef").padded_to(3).plain_text(), "abcdef");
    }
}
