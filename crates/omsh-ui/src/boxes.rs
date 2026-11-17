//! Rounded boxes for panels (UI upgrade, U1): a title in the top rule, key hints in the bottom
//! rule, and either a closed right edge or none.
//!
//! - [`full`]: a box with a right border. Every row is padded to the same width, so it is only
//!   for text that is entirely ours (setup, help, the tour, the details panel).
//! - [`open`]: a left rail and the two rules, with **no right edge** and no padding. For anything
//!   that shows user text (file names, commands): the width of Bangla and other combining text
//!   is not the same in every terminal, so a right border would be crooked. A row is cut to the
//!   width with an ellipsis, so it never wraps.
//!
//! Both return [`Line`]s: pure data, drawn by [`crate::text::render`]. ASCII (`+ - |`) when the
//! caps say so.

use crate::caps::Caps;
use crate::text::{Line, Style, Tone, width};

/// The narrowest a panel is drawn: below this the rules would have no room for a title.
pub const MIN_WIDTH: usize = 10;

/// A key and what it does: `Enter`, `run`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hint {
    /// The key, shown in the key color.
    pub key: String,
    /// What it does, shown dim.
    pub label: String,
}

/// What a box shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Panel {
    /// In the top rule. Empty: a plain rule.
    pub title: String,
    /// The rows between the rules.
    pub body: Vec<Line>,
    /// In the bottom rule. Empty: a plain rule.
    pub hints: Vec<Hint>,
    /// The color of the rules and the rail: `Info` (blue) by default, `Danger` for a risky
    /// question, `Ai` for the AI.
    pub tone: Tone,
}

impl Panel {
    /// A panel with a title and nothing else.
    #[must_use]
    pub fn new(title: &str) -> Self {
        Self {
            title: title.to_owned(),
            body: Vec::new(),
            hints: Vec::new(),
            tone: Tone::Info,
        }
    }

    /// This panel with `line` added to the body.
    #[must_use]
    pub fn line(mut self, line: Line) -> Self {
        self.body.push(line);
        self
    }

    /// This panel with every line of `lines` added to the body.
    #[must_use]
    pub fn lines(mut self, lines: impl IntoIterator<Item = Line>) -> Self {
        self.body.extend(lines);
        self
    }

    /// This panel with a hint added to the bottom rule.
    #[must_use]
    pub fn hint(mut self, key: &str, label: &str) -> Self {
        self.hints.push(Hint {
            key: key.to_owned(),
            label: label.to_owned(),
        });
        self
    }

    /// This panel with the color of its rules set.
    #[must_use]
    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = tone;
        self
    }
}

struct Chars {
    top_left: &'static str,
    top_right: &'static str,
    bottom_left: &'static str,
    bottom_right: &'static str,
    horizontal: &'static str,
    vertical: &'static str,
}

const UNICODE: Chars = Chars {
    top_left: "╭",
    top_right: "╮",
    bottom_left: "╰",
    bottom_right: "╯",
    horizontal: "─",
    vertical: "│",
};

const ASCII: Chars = Chars {
    top_left: "+",
    top_right: "+",
    bottom_left: "+",
    bottom_right: "+",
    horizontal: "-",
    vertical: "|",
};

fn chars(caps: &Caps) -> &'static Chars {
    if caps.unicode { &UNICODE } else { &ASCII }
}

/// A box with a right border, `width` columns wide. Every row is exactly `width` columns, as
/// long as the text in it is measured the same way by the terminal: use it for text that is
/// entirely ours.
#[must_use]
pub fn full(panel: &Panel, caps: &Caps, width: u16) -> Vec<Line> {
    let total = usize::from(width).max(MIN_WIDTH);
    let set = chars(caps);
    let edge = Style::of(panel.tone);
    let inner = total - 4;
    let mut out = Vec::with_capacity(panel.body.len() + 2);
    out.push(rule(
        set.top_left,
        Some(set.top_right),
        title_line(&panel.title),
        total,
        set,
        edge,
        caps,
    ));
    for row in &panel.body {
        let row = row.fit(inner, caps.unicode).padded_to(inner);
        let mut line = Line::new().then(set.vertical, edge).then(" ", Style::PLAIN);
        line.append(&row);
        line.push(" ", Style::PLAIN);
        line.push(set.vertical, edge);
        out.push(line);
    }
    out.push(rule(
        set.bottom_left,
        Some(set.bottom_right),
        hints_line(&panel.hints, caps),
        total,
        set,
        edge,
        caps,
    ));
    out
}

/// A panel with a left rail and the two rules and **no right edge**, at most `width` columns
/// wide. Rows are cut to fit and never padded. Use it for anything that shows user text.
#[must_use]
pub fn open(panel: &Panel, caps: &Caps, width: u16) -> Vec<Line> {
    let total = usize::from(width).max(MIN_WIDTH);
    let set = chars(caps);
    let edge = Style::of(panel.tone);
    let room = total - 2;
    let mut out = Vec::with_capacity(panel.body.len() + 2);
    out.push(rule(
        set.top_left,
        None,
        title_line(&panel.title),
        total,
        set,
        edge,
        caps,
    ));
    for row in &panel.body {
        let mut line = Line::new().then(set.vertical, edge).then(" ", Style::PLAIN);
        line.append(&row.fit(room, caps.unicode));
        out.push(line);
    }
    out.push(rule(
        set.bottom_left,
        None,
        hints_line(&panel.hints, caps),
        total,
        set,
        edge,
        caps,
    ));
    out
}

fn title_line(title: &str) -> Line {
    Line::plain(title)
}

fn hints_line(hints: &[Hint], caps: &Caps) -> Line {
    let separator = if caps.unicode { " · " } else { ", " };
    let mut line = Line::new();
    for (index, hint) in hints.iter().enumerate() {
        if index > 0 {
            line.push(separator, Style::of(Tone::Dim));
        }
        line.push(&hint.key, Style::of(Tone::Key));
        line.push(" ", Style::PLAIN);
        line.push(&hint.label, Style::of(Tone::Dim));
    }
    line
}

/// One rule: `corner h label h h h corner`, `total` columns wide when `right` is given, else
/// the same without the right corner. An empty label gives a plain rule.
fn rule(
    left: &str,
    right: Option<&str>,
    label: Line,
    total: usize,
    set: &Chars,
    edge: Style,
    caps: &Caps,
) -> Line {
    let right_width = right.map_or(0, width);
    let mut line = Line::new().then(left, edge);
    let mut used = width(left) + right_width;
    if !label.is_empty() {
        // corner, one rule character, a blank, the label, a blank, the rest, corner
        let room = total.saturating_sub(used + 1 + 1 + 1);
        let label = label.fit(room, caps.unicode);
        line.push(set.horizontal, edge);
        line.push(" ", Style::PLAIN);
        used += 1 + 1 + label.width() + 1;
        line.append(&label);
        line.push(" ", Style::PLAIN);
    }
    line.push(&set.horizontal.repeat(total.saturating_sub(used)), edge);
    if let Some(right) = right {
        line.push(right, edge);
    }
    line
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

    fn unicode() -> Caps {
        Caps::decide(GOOD, Out::Stderr, Icons::Auto, None)
    }

    fn ascii() -> Caps {
        Caps::decide(GOOD, Out::Stderr, Icons::Ascii, None)
    }

    fn text(lines: &[Line]) -> Vec<String> {
        lines.iter().map(Line::plain_text).collect()
    }

    fn sample() -> Panel {
        Panel::new("Set up AI")
            .line(Line::plain("Pick a provider."))
            .line(Line::plain("Your key stays on this machine."))
            .hint("Enter", "choose")
            .hint("Esc", "cancel")
    }

    #[test]
    fn a_full_box_is_rounded_with_the_title_and_hints_in_the_rules() {
        let lines = text(&full(&sample(), &unicode(), 40));
        assert_eq!(
            lines,
            [
                "╭─ Set up AI ──────────────────────────╮",
                "│ Pick a provider.                     │",
                "│ Your key stays on this machine.      │",
                "╰─ Enter choose · Esc cancel ──────────╯",
            ]
        );
    }

    #[test]
    fn the_ascii_box_uses_plus_minus_and_bar_only() {
        let lines = text(&full(&sample(), &ascii(), 40));
        assert_eq!(
            lines,
            [
                "+- Set up AI --------------------------+",
                "| Pick a provider.                     |",
                "| Your key stays on this machine.      |",
                "+- Enter choose, Esc cancel -----------+",
            ]
        );
        assert!(lines.iter().all(|line| line.is_ascii()));
    }

    #[test]
    fn every_row_of_a_full_box_has_the_width_asked_for() {
        let panel = sample()
            .line(Line::plain("日本語のファイル名"))
            .line(Line::plain(&"x".repeat(200)));
        for width in [10, 24, 40, 79] {
            for caps in [unicode(), ascii()] {
                for line in full(&panel, &caps, width) {
                    assert_eq!(line.width(), usize::from(width), "{:?}", line.plain_text());
                }
            }
        }
    }

    #[test]
    fn an_open_panel_has_a_rail_and_no_right_edge() {
        let panel = Panel::new("Run it?")
            .tone(Tone::Danger)
            .line(Line::plain("rm -rf build/"))
            .line(Line::plain("12 files"))
            .hint("t", "trash");
        let lines = text(&open(&panel, &unicode(), 30));
        assert_eq!(
            lines,
            [
                "╭─ Run it? ───────────────────",
                "│ rm -rf build/",
                "│ 12 files",
                "╰─ t trash ───────────────────",
            ]
        );
        for line in &lines {
            assert!(!line.ends_with('│') && !line.ends_with('╮') && !line.ends_with('╯'));
        }
    }

    #[test]
    fn an_open_panel_cuts_a_long_row_and_never_pads_a_short_one() {
        let panel = Panel::new("t")
            .line(Line::plain(&"abcdefghij".repeat(10)))
            .line(Line::plain("ab"));
        let lines = open(&panel, &unicode(), 20);
        assert_eq!(lines[1].plain_text(), "│ abcdefghijabcdefg…");
        assert!(lines.iter().all(|line| line.width() <= 20));
        assert_eq!(lines[2].plain_text(), "│ ab");
    }

    #[test]
    fn an_open_panel_keeps_every_row_inside_the_width_for_bangla_and_wide_text() {
        let panel = Panel::new("Run it?")
            .line(Line::plain("rm রিপোর্ট.txt নথি/ছবি.png ক্ষত্রিয়"))
            .line(Line::plain("rm 日本語のファイル名.txt"));
        for width in [10, 14, 20, 30] {
            for caps in [unicode(), ascii()] {
                for line in open(&panel, &caps, width) {
                    assert!(
                        line.width() <= usize::from(width),
                        "{width}: {:?}",
                        line.plain_text()
                    );
                }
            }
        }
    }

    #[test]
    fn a_title_that_is_too_long_is_cut_and_a_panel_without_title_or_hints_has_plain_rules() {
        let long = Panel::new(&"T".repeat(100)).line(Line::plain("x"));
        let lines = full(&long, &unicode(), 20);
        assert_eq!(lines[0].width(), 20);
        assert!(lines[0].plain_text().contains('…'));
        let bare = Panel::new("").line(Line::plain("x"));
        assert_eq!(text(&full(&bare, &unicode(), 12))[0], "╭──────────╮");
        assert_eq!(text(&full(&bare, &unicode(), 12))[2], "╰──────────╯");
        assert_eq!(text(&open(&bare, &unicode(), 12))[0], "╭───────────");
    }

    #[test]
    fn a_width_below_the_minimum_is_raised_to_it() {
        let lines = full(&sample(), &unicode(), 3);
        assert!(lines.iter().all(|line| line.width() == MIN_WIDTH));
    }

    #[test]
    fn the_rules_take_the_tone_of_the_panel_and_the_hints_have_the_key_tone() {
        let lines = open(&sample().tone(Tone::Ai), &unicode(), 40);
        let first = lines[0].spans()[0].style.tone;
        assert_eq!(first, Tone::Ai);
        let bottom_tones: Vec<Tone> = lines[3].spans().iter().map(|s| s.style.tone).collect();
        assert!(bottom_tones.contains(&Tone::Key));
        assert!(bottom_tones.contains(&Tone::Dim));
    }

    #[test]
    fn user_text_with_an_escape_cannot_break_out_of_a_box() {
        let panel = Panel::new("t").line(Line::plain("rm \x1b[2J file"));
        for line in open(&panel, &unicode(), 30) {
            assert!(!line.plain_text().contains('\x1b'));
        }
    }
}
