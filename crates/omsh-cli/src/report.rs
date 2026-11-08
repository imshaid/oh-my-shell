//! Showing error reports to the user.

use std::env;
use std::io::{self, IsTerminal, Write};

use miette::{Diagnostic, GraphicalReportHandler, GraphicalTheme, NarratableReportHandler};
use omsh_config::Icons;

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
    /// Picks a style for stderr as it is now. `icons` is the `ui.icons` setting: `ascii` and
    /// `unicode` decide the box characters; `auto` looks at the locale and the terminal.
    #[must_use]
    pub fn detect(icons: Icons) -> Self {
        let dumb = env::var("TERM").is_ok_and(|term| term == "dumb");
        Self::choose(
            io::stderr().is_terminal(),
            dumb,
            env_set("NO_COLOR"),
            locale_is_utf8(),
            icons,
        )
    }

    /// The choice itself, from the facts, so that it can be tested.
    fn choose(terminal: bool, dumb: bool, no_color: bool, utf8: bool, icons: Icons) -> Self {
        if !terminal {
            return Self::Plain;
        }
        Self::Graphical {
            color: !dumb && !no_color,
            unicode: match icons {
                Icons::Unicode => true,
                Icons::Ascii => false,
                Icons::Auto => !dumb && utf8,
            },
        }
    }
}

/// `NO_COLOR` counts when it is set to anything that is not empty.
fn env_set(name: &str) -> bool {
    env::var_os(name).is_some_and(|value| !value.is_empty())
}

/// The first of `LC_ALL`, `LC_CTYPE` and `LANG` that is set decides the character set.
fn locale_is_utf8() -> bool {
    let locale = ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .find_map(|name| env::var(name).ok().filter(|value| !value.is_empty()));
    locale.is_some_and(|value| {
        let value = value.to_ascii_lowercase();
        value.contains("utf-8") || value.contains("utf8")
    })
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
