//! Showing error reports to the user.

use std::io::{self, Write};

use miette::{Diagnostic, GraphicalReportHandler, GraphicalTheme, NarratableReportHandler};
#[cfg(test)]
use omsh_config::Icons;
#[cfg(test)]
use omsh_ui::Facts;
use omsh_ui::Look;

/// How reports are drawn, chosen once from what stderr is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    /// Plain sentences, for a pipe or a file: no boxes or colors.
    Plain,
    /// The source line with marks under the problem.
    Graphical {
        /// Use ANSI colors.
        color: bool,
        /// Use box-drawing characters instead of ASCII.
        unicode: bool,
    },
}

impl Style {
    /// The style that goes with a [`Look`]: plain sentences for a pipe, else the graphical one
    /// with the look's color and Unicode choices.
    #[must_use]
    pub fn from_look(look: Look) -> Self {
        if look.terminal {
            Self::Graphical {
                color: look.color,
                unicode: look.unicode,
            }
        } else {
            Self::Plain
        }
    }

    /// The choice from the facts, so that it can be tested.
    #[cfg(test)]
    fn choose(terminal: bool, dumb: bool, no_color: bool, utf8: bool, icons: Icons) -> Self {
        let facts = Facts {
            terminal,
            dumb,
            no_color,
            utf8,
            ssh: false,
        };
        Self::from_look(Look::decide(facts, icons, None))
    }
}

/// Draws a report as text.
#[must_use]
pub fn render(report: &dyn Diagnostic, style: Style) -> String {
    let mut out = String::new();
    let drawn = match style {
        Style::Plain => NarratableReportHandler::new().render_report(&mut out, report),
        Style::Graphical { color, unicode } => {
            let theme = match (color, unicode) {
                (true, true) => GraphicalTheme::unicode(),
                (true, false) => GraphicalTheme::ascii(),
                (false, true) => GraphicalTheme::unicode_nocolor(),
                (false, false) => GraphicalTheme::none(),
            };
            GraphicalReportHandler::new_themed(theme).render_report(&mut out, report)
        }
    };
    if drawn.is_err() {
        // Formatting into a `String` does not fail, but a broken report must still say
        // something rather than print nothing.
        out = report.to_string();
    }
    out
}

/// Writes a report to stderr. A failed write is ignored: there is nowhere left to say so.
pub fn show(report: &dyn Diagnostic, style: Style) {
    let text = render(report, style);
    let mut err = io::stderr();
    let _ = writeln!(err, "{}", text.trim_end());
}

#[cfg(test)]
mod tests {
    use omsh_exec::{NotYet, Span};

    use super::*;

    fn report() -> impl Diagnostic {
        NotYet {
            what: "pipelines",
            span: Span::new(0, 5),
        }
        .to_diagnostic("a | b")
    }

    #[test]
    fn the_icons_setting_decides_the_box_characters() {
        let graphical = |unicode| Style::Graphical {
            color: true,
            unicode,
        };
        // `ascii` wins over a UTF-8 locale, `unicode` over a dumb terminal.
        assert_eq!(
            Style::choose(true, false, false, true, Icons::Ascii),
            graphical(false)
        );
        assert_eq!(
            Style::choose(true, false, false, false, Icons::Unicode),
            graphical(true)
        );
        // `auto` follows the locale and the terminal.
        assert_eq!(
            Style::choose(true, false, false, true, Icons::Auto),
            graphical(true)
        );
        assert_eq!(
            Style::choose(true, false, false, false, Icons::Auto),
            graphical(false)
        );
        assert_eq!(
            Style::choose(true, true, false, true, Icons::Auto),
            Style::Graphical {
                color: false,
                unicode: false
            }
        );
        // `NO_COLOR` takes the colors only, and a pipe gets plain sentences whatever is set.
        assert_eq!(
            Style::choose(true, false, true, true, Icons::Unicode),
            Style::Graphical {
                color: false,
                unicode: true
            }
        );
        assert_eq!(
            Style::choose(false, false, false, true, Icons::Unicode),
            Style::Plain
        );
    }

    #[test]
    fn a_plain_report_is_sentences_without_escape_codes() {
        let text = render(&report(), Style::Plain);
        assert!(text.contains("pipelines cannot run yet"), "{text}");
        assert!(!text.contains('\u{1b}'), "{text}");
    }

    #[test]
    fn a_graphical_report_shows_the_source_line() {
        let style = Style::Graphical {
            color: false,
            unicode: false,
        };
        let text = render(&report(), style);
        assert!(text.contains("a | b"), "{text}");
        assert!(text.contains("pipelines cannot run yet"), "{text}");
        assert!(!text.contains('\u{1b}'), "{text}");
    }

    #[test]
    fn color_adds_escape_codes() {
        let style = Style::Graphical {
            color: true,
            unicode: true,
        };
        assert!(render(&report(), style).contains('\u{1b}'));
    }
}
