//! Tilde expansion: `~` and `~user`.

use std::os::unix::ffi::OsStrExt;

use nix::unistd::User;

use crate::vars::Variables;

/// A tilde prefix found in a piece of text, and the directory it stands for.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Replacement {
    /// Where the `~` is.
    pub start: usize,
    /// Just after the last character of the user name.
    pub end: usize,
    /// The home directory.
    pub home: Vec<u8>,
}

/// The home directory for the text after a `~`: `$HOME` if it is empty, else that user's.
/// Nothing if there is none, in which case the `~` stays as it is.
fn home_for(prefix: &[u8], vars: &Variables) -> Option<Vec<u8>> {
    if prefix.is_empty() {
        return vars.get("HOME").map(<[u8]>::to_vec);
    }
    let name = std::str::from_utf8(prefix).ok()?;
    let user = User::from_name(name).ok().flatten()?;
    Some(user.dir.as_os_str().as_bytes().to_vec())
}

/// Finds the tilde prefixes in `text`, which is an unquoted literal piece of a word.
///
/// * `at_word_start`: the text starts the word, so a `~` at its first byte counts.
/// * `assignment`: this is the value of `NAME=value`, where a `~` after a `:` counts too, and
///   `:` also ends the prefix.
/// * `last_piece`: no other piece follows in the word. A prefix is a user name up to the next
///   `/`, so if the text ends without one, the name goes on into the next piece (`~$x`,
///   `~"a"`), which makes it not a tilde prefix at all.
pub(crate) fn find(
    text: &str,
    at_word_start: bool,
    assignment: bool,
    last_piece: bool,
    vars: &Variables,
) -> Vec<Replacement> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    for (j, _) in bytes.iter().enumerate().filter(|(_, b)| **b == b'~') {
        let starts = (j == 0 && at_word_start) || (assignment && j > 0 && bytes[j - 1] == b':');
        if !starts {
            continue;
        }
        let rest = &bytes[j + 1..];
        let end = match rest
            .iter()
            .position(|&b| b == b'/' || (assignment && b == b':'))
        {
            Some(n) => j + 1 + n,
            None if last_piece => bytes.len(),
            None => continue,
        };
        if let Some(home) = home_for(&bytes[j + 1..end], vars) {
            out.push(Replacement {
                start: j,
                end,
                home,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars() -> Variables {
        Variables::from_pairs([("HOME", "/home/me")])
    }

    fn one(text: &str, start: bool, assignment: bool, last: bool) -> Vec<(usize, usize, String)> {
        find(text, start, assignment, last, &vars())
            .into_iter()
            .map(|r| (r.start, r.end, String::from_utf8(r.home).unwrap()))
            .collect()
    }

    #[test]
    fn a_lone_tilde_is_home() {
        assert_eq!(one("~", true, false, true), [(0, 1, "/home/me".to_owned())]);
        assert_eq!(
            one("~/bin", true, false, true),
            [(0, 1, "/home/me".to_owned())]
        );
    }

    #[test]
    fn a_tilde_not_at_the_start_is_text() {
        assert!(one("a~", true, false, true).is_empty());
        assert!(one("~", false, false, true).is_empty());
    }

    #[test]
    fn a_user_name_is_looked_up() {
        let found = one("~root/x", true, false, true);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].0, found[0].1), (0, 5));
        assert!(one("~no-such-user-omsh", true, false, true).is_empty());
    }

    #[test]
    fn a_name_that_goes_on_into_the_next_piece_is_not_a_prefix() {
        assert!(one("~", true, false, false).is_empty());
        assert_eq!(one("~/", true, false, false).len(), 1);
    }

    #[test]
    fn nothing_happens_without_home() {
        assert!(find("~", true, false, true, &Variables::default()).is_empty());
    }

    #[test]
    fn an_assignment_also_expands_after_each_colon() {
        let found = one("~/a:~/b:~root", true, true, true);
        assert_eq!(found.len(), 3);
        assert_eq!(found[0].0, 0);
        assert_eq!(found[1].0, 4);
        assert_eq!(found[2].0, 8);
        // Outside an assignment a colon means nothing.
        assert_eq!(one("~/a:~/b", true, false, true).len(), 1);
    }

    #[test]
    fn in_an_assignment_a_colon_ends_the_prefix() {
        let found = one("~:x", true, true, true);
        assert_eq!(found, [(0, 1, "/home/me".to_owned())]);
    }
}
