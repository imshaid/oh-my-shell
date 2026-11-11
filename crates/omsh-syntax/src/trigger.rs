//! The AI trigger check (blueprint §4.2 step 2, §13.2).
//!
//! A line that starts with `?` (after blanks) and has a blank or the end of the line right
//! after it is a request to the AI layer, not shell syntax. The check works on the text alone,
//! **before** alias replacement and parsing, because a request is natural language: `? what's
//! here` has an open quote and `? list *.txt files` has a glob, and neither may be read as shell
//! code.
//!
//! `?x` and `?.txt` stay ordinary words (a glob), and `echo ?` is an ordinary command: only the
//! first word counts.

/// What a line that starts with `?` asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger<'a> {
    /// `? <request>`: plan a task. The text may be empty (`?` alone).
    Plan(&'a str),
    /// `?? <command or topic>`: explain without running. Not available yet.
    Explain(&'a str),
}

/// Whether `line` is an AI trigger, and what comes after it (blanks at both ends removed).
#[must_use]
pub fn trigger(line: &str) -> Option<Trigger<'_>> {
    let line = line.trim_start_matches([' ', '\t']);
    let ends_word = |rest: &str| rest.is_empty() || rest.starts_with([' ', '\t', '\n']);
    if let Some(rest) = line.strip_prefix("??")
        && ends_word(rest)
    {
        return Some(Trigger::Explain(rest.trim()));
    }
    let rest = line.strip_prefix('?')?;
    ends_word(rest).then(|| Trigger::Plan(rest.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_question_mark_and_a_blank_start_a_plan() {
        assert_eq!(
            trigger("? list the big files"),
            Some(Trigger::Plan("list the big files"))
        );
        assert_eq!(trigger("?\tx"), Some(Trigger::Plan("x")));
        assert_eq!(trigger("   ?  spaced  "), Some(Trigger::Plan("spaced")));
        assert_eq!(trigger("?"), Some(Trigger::Plan("")));
        assert_eq!(trigger("?\n"), Some(Trigger::Plan("")));
    }

    #[test]
    fn natural_language_is_not_read_as_shell_code() {
        assert_eq!(trigger("? what's here"), Some(Trigger::Plan("what's here")));
        assert_eq!(trigger("? a | b && ("), Some(Trigger::Plan("a | b && (")));
    }

    #[test]
    fn two_question_marks_explain() {
        assert_eq!(trigger("?? tar -xzf"), Some(Trigger::Explain("tar -xzf")));
        assert_eq!(trigger("??"), Some(Trigger::Explain("")));
    }

    #[test]
    fn globs_and_later_words_are_not_triggers() {
        for line in [
            "?x", "?.txt", "???", "??x", "echo ?", "ls ? x", "", "ls", "\\? x", "'?' x",
        ] {
            assert_eq!(trigger(line), None, "{line:?}");
        }
    }
}
