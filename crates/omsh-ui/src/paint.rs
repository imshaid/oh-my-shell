//! Wrapping text in the theme's colors, and the glyphs that panels are made of.
//!
//! A [`Paint`] is built once from a [`Theme`] and a [`Look`]. When the look has no color, the
//! theme loses its colors first and keeps its attributes (the rule that `NO_COLOR` already
//! follows in the editor), so a dim hint is still dim. When the look says the output is not a
//! terminal, nothing is written but the text.

use crate::look::Look;
use crate::theme::{RoleStyle, Theme, Token};

const RESET: &str = "\x1b[0m";

/// The characters panels are drawn with, in one of two alphabets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Glyphs {
    /// The left edge of a panel line, with the blank after it.
    pub bar: &'static str,
    /// A step that worked.
    pub ok: &'static str,
    /// A step that failed.
    pub fail: &'static str,
    /// A step that was not run.
    pub skip: &'static str,
}

impl Glyphs {
    /// Box-drawing and symbols.
    pub const UNICODE: Glyphs = Glyphs {
        bar: "│ ",
        ok: "✓",
        fail: "✗",
        skip: "–",
    };
    /// Plain ASCII.
    pub const ASCII: Glyphs = Glyphs {
        bar: "| ",
        ok: "ok",
        fail: "failed",
        skip: "-",
    };
}

/// Colors text by token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paint {
    theme: Theme,
    look: Look,
}

impl Paint {
    /// A paint for `theme` under `look`.
    #[must_use]
    pub fn new(theme: &Theme, look: Look) -> Self {
        let theme = if look.color {
            theme.clone()
        } else {
            theme.without_color()
        };
        Self { theme, look }
    }

    /// No color, no attributes, ASCII: what a pipe gets.
    #[must_use]
    pub fn plain() -> Self {
        Self::new(&Theme::default(), Look::plain())
    }

    /// The look this paint was made for.
    #[must_use]
    pub fn look(&self) -> Look {
        self.look
    }

    /// The glyph set.
    #[must_use]
    pub fn glyphs(&self) -> Glyphs {
        if self.look.unicode {
            Glyphs::UNICODE
        } else {
            Glyphs::ASCII
        }
    }

    /// `text` in the look of `token`. Unchanged when the token has no color and no attribute,
    /// and unchanged for any look that is not a terminal.
    #[must_use]
    pub fn token(&self, token: Token, text: &str) -> String {
        if !self.look.terminal || text.is_empty() {
            return text.to_owned();
        }
        match open(self.theme.token(token)) {
            Some(start) => format!("{start}{text}{RESET}"),
            None => text.to_owned(),
        }
    }

    /// The panel's left edge in the color of `token`: the bar and a blank, two columns wide.
    /// A look that is not a terminal gets two blanks instead, so that a panel written to a pipe
    /// or a file is indented as it always was and carries no drawing characters.
    #[must_use]
    pub fn bar(&self, token: Token) -> String {
        if !self.look.terminal {
            return "  ".to_owned();
        }
        self.token(token, self.glyphs().bar.trim_end()) + " "
    }

    /// The escape sequence that starts the look of `token`, or an empty string when it is
    /// plain. The caller writes the reset itself (`ESC [ 0 m`).
    #[must_use]
    pub fn start(&self, token: Token) -> String {
        if !self.look.terminal {
            return String::new();
        }
        open(self.theme.token(token)).unwrap_or_default()
    }
}

/// The SGR sequence for a style, or `None` when it is plain.
fn open(style: RoleStyle) -> Option<String> {
    style
        .sgr_parameters()
        .map(|parameters| format!("\x1b[{parameters}m"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use omsh_config::Motion;

    fn terminal(color: bool, unicode: bool) -> Look {
        Look {
            terminal: true,
            color,
            unicode,
            motion: Motion::Full,
        }
    }

    fn nord(look: Look) -> Paint {
        Paint::new(&Theme::default(), look)
    }

    #[test]
    fn a_token_is_wrapped_in_its_color() {
        let paint = nord(terminal(true, true));
        assert_eq!(paint.token(Token::Danger, "bad"), "\x1b[31mbad\x1b[0m");
        assert_eq!(paint.token(Token::Accent, "[y]"), "\x1b[34m[y]\x1b[0m");
        assert_eq!(paint.token(Token::Ai, "plan"), "\x1b[35mplan\x1b[0m");
    }

    #[test]
    fn the_dim_token_carries_dim_and_its_color() {
        let paint = nord(terminal(true, true));
        assert_eq!(paint.token(Token::Dim, "x"), "\x1b[2;90mx\x1b[0m");
    }

    #[test]
    fn the_muted_token_is_plain_text() {
        let paint = nord(terminal(true, true));
        assert_eq!(paint.token(Token::Muted, "x"), "x");
    }

    #[test]
    fn without_color_the_attributes_stay() {
        let paint = nord(terminal(false, true));
        assert_eq!(paint.token(Token::Danger, "bad"), "bad");
        assert_eq!(paint.token(Token::Dim, "x"), "\x1b[2mx\x1b[0m");
    }

    #[test]
    fn a_pipe_gets_no_escape_at_all() {
        let paint = Paint::plain();
        for token in Token::ALL {
            assert_eq!(paint.token(token, "text"), "text", "{token:?}");
            assert_eq!(paint.start(token), "");
        }
    }

    #[test]
    fn empty_text_is_not_wrapped() {
        assert_eq!(nord(terminal(true, true)).token(Token::Danger, ""), "");
    }

    #[test]
    fn the_glyphs_follow_the_look() {
        assert_eq!(nord(terminal(true, true)).glyphs(), Glyphs::UNICODE);
        assert_eq!(nord(terminal(true, false)).glyphs(), Glyphs::ASCII);
        assert_eq!(Glyphs::UNICODE.bar, "│ ");
        assert_eq!(Glyphs::ASCII.bar, "| ");
    }

    #[test]
    fn the_bar_is_colored_but_the_blank_after_it_is_not() {
        let paint = nord(terminal(true, true));
        assert_eq!(paint.bar(Token::Danger), "\x1b[31m│\x1b[0m ");
        assert_eq!(
            nord(terminal(true, false)).bar(Token::Danger),
            "\x1b[31m|\x1b[0m "
        );
        assert_eq!(
            Paint::plain().bar(Token::Danger),
            "  ",
            "a pipe gets blanks"
        );
    }

    #[test]
    fn start_gives_the_opening_sequence_only() {
        let paint = nord(terminal(true, true));
        assert_eq!(paint.start(Token::Danger), "\x1b[31m");
        assert_eq!(paint.start(Token::Muted), "");
    }

    #[test]
    fn a_custom_theme_changes_the_bytes() {
        let mut theme = Theme::default();
        theme.set_token(Token::Danger, RoleStyle::color(crate::AnsiColor::Magenta));
        let paint = Paint::new(&theme, terminal(true, true));
        assert_eq!(paint.token(Token::Danger, "x"), "\x1b[35mx\x1b[0m");
    }
}
