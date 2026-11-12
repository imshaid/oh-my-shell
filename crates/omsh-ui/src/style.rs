//! From the theme's look to ratatui's. This is the one place where the 16 ANSI color names
//! become ratatui colors.

use ratatui::style::{Color, Modifier, Style};

use crate::theme::{AnsiColor, RoleStyle};

/// The ratatui color for one of the 16 ANSI colors.
#[must_use]
pub fn color(color: AnsiColor) -> Color {
    match color {
        AnsiColor::Black => Color::Black,
        AnsiColor::Red => Color::Red,
        AnsiColor::Green => Color::Green,
        AnsiColor::Yellow => Color::Yellow,
        AnsiColor::Blue => Color::Blue,
        AnsiColor::Magenta => Color::Magenta,
        AnsiColor::Cyan => Color::Cyan,
        AnsiColor::White => Color::Gray,
        AnsiColor::BrightBlack => Color::DarkGray,
        AnsiColor::BrightRed => Color::LightRed,
        AnsiColor::BrightGreen => Color::LightGreen,
        AnsiColor::BrightYellow => Color::LightYellow,
        AnsiColor::BrightBlue => Color::LightBlue,
        AnsiColor::BrightMagenta => Color::LightMagenta,
        AnsiColor::BrightCyan => Color::LightCyan,
        AnsiColor::BrightWhite => Color::White,
    }
}

/// The ratatui style of a role or token: its color (if any) and its dim, underline and reverse
/// attributes. There is no bold, as in the rest of the theme.
#[must_use]
pub fn style(role: RoleStyle) -> Style {
    let mut style = Style::default();
    if let Some(wanted) = role.color {
        style = style.fg(color(wanted));
    }
    if role.dim {
        style = style.add_modifier(Modifier::DIM);
    }
    if role.underline {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    if role.reverse {
        style = style.add_modifier(Modifier::REVERSED);
    }
    style
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_sixteen_colors_map_to_different_ratatui_colors() {
        let mut seen: Vec<Color> = AnsiColor::ALL.iter().map(|c| color(*c)).collect();
        seen.sort_by_key(|c| format!("{c:?}"));
        seen.dedup();
        assert_eq!(seen.len(), 16);
    }

    #[test]
    fn a_style_carries_the_color_and_the_attributes() {
        let plain = style(RoleStyle::PLAIN);
        assert_eq!(plain, Style::default());
        let dim = style(RoleStyle {
            color: Some(AnsiColor::BrightBlack),
            dim: true,
            underline: false,
            reverse: true,
        });
        assert_eq!(dim.fg, Some(Color::DarkGray));
        assert!(dim.add_modifier.contains(Modifier::DIM));
        assert!(dim.add_modifier.contains(Modifier::REVERSED));
        assert!(!dim.add_modifier.contains(Modifier::UNDERLINED));
    }
}
