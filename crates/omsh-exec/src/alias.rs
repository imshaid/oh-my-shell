//! Aliases: `alias ll='ls -l'` makes `ll` stand for `ls -l` where a command name is expected.
//!
//! POSIX replaces aliases while it reads the text, before the grammar is applied, so an alias
//! can hold more than a word (`alias both='a; b'`). That is why this is a step on the text of a
//! line, before [`parse`](omsh_syntax::parse), and not something the executor does to a tree.
//!
//! The words that are in command position come from the highlighter's lexer
//! ([`lex_for_highlight`]), which already tracks that for coloring. It also marks a reserved
//! word as a keyword, so `if` is never replaced.
//!
//! Rules (from POSIX, checked against dash):
//! - Only a whole, unquoted word counts: `\ll` and `'ll'` are not the alias `ll`.
//! - The value is searched for aliases too, except for the ones being replaced, so that
//!   `alias ls='ls --color'` does not loop.
//! - If the value ends in a blank, the word after it may be an alias as well.

use std::borrow::Cow;
use std::collections::BTreeMap;

use omsh_syntax::{HighlightKind, lex_for_highlight};

use crate::shell::Shell;

/// Characters that end a word without being part of it.
const DELIMITERS: &[u8] = b" \t\n;&|()<>";

impl Shell {
    /// Replaces the aliases in `src`. Returns the text itself if there is nothing to replace.
    pub(crate) fn expand_aliases<'a>(&self, src: &'a str) -> Cow<'a, str> {
        if self.aliases.is_empty() {
            return Cow::Borrowed(src);
        }
        match expand_text(&self.aliases, src, &mut Vec::new()) {
            Some(text) => Cow::Owned(text),
            None => Cow::Borrowed(src),
        }
    }
}

/// True if `name` can be the name of an alias.
pub(crate) fn is_alias_name(name: &str) -> bool {
    !name.is_empty()
        && !name.bytes().any(|b| {
            b.is_ascii_whitespace()
                || b.is_ascii_control()
                || b"=/$`\\'\"|&;<>()*?[]{}~#".contains(&b)
        })
}

/// Replaces aliases in `src`. `blocked` holds the names being replaced right now. Returns
/// nothing if the text did not change.
fn expand_text(
    aliases: &BTreeMap<String, String>,
    src: &str,
    blocked: &mut Vec<String>,
) -> Option<String> {
    let bytes = src.as_bytes();
    let mut out = String::new();
    // Everything before this position is already in `out` or was handled with a replacement.
    let mut cursor = 0;
    let mut changed = false;
    let spans = lex_for_highlight(src).spans;
    for (index, piece) in spans.iter().enumerate() {
        if piece.kind != HighlightKind::Command || piece.span.start < cursor {
            continue;
        }
        let (start, end) = (piece.span.start, piece.span.end);
        // Something right in front of the piece that is not an operator belongs to the same
        // word: the `$(:)` of `$(:)ll`, the quotes of `"x"ll`.
        let joined_to_before = index > 0
            && spans[index - 1].span.end == start
            && !matches!(
                spans[index - 1].kind,
                HighlightKind::Operator | HighlightKind::Separator
            )
            // The `$(` that opens a substitution is no part of the word inside it.
            && bytes[start - 1] != b'(';
        if joined_to_before || !whole_word(bytes, end, bytes.len()) {
            continue;
        }
        let Some(mut value) = lookup(aliases, &src[start..end], blocked) else {
            continue;
        };
        changed = true;
        out.push_str(&src[cursor..start]);
        cursor = end;
        let mut name = src[start..end].to_owned();
        loop {
            blocked.push(name);
            let expanded = expand_text(aliases, value, blocked);
            blocked.pop();
            out.push_str(expanded.as_deref().unwrap_or(value));
            // A value that ends in a blank lets the next word be an alias too.
            if !value.ends_with([' ', '\t']) {
                break;
            }
            let mut at = cursor;
            while bytes.get(at).is_some_and(|b| matches!(b, b' ' | b'\t')) {
                at += 1;
            }
            let word_end = bytes[at..]
                .iter()
                .position(|b| DELIMITERS.contains(b))
                .map_or(bytes.len(), |n| at + n);
            let Some(next) = lookup(aliases, &src[at..word_end], blocked) else {
                break;
            };
            out.push_str(&src[cursor..at]);
            name = src[at..word_end].to_owned();
            cursor = word_end;
            value = next;
        }
    }
    if !changed {
        return None;
    }
    out.push_str(&src[cursor..]);
    Some(out)
}

/// The value of the alias `word`, unless it is not one or is being replaced already.
fn lookup<'a>(
    aliases: &'a BTreeMap<String, String>,
    word: &str,
    blocked: &[String],
) -> Option<&'a str> {
    if blocked.iter().any(|b| b == word) {
        return None;
    }
    aliases.get(word).map(String::as_str)
}

/// True if the word that ends at `end` ends there: the next byte is not part of it. The
/// highlighter gives a quoted part of a word its own span, so `ll"x"` has a command span `ll`
/// that is not the whole word.
fn whole_word(bytes: &[u8], end: usize, _len: usize) -> bool {
    end >= bytes.len() || DELIMITERS.contains(&bytes[end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LineResult;
    use crate::vars::Variables;

    fn expand(aliases: &[(&str, &str)], src: &str) -> String {
        let mut shell = Shell::with_variables(Variables::default());
        for (name, value) in aliases {
            shell
                .aliases
                .insert((*name).to_owned(), (*value).to_owned());
        }
        shell.expand_aliases(src).into_owned()
    }

    #[test]
    fn a_command_word_is_replaced() {
        assert_eq!(expand(&[("ll", "ls -l")], "ll /tmp"), "ls -l /tmp");
        assert_eq!(
            expand(&[("ll", "ls -l")], "echo a; ll\n"),
            "echo a; ls -l\n"
        );
        assert_eq!(
            expand(&[("ll", "ls -l")], "a | ll && ll"),
            "a | ls -l && ls -l"
        );
        assert_eq!(expand(&[("ll", "ls -l")], "echo $(ll)"), "echo $(ls -l)");
        assert_eq!(
            expand(&[("ll", "ls -l")], "if ll; then ll; fi"),
            "if ls -l; then ls -l; fi"
        );
    }

    /// `;` and `&` have their own highlight kind (S14a), apart from `|` and `&&`. A word that
    /// touches one of them is still a new command, not a part of the word before it.
    #[test]
    fn a_command_right_after_a_separator_is_replaced() {
        let a = [("ll", "ls -l")];
        assert_eq!(expand(&a, "echo a;ll"), "echo a;ls -l");
        assert_eq!(expand(&a, "echo a&ll"), "echo a&ls -l");
        assert_eq!(
            expand(&a, "case x in x) ll;;esac"),
            "case x in x) ls -l;;esac"
        );
        assert_eq!(expand(&a, "a|ll&&ll"), "a|ls -l&&ls -l");
    }

    #[test]
    fn an_argument_a_quoted_word_and_a_keyword_are_left_alone() {
        let a = [("ll", "ls -l"), ("if", "echo")];
        assert_eq!(expand(&a, "echo ll"), "echo ll");
        assert_eq!(expand(&a, r"\ll"), r"\ll");
        assert_eq!(expand(&a, "'ll'"), "'ll'");
        assert_eq!(expand(&a, "ll\"x\""), "ll\"x\"");
        assert_eq!(expand(&a, "if true; then :; fi"), "if true; then :; fi");
        assert_eq!(expand(&a, "echo x > ll"), "echo x > ll");
    }

    #[test]
    fn something_joined_to_the_front_of_the_word_stops_it_being_an_alias() {
        let a = [("ll", "ls -l")];
        assert_eq!(expand(&a, "$(:)ll hi"), "$(:)ll hi");
        assert_eq!(expand(&a, "\"x\"ll hi"), "\"x\"ll hi");
        assert_eq!(expand(&a, "x\\ll hi"), "x\\ll hi");
        assert_eq!(expand(&a, "(ll)"), "(ls -l)");
        assert_eq!(
            expand(&a, "case x in x)ll;; esac"),
            "case x in x)ls -l;; esac"
        );
    }

    #[test]
    fn an_alias_is_not_replaced_inside_itself() {
        assert_eq!(expand(&[("ls", "ls --color")], "ls"), "ls --color");
        assert_eq!(expand(&[("a", "b"), ("b", "a")], "a"), "a");
        assert_eq!(expand(&[("a", "b x"), ("b", "c")], "a"), "c x");
    }

    #[test]
    fn a_value_ending_in_a_blank_lets_the_next_word_be_an_alias() {
        let a = [("sudo", "sudo "), ("ll", "ls -l")];
        assert_eq!(expand(&a, "sudo ll /x"), "sudo  ls -l /x");
        let a = [("e", "echo"), ("ll", "ls -l")];
        assert_eq!(expand(&a, "e ll"), "echo ll");
    }

    #[test]
    fn an_alias_can_hold_more_than_a_word() {
        assert_eq!(expand(&[("both", "a; b")], "both && c"), "a; b && c");
    }

    #[test]
    fn prepare_unaliased_keeps_the_text_that_prepare_would_replace() {
        let mut shell = Shell::with_variables(Variables::default());
        shell.aliases.insert("ll".to_owned(), "ls -l".to_owned());
        let replaced = shell.prepare("ll /x", true).unwrap();
        assert_eq!(replaced.text(), "ls -l /x");
        let kept = shell.prepare_unaliased("ll /x", true).unwrap();
        assert_eq!(kept.text(), "ll /x");
        // An unfinished command is still reported as such, and a bad one still rejected.
        assert!(matches!(
            *shell.prepare_unaliased("echo 'x", false).unwrap_err(),
            LineResult::Incomplete
        ));
        assert!(matches!(
            *shell.prepare_unaliased("echo )", true).unwrap_err(),
            LineResult::Rejected(_)
        ));
    }

    #[test]
    fn no_aliases_borrows_the_text() {
        let shell = Shell::with_variables(Variables::default());
        assert!(matches!(shell.expand_aliases("ll"), Cow::Borrowed(_)));
    }

    #[test]
    fn names() {
        assert!(is_alias_name("ll"));
        assert!(is_alias_name("l.s-x@!%,"));
        assert!(!is_alias_name(""));
        assert!(!is_alias_name("a b"));
        assert!(!is_alias_name("a=b"));
        assert!(!is_alias_name("a/b"));
    }
}
