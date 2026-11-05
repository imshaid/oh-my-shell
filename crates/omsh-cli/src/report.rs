//! Showing error reports to the user.

use std::env;
use std::io::{self, IsTerminal, Write};

use miette::{Diagnostic, GraphicalReportHandler, GraphicalTheme, NarratableReportHandler};

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
    /// Picks a style for stderr as it is now.
    #[must_use]
    pub fn detect() -> Self {
        if !io::stderr().is_terminal() {
            return Self::Plain;
        }
        let dumb = env::var("TERM").is_ok_and(|term| term == "dumb");
        Self::Graphical {
            color: !dumb && !env_set("NO_COLOR"),
            unicode: !dumb && locale_is_utf8(),
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
