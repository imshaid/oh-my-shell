//! The fallback for a person who cannot press arrow keys: options printed with numbers and
//! letters, and a line read from input. It is what runs when the input or the output is not a
//! terminal, in `-c`, in tests and under `TERM=dumb` (spec section 1, rule 7).
//!
//! It writes plain text only: no escape, no cursor control, no box.

use std::io::{self, BufRead, Write};

/// How many wrong answers are put up with before the question is taken as cancelled.
pub const TRIES: usize = 3;

/// One option of a numbered question.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NumberedOption {
    /// What it says.
    pub label: String,
    /// The letter that picks it, if any.
    pub shortcut: Option<char>,
}

/// What a typed answer means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    /// This option (index from 0).
    Choice(usize),
    /// Not an answer.
    Invalid,
}

/// A numbered question.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Numbered {
    prompt: String,
    options: Vec<NumberedOption>,
    default: Option<usize>,
}

impl Numbered {
    /// A question. `default` is what an empty answer picks (the safe option); `None` means an
    /// empty answer is not an answer.
    #[must_use]
    pub fn new(prompt: &str, options: Vec<NumberedOption>, default: Option<usize>) -> Self {
        let default = default.filter(|index| *index < options.len());
        Self {
            prompt: prompt.to_owned(),
            options,
            default,
        }
    }

    /// A question from plain labels: no letters and no default.
    #[must_use]
    pub fn from_labels(prompt: &str, labels: &[&str]) -> Self {
        Self::new(
            prompt,
            labels
                .iter()
                .map(|label| NumberedOption {
                    label: (*label).to_owned(),
                    shortcut: None,
                })
                .collect(),
            None,
        )
    }

    /// The text shown before the answer is read: the question, then one numbered line for each
    /// option (`2) Run anyway [y]`, and `(default)` on the one an empty answer picks), then the
    /// line that asks.
    #[must_use]
    pub fn render(&self) -> Vec<String> {
        let mut lines = vec![crate::text::harmless(&self.prompt)];
        for (index, option) in self.options.iter().enumerate() {
            let mut line = format!("  {}) {}", index + 1, crate::text::harmless(&option.label));
            if let Some(letter) = option.shortcut {
                line.push_str(&format!(
                    " [{}]",
                    crate::text::harmless(&letter.to_string())
                ));
            }
            if self.default == Some(index) {
                line.push_str(" (default)");
            }
            lines.push(line);
        }
        lines.push(self.ask_line());
        lines
    }

    fn ask_line(&self) -> String {
        let count = self.options.len();
        match self.default {
            Some(default) => format!("Choose 1-{count} [{}]: ", default + 1),
            None => format!("Choose 1-{count}: "),
        }
    }

    /// What `answer` means: empty is the default (if there is one), a number from 1 to the
    /// count, or the letter of an option in either case.
    #[must_use]
    pub fn parse(&self, answer: &str) -> Answer {
        let answer = answer.trim();
        if answer.is_empty() {
            return self.default.map_or(Answer::Invalid, Answer::Choice);
        }
        if answer.bytes().all(|b| b.is_ascii_digit()) {
            return match answer.parse::<usize>() {
                Ok(number) if (1..=self.options.len()).contains(&number) => {
                    Answer::Choice(number - 1)
                }
                _ => Answer::Invalid,
            };
        }
        let mut chars = answer.chars();
        if let (Some(letter), None) = (chars.next(), chars.next()) {
            let found = self.options.iter().position(|option| {
                option
                    .shortcut
                    .is_some_and(|own| own.to_lowercase().eq(letter.to_lowercase()))
            });
            if let Some(index) = found {
                return Answer::Choice(index);
            }
        }
        Answer::Invalid
    }

    /// Writes the question to `output` and reads answers from `input` until one is valid.
    /// `None` when the input ends, when [`TRIES`] answers in a row were wrong, or when there are
    /// no options: nothing is chosen, which is the safe result.
    ///
    /// # Errors
    ///
    /// When reading or writing fails.
    pub fn ask<R: BufRead, W: Write>(
        &self,
        input: &mut R,
        output: &mut W,
    ) -> io::Result<Option<usize>> {
        if self.options.is_empty() {
            return Ok(None);
        }
        let lines = self.render();
        let (last, shown) = lines
            .split_last()
            .map_or(("", &[][..]), |(l, s)| (l.as_str(), s));
        for line in shown {
            writeln!(output, "{line}")?;
        }
        for _ in 0..TRIES {
            write!(output, "{last}")?;
            output.flush()?;
            let mut answer = String::new();
            if input.read_line(&mut answer)? == 0 {
                writeln!(output)?;
                return Ok(None);
            }
            if let Answer::Choice(index) = self.parse(&answer) {
                return Ok(Some(index));
            }
            writeln!(output, "That is not one of the choices.")?;
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn run_it() -> Numbered {
        Numbered::new(
            "Run it?",
            vec![
                NumberedOption {
                    label: "Move to trash".into(),
                    shortcut: Some('t'),
                },
                NumberedOption {
                    label: "Run anyway".into(),
                    shortcut: Some('y'),
                },
                NumberedOption {
                    label: "Cancel".into(),
                    shortcut: Some('n'),
                },
            ],
            Some(2),
        )
    }

    fn ask(question: &Numbered, typed: &str) -> (Option<usize>, String) {
        let mut input = Cursor::new(typed.as_bytes().to_vec());
        let mut output = Vec::new();
        let chosen = question.ask(&mut input, &mut output).unwrap();
        (chosen, String::from_utf8(output).unwrap())
    }

    #[test]
    fn the_question_is_plain_numbered_text_with_the_default_marked() {
        assert_eq!(
            run_it().render(),
            [
                "Run it?",
                "  1) Move to trash [t]",
                "  2) Run anyway [y]",
                "  3) Cancel [n] (default)",
                "Choose 1-3 [3]: ",
            ]
        );
    }

    #[test]
    fn a_question_without_a_default_does_not_name_one() {
        let question = Numbered::from_labels("Which?", &["a", "b"]);
        assert_eq!(
            question.render(),
            ["Which?", "  1) a", "  2) b", "Choose 1-2: "]
        );
    }

    #[test]
    fn answers_are_numbers_letters_or_empty_for_the_default() {
        let q = run_it();
        assert_eq!(q.parse("1"), Answer::Choice(0));
        assert_eq!(q.parse(" 2 \n"), Answer::Choice(1));
        assert_eq!(q.parse("T"), Answer::Choice(0));
        assert_eq!(q.parse("n"), Answer::Choice(2));
        assert_eq!(q.parse(""), Answer::Choice(2));
        for bad in ["0", "4", "99999999999999999999", "x", "ty", "-1", "1 2"] {
            assert_eq!(q.parse(bad), Answer::Invalid, "{bad:?}");
        }
        assert_eq!(
            Numbered::from_labels("p", &["a"]).parse(""),
            Answer::Invalid
        );
    }

    #[test]
    fn asking_reads_a_line_and_returns_the_choice() {
        let (chosen, written) = ask(&run_it(), "2\n");
        assert_eq!(chosen, Some(1));
        assert!(written.starts_with("Run it?\n"));
        assert!(!written.contains('\x1b'), "plain text only");
    }

    #[test]
    fn an_empty_line_takes_the_safe_default() {
        assert_eq!(ask(&run_it(), "\n").0, Some(2));
    }

    #[test]
    fn a_wrong_answer_is_asked_again_and_three_wrong_ones_cancel() {
        let (chosen, written) = ask(&run_it(), "nope\n7\ny\n");
        assert_eq!(chosen, Some(1));
        assert_eq!(written.matches("not one of the choices").count(), 2);
        let (chosen, _) = ask(&run_it(), "a\nb\nc\nt\n");
        assert_eq!(chosen, None, "the fourth line is never read");
    }

    #[test]
    fn the_end_of_input_cancels_and_never_picks_the_default() {
        let (chosen, _) = ask(&run_it(), "");
        assert_eq!(chosen, None);
    }

    #[test]
    fn no_options_is_nothing_chosen() {
        let question = Numbered::new("p", vec![], Some(0));
        assert_eq!(ask(&question, "1\n").0, None);
    }

    #[test]
    fn user_text_in_a_label_cannot_carry_an_escape() {
        let question = Numbered::from_labels("p", &["rm \x1b[2J"]);
        assert!(question.render().iter().all(|line| !line.contains('\x1b')));
    }
}
