//! The theme table: which look each role of the command line has.
//!
//! **Only the 16 ANSI color names exist here.** [`AnsiColor`] has exactly sixteen values, so a
//! hex color or a 256-color index cannot even be written. The terminal decides what "red" or
//! "cyan" looks like, which is why a theme made of these names follows whatever theme the
//! user's terminal has. There is no bold either: [`RoleStyle`] has no field for it. The dim
//! attribute is used only for secondary things (a comment, an autosuggestion).
//!
//! A [`Theme`] maps every [`Role`] to a [`RoleStyle`]. Roles have stable names
//! ([`Role::name`], [`Role::from_name`]) so that a later `ui.theme` file can swap the table.

/// One of the 16 colors every terminal has. `White` is the normal white (often light gray) and
/// `BrightBlack` is what most terminals draw as dark gray.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AnsiColor {
    /// SGR 30.
    Black,
    /// SGR 31.
    Red,
    /// SGR 32.
    Green,
    /// SGR 33.
    Yellow,
    /// SGR 34.
    Blue,
    /// SGR 35.
    Magenta,
    /// SGR 36.
    Cyan,
    /// SGR 37.
    White,
    /// SGR 90.
    BrightBlack,
    /// SGR 91.
    BrightRed,
    /// SGR 92.
    BrightGreen,
    /// SGR 93.
    BrightYellow,
    /// SGR 94.
    BrightBlue,
    /// SGR 95.
    BrightMagenta,
    /// SGR 96.
    BrightCyan,
    /// SGR 97.
    BrightWhite,
}

impl AnsiColor {
    /// All sixteen, in SGR order.
    pub const ALL: [AnsiColor; 16] = [
        AnsiColor::Black,
        AnsiColor::Red,
        AnsiColor::Green,
        AnsiColor::Yellow,
        AnsiColor::Blue,
        AnsiColor::Magenta,
        AnsiColor::Cyan,
        AnsiColor::White,
        AnsiColor::BrightBlack,
        AnsiColor::BrightRed,
        AnsiColor::BrightGreen,
        AnsiColor::BrightYellow,
        AnsiColor::BrightBlue,
        AnsiColor::BrightMagenta,
        AnsiColor::BrightCyan,
        AnsiColor::BrightWhite,
    ];

    /// The name used in theme files: `red`, `bright-black`, ...
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            AnsiColor::Black => "black",
            AnsiColor::Red => "red",
            AnsiColor::Green => "green",
            AnsiColor::Yellow => "yellow",
            AnsiColor::Blue => "blue",
            AnsiColor::Magenta => "magenta",
            AnsiColor::Cyan => "cyan",
            AnsiColor::White => "white",
            AnsiColor::BrightBlack => "bright-black",
            AnsiColor::BrightRed => "bright-red",
            AnsiColor::BrightGreen => "bright-green",
            AnsiColor::BrightYellow => "bright-yellow",
            AnsiColor::BrightBlue => "bright-blue",
            AnsiColor::BrightMagenta => "bright-magenta",
            AnsiColor::BrightCyan => "bright-cyan",
            AnsiColor::BrightWhite => "bright-white",
        }
    }

    /// The color called `name`, if there is one.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|color| color.name() == name)
    }

    /// The number of the "select graphic rendition" code that sets this foreground color:
    /// 30 to 37 and 90 to 97.
    #[must_use]
    pub fn sgr_foreground(self) -> u8 {
        let index = Self::ALL
            .iter()
            .position(|color| *color == self)
            .unwrap_or(0);
        // `ALL` has 16 entries, so the index fits.
        let index = u8::try_from(index).unwrap_or(0);
        if index < 8 {
            30 + index
        } else {
            90 + (index - 8)
        }
    }
}

/// What a piece of the command line is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    /// Blanks and anything without a role of its own.
    Normal,
    /// A command that exists (builtin, function, alias or program).
    Command,
    /// A reserved word: `if`, `then`, `for`, `{`, `!`, ...
    Keyword,
    /// A command that was not found, or a piece of text with a syntax problem.
    Error,
    /// An ordinary argument, or the `FOO=` of an assignment.
    Param,
    /// An argument that starts with `-`.
    Option,
    /// Added to an argument that names an existing file or directory.
    ValidPath,
    /// Quoted text, and the body of a here-document.
    Quote,
    /// `|`, `&&`, `||`, `(` and `)`.
    Operator,
    /// `>`, `<`, `>>`, `2>&1`, the digits of `2>`, and a here-document's delimiter.
    Redirection,
    /// `;`, `&` and `;;`: what ends a command.
    End,
    /// A comment.
    Comment,
    /// A backslash escape or a line continuation.
    Escape,
    /// `$x`, `${...}`, `$(...)`, backticks and `$((...))`.
    Expansion,
    /// The grey text that the history suggests (used by the hinter, S14b).
    Autosuggestion,
    /// The selected text in the editor.
    Selection,
}

impl Role {
    /// Every role.
    pub const ALL: [Role; 16] = [
        Role::Normal,
        Role::Command,
        Role::Keyword,
        Role::Error,
        Role::Param,
        Role::Option,
        Role::ValidPath,
        Role::Quote,
        Role::Operator,
        Role::Redirection,
        Role::End,
        Role::Comment,
        Role::Escape,
        Role::Expansion,
        Role::Autosuggestion,
        Role::Selection,
    ];

    /// The name used in theme files.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Role::Normal => "normal",
            Role::Command => "command",
            Role::Keyword => "keyword",
            Role::Error => "error",
            Role::Param => "param",
            Role::Option => "option",
            Role::ValidPath => "valid_path",
            Role::Quote => "quote",
            Role::Operator => "operator",
            Role::Redirection => "redirection",
            Role::End => "end",
            Role::Comment => "comment",
            Role::Escape => "escape",
            Role::Expansion => "expansion",
            Role::Autosuggestion => "autosuggestion",
            Role::Selection => "selection",
        }
    }

    /// The role called `name`, if there is one.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|role| role.name() == name)
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|role| *role == self).unwrap_or(0)
    }
}

/// How one role looks. There is no bold on purpose.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RoleStyle {
    /// The text color. `None` is the terminal's own default color.
    pub color: Option<AnsiColor>,
    /// The dim attribute (SGR 2). For secondary things only.
    pub dim: bool,
    /// Underline (SGR 4).
    pub underline: bool,
    /// Swapped foreground and background (SGR 7).
    pub reverse: bool,
}

impl RoleStyle {
    /// The terminal's defaults: no color, no attribute.
    pub const PLAIN: RoleStyle = RoleStyle {
        color: None,
        dim: false,
        underline: false,
        reverse: false,
    };

    const fn color(color: AnsiColor) -> Self {
        RoleStyle {
            color: Some(color),
            ..Self::PLAIN
        }
    }

    /// This style with `top` laid over it: attributes add up, and the color of `top` wins when
    /// it has one. This is how an existing file keeps the color of its word and gets underlined.
    #[must_use]
    pub fn overlaid_with(self, top: RoleStyle) -> RoleStyle {
        RoleStyle {
            color: top.color.or(self.color),
            dim: self.dim || top.dim,
            underline: self.underline || top.underline,
            reverse: self.reverse || top.reverse,
        }
    }

    /// The same style without its color. Attributes stay, so that an autosuggestion is still
    /// dim and a valid path still underlined when `NO_COLOR` is set.
    #[must_use]
    pub fn without_color(self) -> RoleStyle {
        RoleStyle {
            color: None,
            ..self
        }
    }
}

/// The table from role to look.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Theme {
    styles: [RoleStyle; Role::ALL.len()],
}

impl Default for Theme {
    /// The default theme: terminal defaults for text that is fine, normal (not bright) colors
    /// for the rest. Dim only for comments and autosuggestions.
    fn default() -> Self {
        let mut theme = Theme {
            styles: [RoleStyle::PLAIN; Role::ALL.len()],
        };
        theme.set(Role::Error, RoleStyle::color(AnsiColor::Red));
        theme.set(Role::Param, RoleStyle::color(AnsiColor::Cyan));
        theme.set(Role::Option, RoleStyle::color(AnsiColor::Cyan));
        theme.set(
            Role::ValidPath,
            RoleStyle {
                underline: true,
                ..RoleStyle::PLAIN
            },
        );
        theme.set(Role::Quote, RoleStyle::color(AnsiColor::Yellow));
        theme.set(Role::Operator, RoleStyle::color(AnsiColor::Cyan));
        theme.set(Role::Redirection, RoleStyle::color(AnsiColor::Cyan));
        theme.set(Role::End, RoleStyle::color(AnsiColor::Green));
        theme.set(
            Role::Comment,
            RoleStyle {
                dim: true,
                ..RoleStyle::color(AnsiColor::Red)
            },
        );
        theme.set(Role::Escape, RoleStyle::color(AnsiColor::Cyan));
        theme.set(Role::Expansion, RoleStyle::color(AnsiColor::Cyan));
        theme.set(
            Role::Autosuggestion,
            RoleStyle {
                dim: true,
                ..RoleStyle::color(AnsiColor::BrightBlack)
            },
        );
        theme.set(
            Role::Selection,
            RoleStyle {
                reverse: true,
                ..RoleStyle::PLAIN
            },
        );
        theme
    }
}

impl Theme {
    /// The look of `role`.
    #[must_use]
    pub fn get(&self, role: Role) -> RoleStyle {
        self.styles[role.index()]
    }

    /// Changes the look of `role`.
    pub fn set(&mut self, role: Role, style: RoleStyle) {
        self.styles[role.index()] = style;
    }

    /// The same theme with every color removed (for `NO_COLOR`). Attributes stay.
    #[must_use]
    pub fn without_color(&self) -> Theme {
        Theme {
            styles: self.styles.map(RoleStyle::without_color),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn there_are_sixteen_colors_with_the_standard_codes() {
        let codes: Vec<u8> = AnsiColor::ALL.iter().map(|c| c.sgr_foreground()).collect();
        assert_eq!(
            codes,
            [
                30, 31, 32, 33, 34, 35, 36, 37, 90, 91, 92, 93, 94, 95, 96, 97
            ]
        );
    }

    #[test]
    fn color_and_role_names_round_trip() {
        for color in AnsiColor::ALL {
            assert_eq!(AnsiColor::from_name(color.name()), Some(color));
        }
        for role in Role::ALL {
            assert_eq!(Role::from_name(role.name()), Some(role));
        }
        assert_eq!(AnsiColor::from_name("#ff0000"), None);
        assert_eq!(AnsiColor::from_name("color208"), None);
        assert_eq!(Role::from_name("nope"), None);
    }

    #[test]
    fn every_role_has_its_own_name_and_index() {
        let mut names: Vec<&str> = Role::ALL.iter().map(|r| r.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), Role::ALL.len());
        for (index, role) in Role::ALL.iter().enumerate() {
            assert_eq!(role.index(), index);
        }
    }

    #[test]
    fn the_default_roles_are_the_ones_the_owner_listed() {
        let theme = Theme::default();
        let color = |role| theme.get(role).color;
        assert_eq!(color(Role::Command), None, "a found command has no color");
        assert_eq!(color(Role::Error), Some(AnsiColor::Red));
        assert_eq!(color(Role::Param), Some(AnsiColor::Cyan));
        assert_eq!(color(Role::Option), Some(AnsiColor::Cyan));
        assert_eq!(color(Role::Quote), Some(AnsiColor::Yellow));
        assert_eq!(color(Role::Operator), Some(AnsiColor::Cyan));
        assert_eq!(color(Role::Redirection), Some(AnsiColor::Cyan));
        assert_eq!(color(Role::End), Some(AnsiColor::Green));
        assert_eq!(color(Role::Comment), Some(AnsiColor::Red));
        assert_eq!(color(Role::Autosuggestion), Some(AnsiColor::BrightBlack));
        let path = theme.get(Role::ValidPath);
        assert!(path.underline && path.color.is_none(), "keeps its color");
    }

    #[test]
    fn only_comments_and_autosuggestions_are_dim() {
        let theme = Theme::default();
        let dim: Vec<Role> = Role::ALL
            .into_iter()
            .filter(|role| theme.get(*role).dim)
            .collect();
        assert_eq!(dim, [Role::Comment, Role::Autosuggestion]);
    }

    #[test]
    fn the_default_theme_prefers_normal_colors() {
        // The one bright color is the autosuggestion (bright black), as the owner asked.
        let theme = Theme::default();
        for role in Role::ALL {
            if let Some(color) = theme.get(role).color {
                assert!(
                    color.sgr_foreground() < 90 || role == Role::Autosuggestion,
                    "{role:?} uses {color:?}"
                );
            }
        }
    }

    #[test]
    fn no_color_removes_colors_and_keeps_attributes() {
        let plain = Theme::default().without_color();
        for role in Role::ALL {
            assert_eq!(plain.get(role).color, None, "{role:?}");
        }
        assert!(plain.get(Role::Comment).dim);
        assert!(plain.get(Role::Autosuggestion).dim);
        assert!(plain.get(Role::ValidPath).underline);
        assert!(plain.get(Role::Selection).reverse);
    }

    #[test]
    fn overlaying_underline_keeps_the_color_of_the_word() {
        let cyan = RoleStyle::color(AnsiColor::Cyan);
        let underline = Theme::default().get(Role::ValidPath);
        let both = cyan.overlaid_with(underline);
        assert_eq!(both.color, Some(AnsiColor::Cyan));
        assert!(both.underline);
        assert!(!both.dim);
    }

    #[test]
    fn a_theme_can_be_changed_role_by_role() {
        let mut theme = Theme::default();
        theme.set(Role::Quote, RoleStyle::color(AnsiColor::Green));
        assert_eq!(theme.get(Role::Quote).color, Some(AnsiColor::Green));
        assert_eq!(theme.get(Role::Param).color, Some(AnsiColor::Cyan));
    }
}
