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
//!
//! `cmd | ? question` (S24) is the one trigger that is not at the start. A `|` counts only when it
//! is a real pipe operator: the text in front of it must be a finished command that parses on its
//! own. That rules out a `|` inside quotes, `$( )`, `if ... then` and the like (all unfinished in
//! front of the `|`), and `||` and `>|` are skipped by their neighbors.

use crate::parser::parse;

/// What a line that starts with `?` asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger<'a> {
    /// `? <request>`: plan a task. The text may be empty (`?` alone).
    Plan(&'a str),
    /// `?? <command or topic>`: explain without running. Not available yet.
    ///
    /// With no text (`??` alone) it asks for the last failed command to be explained.
    Explain(&'a str),
    /// `cmd | ? question`: run `command`, send its output to the AI with `question`.
    Pipe {
        /// The text in front of the `|`, blanks removed at both ends.
        command: &'a str,
        /// The text after the `?`, blanks removed. May be empty.
        question: &'a str,
        /// Byte index of the `|` in the line.
        at: usize,
    },
}

/// Whether `line` is an AI trigger, and what comes after it (blanks at both ends removed).
#[must_use]
pub fn trigger(line: &str) -> Option<Trigger<'_>> {
    let full = line;
    let line = line.trim_start_matches([' ', '\t']);
    let ends_word = |rest: &str| rest.is_empty() || rest.starts_with([' ', '\t', '\n']);
    if let Some(rest) = line.strip_prefix("??")
        && ends_word(rest)
    {
        return Some(Trigger::Explain(rest.trim()));
    }
    if let Some(rest) = line.strip_prefix('?')
        && ends_word(rest)
    {
        return Some(Trigger::Plan(rest.trim()));
    }
    let skipped = full.len() - line.len();
    match pipe_trigger(line)? {
        Trigger::Pipe {
            command,
            question,
            at,
        } => Some(Trigger::Pipe {
            command,
            question,
            at: at + skipped,
        }),
        other => Some(other),
    }
}

/// `cmd | ? question`, if `line` is one.
fn pipe_trigger(line: &str) -> Option<Trigger<'_>> {
    // Cheap exit for the common case: this runs on every typed key.
    if !line.contains('?') {
        return None;
    }
    let bytes = line.as_bytes();
    for (at, _) in line.match_indices('|') {
        if at == 0 || matches!(bytes[at - 1], b'|' | b'>') || bytes.get(at + 1) == Some(&b'|') {
            continue;
        }
        let after = line[at + 1..].trim_start_matches([' ', '\t']);
        let Some(rest) = after.strip_prefix('?') else {
            continue;
        };
        if !(rest.is_empty() || rest.starts_with([' ', '\t', '\n'])) {
            continue;
        }
        let command = line[..at].trim();
        if command.is_empty() || command.ends_with(['&', ';']) {
            continue;
        }
        if parse(command).is_ok_and(|list| !list.items.is_empty()) {
            return Some(Trigger::Pipe {
                command,
                question: rest.trim(),
                at,
            });
        }
    }
    None
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
    fn a_pipe_into_a_question_mark_is_a_trigger() {
        let pipe = |line: &str| match trigger(line) {
            Some(Trigger::Pipe {
                command,
                question,
                at,
            }) => Some((command.to_owned(), question.to_owned(), at)),
            _ => None,
        };
        assert_eq!(
            pipe("ls -l | ? which is biggest"),
            Some(("ls -l".into(), "which is biggest".into(), 6))
        );
        assert_eq!(pipe("a|?q").map(|p| p.1), None, "no blank after the ?");
        assert_eq!(pipe("a |?"), Some(("a".into(), String::new(), 2)));
        assert_eq!(
            pipe("  a | ? q").map(|p| p.2),
            Some(4),
            "index in the full line"
        );
        assert_eq!(pipe("a |\t? q"), Some(("a".into(), "q".into(), 2)));
        assert_eq!(
            pipe("cat f | grep x | ? what's this"),
            Some(("cat f | grep x".into(), "what's this".into(), 15))
        );
        assert_eq!(
            pipe("echo \"x | y\" | ? q"),
            Some(("echo \"x | y\"".into(), "q".into(), 13))
        );
        assert_eq!(pipe("a && b | ? q").map(|p| p.0), Some("a && b".to_owned()));
        // The first `|` that can be a trigger wins.
        assert_eq!(pipe("a | ? b | ? c").map(|p| p.1), Some("b | ? c".into()));
    }

    #[test]
    fn a_question_mark_that_is_not_a_pipe_target_is_not_a_trigger() {
        for line in [
            "echo 'a | ? b'",
            "echo \"a | ? b\"",
            "echo $(a | ? b)",
            "echo `a | ? b`",
            "if x; then a | ? q",
            "a || ? b",
            "x >| ? f",
            "ls | '?' x",
            "ls | \\? x",
            "ls | ?x",
            "ls | ?.txt",
            "echo \\| ? q",
            "| ? q",
            "a && | ? q",
            "a; | ? q",
            "a & | ? q",
            "# note | ? q",
            "cat <<EOF | ? q",
            "ls | wc",
            "echo ?",
        ] {
            assert_eq!(trigger(line), None, "{line:?}");
        }
    }

    #[test]
    fn explain_and_plan_come_before_pipe() {
        assert_eq!(trigger("?? x | ? y"), Some(Trigger::Explain("x | ? y")));
        assert_eq!(trigger("? a | ? b"), Some(Trigger::Plan("a | ? b")));
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
