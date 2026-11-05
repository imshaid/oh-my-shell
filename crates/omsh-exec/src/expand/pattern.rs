//! Shell patterns: `*`, `?`, `[...]`.
//!
//! The matcher works on bytes, because file names and variable values are bytes. It reads
//! UTF-8 where it can, so `?` matches one whole character (a Bangla letter is three bytes),
//! and a byte that is not valid UTF-8 counts as one character of its own.
//!
//! A pattern is built from the text and a mask that says which characters are "active". A
//! quoted `*` is not active: it matches only a star.

/// Added to a byte that is not valid UTF-8, to give it a number no real character has.
const INVALID_BASE: u32 = 0x11_0000;

/// Reads one character at the start of `s`, which must not be empty. Returns its number and
/// how many bytes it took.
pub(crate) fn unit_at(s: &[u8]) -> (u32, usize) {
    let first = s[0];
    if first < 0x80 {
        return (u32::from(first), 1);
    }
    let len = match first {
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        _ => return (INVALID_BASE + u32::from(first), 1),
    };
    if let Some(bytes) = s.get(..len)
        && let Ok(text) = std::str::from_utf8(bytes)
        && let Some(c) = text.chars().next()
    {
        return (u32::from(c), len);
    }
    (INVALID_BASE + u32::from(first), 1)
}

/// The offsets in `text` where a character starts, and the end.
pub(crate) fn boundaries(text: &[u8]) -> Vec<usize> {
    let mut out = Vec::with_capacity(text.len() + 1);
    let mut at = 0;
    while at < text.len() {
        out.push(at);
        at += unit_at(&text[at..]).1;
    }
    out.push(text.len());
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    Alpha,
    Digit,
    Alnum,
    Upper,
    Lower,
    Space,
    Blank,
    Punct,
    Print,
    Graph,
    Cntrl,
    Xdigit,
}

impl Class {
    fn from_name(name: &[u8]) -> Option<Self> {
        Some(match name {
            b"alpha" => Self::Alpha,
            b"digit" => Self::Digit,
            b"alnum" => Self::Alnum,
            b"upper" => Self::Upper,
            b"lower" => Self::Lower,
            b"space" => Self::Space,
            b"blank" => Self::Blank,
            b"punct" => Self::Punct,
            b"print" => Self::Print,
            b"graph" => Self::Graph,
            b"cntrl" => Self::Cntrl,
            b"xdigit" => Self::Xdigit,
            _ => return None,
        })
    }

    /// Classes are ASCII only for now.
    fn contains(self, unit: u32) -> bool {
        let Some(c) = u8::try_from(unit).ok().filter(u8::is_ascii) else {
            return false;
        };
        match self {
            Self::Alpha => c.is_ascii_alphabetic(),
            Self::Digit => c.is_ascii_digit(),
            Self::Alnum => c.is_ascii_alphanumeric(),
            Self::Upper => c.is_ascii_uppercase(),
            Self::Lower => c.is_ascii_lowercase(),
            Self::Space => matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r'),
            Self::Blank => matches!(c, b' ' | b'\t'),
            Self::Punct => c.is_ascii_punctuation(),
            Self::Print => c.is_ascii_graphic() || c == b' ',
            Self::Graph => c.is_ascii_graphic(),
            Self::Cntrl => c.is_ascii_control(),
            Self::Xdigit => c.is_ascii_hexdigit(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Member {
    One(u32),
    Range(u32, u32),
    Class(Class),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Set {
    negate: bool,
    members: Vec<Member>,
}

impl Set {
    fn contains(&self, unit: u32) -> bool {
        let found = self.members.iter().any(|m| match *m {
            Member::One(c) => c == unit,
            Member::Range(lo, hi) => lo <= unit && unit <= hi,
            Member::Class(class) => class.contains(unit),
        });
        found != self.negate
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Item {
    Char(u32),
    AnyOne,
    AnyMany,
    Set(Set),
}

/// A compiled pattern.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Pattern {
    items: Vec<Item>,
}

impl Pattern {
    /// Compiles `text`. `active[i]` says whether byte `i` may act as a pattern character;
    /// a quoted one may not. `active` must be as long as `text`.
    pub(crate) fn new(text: &[u8], active: &[bool]) -> Self {
        debug_assert_eq!(text.len(), active.len());
        let mut items = Vec::new();
        let mut i = 0;
        while i < text.len() {
            let (unit, len) = unit_at(&text[i..]);
            if !active[i] {
                items.push(Item::Char(unit));
                i += len;
                continue;
            }
            match text[i] {
                b'*' => {
                    if items.last() != Some(&Item::AnyMany) {
                        items.push(Item::AnyMany);
                    }
                    i += 1;
                }
                b'?' => {
                    items.push(Item::AnyOne);
                    i += 1;
                }
                b'[' => {
                    if let Some((set, next)) = parse_set(text, active, i + 1) {
                        items.push(Item::Set(set));
                        i = next;
                    } else {
                        items.push(Item::Char(u32::from(b'[')));
                        i += 1;
                    }
                }
                _ => {
                    items.push(Item::Char(unit));
                    i += len;
                }
            }
        }
        Self { items }
    }

    /// A pattern in which every character is active.
    #[cfg(test)]
    pub(crate) fn from_text(text: &str) -> Self {
        Self::new(text.as_bytes(), &vec![true; text.len()])
    }

    /// True if nothing in the pattern is special, so that it matches only its own text.
    pub(crate) fn is_literal(&self) -> bool {
        self.items.iter().all(|i| matches!(i, Item::Char(_)))
    }

    /// True if the pattern can only match a name that starts with a `.`.
    pub(crate) fn starts_with_dot(&self) -> bool {
        matches!(self.items.first(), Some(Item::Char(c)) if *c == u32::from(b'.'))
    }

    /// True if the whole of `text` matches.
    pub(crate) fn matches(&self, text: &[u8]) -> bool {
        let items = &self.items;
        let (mut pi, mut ti) = (0, 0);
        // Where the last `*` was, and how much of the text it has taken so far.
        let mut star: Option<(usize, usize)> = None;
        while ti < text.len() {
            let (unit, len) = unit_at(&text[ti..]);
            let step = match items.get(pi) {
                Some(Item::AnyMany) => {
                    star = Some((pi, ti));
                    pi += 1;
                    continue;
                }
                Some(Item::AnyOne) => true,
                Some(Item::Char(c)) => *c == unit,
                Some(Item::Set(set)) => set.contains(unit),
                None => false,
            };
            if step {
                pi += 1;
                ti += len;
            } else if let Some((star_pi, star_ti)) = star {
                // Let the star take one more character and try again.
                let taken = unit_at(&text[star_ti..]).1;
                star = Some((star_pi, star_ti + taken));
                pi = star_pi + 1;
                ti = star_ti + taken;
            } else {
                return false;
            }
        }
        items[pi.min(items.len())..]
            .iter()
            .all(|i| matches!(i, Item::AnyMany))
    }

    /// The length of the shortest prefix of `text` that matches.
    pub(crate) fn shortest_prefix(&self, text: &[u8]) -> Option<usize> {
        boundaries(text)
            .into_iter()
            .find(|&end| self.matches(&text[..end]))
    }

    /// The length of the longest prefix of `text` that matches.
    pub(crate) fn longest_prefix(&self, text: &[u8]) -> Option<usize> {
        boundaries(text)
            .into_iter()
            .rev()
            .find(|&end| self.matches(&text[..end]))
    }

    /// The start of the shortest suffix of `text` that matches.
    pub(crate) fn shortest_suffix(&self, text: &[u8]) -> Option<usize> {
        boundaries(text)
            .into_iter()
            .rev()
            .find(|&start| self.matches(&text[start..]))
    }

    /// The start of the longest suffix of `text` that matches.
    pub(crate) fn longest_suffix(&self, text: &[u8]) -> Option<usize> {
        boundaries(text)
            .into_iter()
            .find(|&start| self.matches(&text[start..]))
    }
}

/// Reads a bracket expression whose `[` is just before `start`. Returns the set and the
/// offset after the closing `]`, or nothing if there is no closing `]`, in which case the `[`
/// is an ordinary character.
fn parse_set(text: &[u8], active: &[bool], start: usize) -> Option<(Set, usize)> {
    let n = text.len();
    let mut i = start;
    let mut negate = false;
    // `!` is the POSIX negation. `^` is not, but bash and most users expect it.
    if i < n && active[i] && matches!(text[i], b'!' | b'^') {
        negate = true;
        i += 1;
    }
    let mut members = Vec::new();
    let mut first = true;
    loop {
        if i >= n {
            return None;
        }
        if active[i] && text[i] == b']' && !first {
            return Some((Set { negate, members }, i + 1));
        }
        first = false;
        if active[i]
            && text[i] == b'['
            && i + 1 < n
            && text[i + 1] == b':'
            && let Some(close) = find_class_end(text, i + 2)
            && let Some(class) = Class::from_name(&text[i + 2..close])
        {
            members.push(Member::Class(class));
            i = close + 2;
            continue;
        }
        let (low, len) = unit_at(&text[i..]);
        i += len;
        // A `-` between two characters makes a range, unless the second one is the final `]`.
        if i + 1 < n && active[i] && text[i] == b'-' && !(active[i + 1] && text[i + 1] == b']') {
            let (high, len) = unit_at(&text[i + 1..]);
            i += 1 + len;
            members.push(Member::Range(low, high));
        } else {
            members.push(Member::One(low));
        }
    }
}

/// The offset of the `:` of a `:]` at or after `from`.
fn find_class_end(text: &[u8], from: usize) -> Option<usize> {
    (from..text.len().saturating_sub(1)).find(|&j| text[j] == b':' && text[j + 1] == b']')
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    fn m(pattern: &str, text: &str) -> bool {
        Pattern::from_text(pattern).matches(text.as_bytes())
    }

    #[test]
    fn plain_text_matches_only_itself() {
        assert!(m("abc", "abc"));
        assert!(!m("abc", "abcd"));
        assert!(!m("abc", "ab"));
        assert!(m("", ""));
        assert!(!m("", "a"));
    }

    #[test]
    fn star_matches_any_run_including_none() {
        assert!(m("*", ""));
        assert!(m("*", "anything"));
        assert!(m("a*c", "ac"));
        assert!(m("a*c", "abbbc"));
        assert!(!m("a*c", "abbbd"));
        assert!(m("*.rs", "main.rs"));
        assert!(!m("*.rs", "main.rs.bak"));
        assert!(m("**a**", "bab"));
    }

    #[test]
    fn question_mark_matches_one_character() {
        assert!(m("a?c", "abc"));
        assert!(!m("a?c", "ac"));
        assert!(!m("a?c", "abbc"));
    }

    #[test]
    fn question_mark_matches_a_whole_multibyte_character() {
        assert!(m("?", "ব"));
        assert!(m("???", "বাং"));
        assert!(!m("??", "বাং"));
        assert!(m("ব*", "বাংলা"));
    }

    #[test]
    fn a_byte_that_is_not_utf8_is_one_character() {
        let pattern = Pattern::from_text("a?c");
        assert!(pattern.matches(b"a\xffc"));
        assert!(!pattern.matches(b"a\xe0\xa6c"));
    }

    #[test]
    fn sets_ranges_and_negation() {
        assert!(m("[abc]", "b"));
        assert!(!m("[abc]", "d"));
        assert!(m("[a-c]x", "bx"));
        assert!(!m("[a-c]x", "dx"));
        assert!(m("[!a-c]", "d"));
        assert!(!m("[!a-c]", "a"));
        assert!(m("[^a]", "b"));
        assert!(!m("[^a]", "a"));
        assert!(m("[a-]", "-"));
        assert!(m("[]a]", "]"));
        assert!(m("[!]a]", "b"));
        assert!(!m("[!]a]", "]"));
        assert!(m("[ক-হ]", "ব"));
    }

    #[test]
    fn classes() {
        assert!(m("[[:digit:]]", "7"));
        assert!(!m("[[:digit:]]", "x"));
        assert!(m("[[:alpha:][:digit:]_]", "_"));
        assert!(m("[![:space:]]", "a"));
        assert!(m("[[:upper:]]", "Q"));
        assert!(!m("[[:upper:]]", "q"));
    }

    #[test]
    fn an_unclosed_bracket_is_an_ordinary_character() {
        assert!(m("[abc", "[abc"));
        assert!(m("a[", "a["));
        assert!(!m("[abc", "a"));
        assert!(m("[]", "[]"));
        assert!(m("[[:nosuch:]]", "n]"));
    }

    #[test]
    fn a_quoted_character_is_not_special() {
        let text = b"a*b";
        let pattern = Pattern::new(text, &[true, false, true]);
        assert!(pattern.matches(b"a*b"));
        assert!(!pattern.matches(b"axxb"));
        // A quoted `]` does not close the set.
        let pattern = Pattern::new(b"[a]", &[true, true, false]);
        assert!(pattern.matches(b"[a]"));
        assert!(!pattern.matches(b"a"));
    }

    #[test]
    fn prefix_and_suffix_matches() {
        let p = Pattern::from_text("*b");
        assert_eq!(p.shortest_prefix(b"abcabc"), Some(2));
        assert_eq!(p.longest_prefix(b"abcabc"), Some(5));
        let p = Pattern::from_text("b*");
        assert_eq!(p.shortest_suffix(b"abcabc"), Some(4));
        assert_eq!(p.longest_suffix(b"abcabc"), Some(1));
        assert_eq!(Pattern::from_text("z*").shortest_suffix(b"abc"), None);
        // The empty prefix matches an empty pattern.
        assert_eq!(Pattern::from_text("").shortest_prefix(b"abc"), Some(0));
    }

    #[test]
    fn prefixes_cut_at_character_boundaries() {
        let p = Pattern::from_text("?");
        assert_eq!(p.shortest_prefix("বাংলা".as_bytes()), Some(3));
    }

    #[test]
    fn many_stars_do_not_take_exponential_time() {
        let pattern = Pattern::from_text("*a*a*a*a*a*a*a*a*a*b");
        let text = "a".repeat(5_000);
        let start = Instant::now();
        assert!(!pattern.matches(text.as_bytes()));
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn literal_and_dot_detection() {
        assert!(Pattern::from_text("abc").is_literal());
        assert!(!Pattern::from_text("a*").is_literal());
        assert!(!Pattern::from_text("[a]").is_literal());
        assert!(Pattern::from_text("[a").is_literal());
        assert!(Pattern::from_text(".*").starts_with_dot());
        assert!(!Pattern::from_text("*").starts_with_dot());
        assert!(!Pattern::from_text("[.]x").starts_with_dot());
    }
}
