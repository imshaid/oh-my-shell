//! Decides whether Enter runs the line or adds a new line to it.

use omsh_syntax::parse;
use reedline::{ValidationResult, Validator};

/// True if `text` is a command that is not finished yet: an open quote, an `if` without its
/// `fi`, a here-document without its end word, a trailing backslash. More typed text could fix
/// it. A line with a real mistake is *not* incomplete: it is run, and the shell reports it.
#[must_use]
pub fn is_incomplete(text: &str) -> bool {
    parse(text).is_err_and(|failure| failure.is_incomplete())
}

/// Plugs [`is_incomplete`] into the editor: while it holds, Enter inserts a line break.
pub struct ShellValidator;

impl Validator for ShellValidator {
    fn validate(&self, line: &str) -> ValidationResult {
        if is_incomplete(line) {
            ValidationResult::Incomplete
        } else {
            ValidationResult::Complete
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finished_commands_are_complete() {
        for line in [
            "",
            "echo hi",
            "ls | wc -l",
            "if true; then :; fi",
            "echo 'a b'",
        ] {
            assert!(!is_incomplete(line), "{line:?}");
        }
    }

    #[test]
    fn unfinished_commands_wait_for_more() {
        for line in [
            "if true; then",
            "echo 'abc",
            "echo \"abc",
            "echo hi \\",
            "cat <<EOF",
            "cat <<EOF\nbody",
            "while true; do",
            "f() {",
            "echo $(",
            "ls |",
            "a &&",
        ] {
            assert!(is_incomplete(line), "{line:?}");
        }
    }

    #[test]
    fn a_here_document_is_complete_once_it_has_its_end_word() {
        assert!(!is_incomplete("cat <<EOF\nbody\nEOF"));
    }

    #[test]
    fn a_real_mistake_is_not_waited_for() {
        // The shell must see these to report them; Enter must not swallow them.
        for line in ["fi", "echo )", "; ls", "if true; then fi done"] {
            assert!(!is_incomplete(line), "{line:?}");
        }
    }

    #[test]
    fn the_validator_agrees() {
        assert!(matches!(
            ShellValidator.validate("if true; then"),
            ValidationResult::Incomplete
        ));
        assert!(matches!(
            ShellValidator.validate("echo hi"),
            ValidationResult::Complete
        ));
    }
}
