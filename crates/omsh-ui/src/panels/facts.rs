//! What the AI panels know about one call, as plain data (UI upgrade, phase 4).
//!
//! `omsh-ui` does not know the AI crate: the shell fills a [`CallFacts`] from what the provider
//! reported. **A number the provider did not report is `None` and is drawn as a dash, never as
//! 0 and never as a guess.**

use std::time::Duration;

use crate::caps::Caps;

/// One finished AI call.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CallFacts {
    /// The provider's name as a person says it (`Ollama`, `Gemini`).
    pub provider: String,
    pub model: String,
    /// The reasoning effort, when the request asked for one.
    pub effort: Option<String>,
    /// How long the call took, measured by the shell.
    pub time: Option<Duration>,
    pub input: Option<u32>,
    pub thinking: Option<u32>,
    /// Tokens of the answer itself, without the thinking.
    pub output: Option<u32>,
    pub cached: Option<u32>,
    /// The total as the provider (or the client library) gives it.
    pub total: Option<u32>,
    /// Tokens a second, **derived** from the provider's count and generation time.
    pub tokens_per_sec: Option<f64>,
    /// What the next call will roughly start with: the last call's total. Approximate.
    pub context_used: Option<u32>,
    /// The size of the window, when it is known.
    pub context_limit: Option<u32>,
    /// Where the limit came from, for the note under the details table.
    pub limit_note: Option<String>,
}

/// The few characters that differ between a Unicode and an ASCII terminal.
#[derive(Clone, Copy, Debug)]
pub struct Marks {
    /// A number that is not known.
    pub dash: &'static str,
    /// "About".
    pub approx: &'static str,
    /// Between two facts on one line.
    pub sep: &'static str,
    /// The multiplication sign.
    pub times: &'static str,
}

impl Marks {
    #[must_use]
    pub const fn of(caps: &Caps) -> Self {
        if caps.unicode {
            Self {
                dash: "–",
                approx: "≈",
                sep: " · ",
                times: "×",
            }
        } else {
            Self {
                dash: "-",
                approx: "~",
                sep: ", ",
                times: "x",
            }
        }
    }
}

/// `1204` as `1,204`.
#[must_use]
pub fn group(n: u32) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A count, or the dash for one that is not known.
#[must_use]
pub fn count_or_dash(n: Option<u32>, marks: Marks) -> String {
    n.map_or_else(|| marks.dash.to_owned(), group)
}

/// `3.2 s`.
#[must_use]
pub fn seconds(time: Duration) -> String {
    format!("{:.1} s", time.as_secs_f64())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_grouped_by_three() {
        for (n, text) in [
            (0, "0"),
            (999, "999"),
            (1000, "1,000"),
            (1_234_567, "1,234,567"),
        ] {
            assert_eq!(group(n), text);
        }
    }

    #[test]
    fn an_unknown_count_is_a_dash_and_never_zero() {
        let unicode = Marks::of(&crate::panels::test_support::caps(
            omsh_config::Motion::Off,
            true,
        ));
        assert_eq!(count_or_dash(None, unicode), unicode.dash);
        assert_eq!(
            count_or_dash(Some(0), unicode),
            "0",
            "a real zero stays a zero"
        );
    }

    #[test]
    fn ascii_has_its_own_marks() {
        let marks = Marks::of(&crate::panels::test_support::caps(
            omsh_config::Motion::Off,
            false,
        ));
        assert_eq!(
            (marks.dash, marks.approx, marks.sep, marks.times),
            ("-", "~", ", ", "x")
        );
        assert!(marks.dash.is_ascii() && marks.approx.is_ascii());
    }
}
