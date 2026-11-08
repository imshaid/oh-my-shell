//! Tests that cannot run on this machine: a visible note locally, a failure on CI.
//!
//! A test that returns early looks like any passing test, and on CI that would hide the fact
//! that it never ran. So a skip is only allowed off CI. `crates/omsh-cli/tests/ssh.rs` has a
//! copy of this code (`omsh-cli` cannot depend on this crate: the direction is `tests -> cli`).
//! Keep the two in step.

use std::ffi::OsStr;
use std::io::{self, Write};

/// True for the value GitHub Actions gives the `CI` variable.
fn is_ci(value: Option<&OsStr>) -> bool {
    value.is_some_and(|v| v == "true")
}

/// True if the `CI` environment variable is `true`.
pub fn on_ci() -> bool {
    is_ci(std::env::var_os("CI").as_deref())
}

/// The test `test` cannot run because of `reason`.
///
/// On CI this panics, so the test fails. Elsewhere it prints a note and returns, and the caller
/// returns from the test. The note is written to the real standard error and not with
/// `eprintln!`: libtest captures that macro and drops the output of a passing test, so the note
/// would never be seen without `--nocapture`.
pub fn skip(test: &str, reason: &str) {
    decide(test, reason, on_ci(), &mut io::stderr());
}

fn decide(test: &str, reason: &str, ci: bool, out: &mut dyn Write) {
    assert!(
        !ci,
        "{test} cannot run: {reason}. CI must run every test; see .github/workflows/ci.yml"
    );
    let _ = out.write_all(format!("note: {test} SKIPPED: {reason}\n").as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_ci_a_skip_writes_one_note_and_returns() {
        let mut out = Vec::new();
        decide("some_test", "tool x is missing", false, &mut out);
        assert_eq!(
            String::from_utf8_lossy(&out),
            "note: some_test SKIPPED: tool x is missing\n"
        );
    }

    #[test]
    #[should_panic(expected = "CI must run every test")]
    fn on_ci_a_skip_fails_the_test() {
        decide("some_test", "tool x is missing", true, &mut Vec::new());
    }

    #[test]
    fn only_the_value_true_means_ci() {
        assert!(is_ci(Some(OsStr::new("true"))));
        assert!(!is_ci(None));
        assert!(!is_ci(Some(OsStr::new("false"))));
        assert!(!is_ci(Some(OsStr::new("1"))));
        assert!(!is_ci(Some(OsStr::new(""))));
    }
}
