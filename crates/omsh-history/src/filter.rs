//! Which lines history keeps.
//!
//! A line is not stored if it is blank, if it matches one of the `[history] ignore` patterns,
//! or if it looks like it holds a secret (always, whatever the patterns say: §14.2).

use omsh_secrets as secrets;

/// The rule for what is stored. Cheap to share: the shell and the editor each hold one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filter {
    patterns: Vec<Vec<char>>,
}

impl Filter {
    /// `patterns` are the `[history] ignore` patterns: `*` stands for any text (also none, also
    /// line endings), `?` for one character, everything else for itself. A pattern has to match
    /// the whole command, so `" *"` means "starts with a space".
    #[must_use]
    pub fn new(patterns: &[String]) -> Self {
        Self {
            patterns: patterns
                .iter()
                .map(|pattern| pattern.chars().collect())
                .collect(),
        }
    }

    /// True if `command` may be written to history.
    #[must_use]
    pub fn stores(&self, command: &str) -> bool {
        !self.ignores(command) && !secrets::looks_secret(command)
    }

    /// True if `command` is blank or matches a `[history] ignore` pattern: a line the person
    /// asked not to be remembered. Unlike [`Filter::stores`] this does not look for secrets,
    /// because the AI context masks them instead of dropping the line.
    #[must_use]
    pub fn ignores(&self, command: &str) -> bool {
        if command.trim().is_empty() {
            return true;
        }
        let text: Vec<char> = command.chars().collect();
        self.patterns.iter().any(|pattern| matches(pattern, &text))
    }
}

/// Whether the whole of `text` matches `pattern`.
///
/// The usual way without recursion: remember the last `*` and how much text it has taken; when
/// the match gets stuck, let that `*` take one character more and try again from there.
fn matches(pattern: &[char], text: &[char]) -> bool {
    let (mut p, mut t) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        match pattern.get(p) {
            Some('*') => {
                star = Some((p, t));
                p += 1;
            }
            Some(&c) if c == '?' || c == text[t] => {
                p += 1;
                t += 1;
            }
            _ => match star {
                Some((star_p, star_t)) => {
                    p = star_p + 1;
                    t = star_t + 1;
                    star = Some((star_p, star_t + 1));
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|c| *c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glob(pattern: &str, text: &str) -> bool {
        let pattern: Vec<char> = pattern.chars().collect();
        let text: Vec<char> = text.chars().collect();
        matches(&pattern, &text)
    }

    #[test]
    fn patterns_match_the_whole_text() {
        assert!(glob("ls", "ls"));
        assert!(!glob("ls", "ls -l"));
        assert!(!glob("ls", "als"));
        assert!(glob("", ""));
        assert!(!glob("", "a"));
    }

    #[test]
    fn a_star_stands_for_any_text() {
        assert!(glob(" *", " secret"));
        assert!(glob(" *", " "));
        assert!(!glob(" *", "x secret"));
        assert!(glob("*", ""));
        assert!(glob("*pass*", "mypassword"));
        assert!(glob("a*b*c", "a--b--b--c"));
        assert!(!glob("a*b*c", "a--b--b--"));
        assert!(glob("a**", "a"));
        assert!(glob("*x", "a\nb\nx"), "a star crosses line endings");
    }

    #[test]
    fn a_question_mark_stands_for_one_character() {
        assert!(glob("l?", "ls"));
        assert!(!glob("l?", "l"));
        assert!(!glob("l?", "lss"));
        assert!(glob("?", "ক"), "one character, not one byte");
    }

    #[test]
    fn blank_lines_are_not_stored() {
        let filter = Filter::default();
        assert!(!filter.stores(""));
        assert!(!filter.stores("  \t\n"));
        assert!(filter.stores("ls"));
    }

    #[test]
    fn a_line_that_matches_a_pattern_is_not_stored() {
        let filter = Filter::new(&[" *".to_owned(), "fg".to_owned()]);
        assert!(!filter.stores(" ls"));
        assert!(!filter.stores("fg"));
        assert!(filter.stores("fg %1"));
        assert!(filter.stores("ls"));
    }

    #[test]
    fn a_secret_is_never_stored_whatever_the_patterns_say() {
        for filter in [Filter::default(), Filter::new(&["nothing".to_owned()])] {
            assert!(!filter.stores("export API_KEY=abc"));
            assert!(filter.stores("export PATH=/bin"));
        }
    }

    #[test]
    fn without_patterns_a_leading_space_is_stored() {
        assert!(Filter::default().stores(" ls"));
    }
}
