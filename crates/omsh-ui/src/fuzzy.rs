//! A small fuzzy matcher for the history explorer.
//!
//! It is our own code on purpose (Decision Log, S27a): the crates that were looked at either did
//! not build on the pinned Rust (`nucleo-matcher`) or added 1.3 to 3.9 MB to the binary
//! (`frizbee`). A query is words separated by blanks. Every word must match; a word is
//!
//! | word    | meaning                                                  |
//! |---------|----------------------------------------------------------|
//! | `foo`   | the letters in this order, anywhere ("fuzzy")            |
//! | `'foo`  | the text `foo`, exactly, anywhere                        |
//! | `^foo`  | starts with `foo`                                        |
//! | `foo$`  | ends with `foo`                                          |
//! | `^foo$` | is `foo`                                                 |
//! | `!foo`  | does not match `foo` (any of the forms above after `!`)  |
//!
//! Case is ignored unless the word has a capital letter ("smart case"). The score rewards
//! letters at the start of a word and letters next to each other, and charges for gaps. The
//! alignment is found in one forward and one backward pass over the text (the way fzf's first
//! algorithm does), so it costs a few steps for each character and no matrix.

/// The text to search, prepared once so that a keystroke does not lower-case it again.
///
/// A list of 10,000 commands is held while the history explorer is open, so the memory counts:
/// the text is kept in one form when lower-casing changes nothing (most commands), and in two
/// when it does.
#[derive(Clone, Debug)]
pub struct Haystack {
    /// The characters as they are, only when they differ from `folded`.
    original: Option<Vec<char>>,
    folded: Vec<char>,
}

impl Haystack {
    #[must_use]
    pub fn new(text: &str) -> Self {
        let chars: Vec<char> = text.chars().collect();
        let folded: Vec<char> = chars.iter().map(|c| fold(*c)).collect();
        let original = (chars != folded).then_some(chars);
        Self { original, folded }
    }

    /// The characters as they were given.
    fn chars(&self) -> &[char] {
        self.original.as_deref().unwrap_or(&self.folded)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.folded.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.folded.is_empty()
    }
}

/// One character in lower case (the first character of its lower-case form).
fn fold(c: char) -> char {
    if c.is_ascii() {
        c.to_ascii_lowercase()
    } else {
        c.to_lowercase().next().unwrap_or(c)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Fuzzy,
    Substring,
    Prefix,
    Suffix,
    Whole,
}

#[derive(Clone, Debug)]
struct Atom {
    kind: Kind,
    negated: bool,
    /// The word as typed, without the markers.
    needle: Vec<char>,
    /// `needle`, lower-cased (equal to it when the word is case sensitive).
    folded: Vec<char>,
    sensitive: bool,
}

/// A parsed query.
#[derive(Clone, Debug)]
pub struct Query {
    text: String,
    atoms: Vec<Atom>,
}

const MATCH: i32 = 16;
const START_OF_TEXT: i32 = 10;
const START_OF_WORD: i32 = 8;
const NEXT_TO_PREVIOUS: i32 = 6;
const SAME_CASE: i32 = 1;
const GAP_OPEN: i32 = 3;
const GAP_EXTEND: i32 = 1;
const GAP_LIMIT: i32 = 12;

impl Query {
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let atoms = text.split_whitespace().filter_map(parse_atom).collect();
        Self {
            text: text.to_owned(),
            atoms,
        }
    }

    /// The text this query was parsed from.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// True when nothing is asked: every text matches.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.atoms.is_empty()
    }

    /// The score of `haystack`, or `None` when it does not match. Higher is better. An empty
    /// query gives every text the score 0.
    #[must_use]
    pub fn score(&self, haystack: &Haystack) -> Option<i32> {
        let mut total = 0;
        for atom in &self.atoms {
            let found = atom.find(haystack);
            if atom.negated {
                if found.is_some() {
                    return None;
                }
            } else {
                total += found?.score;
            }
        }
        Some(total)
    }

    /// The places (character indexes, ascending) of the letters that matched, for drawing them
    /// in another color. Empty for a text that does not match.
    #[must_use]
    pub fn positions(&self, haystack: &Haystack) -> Vec<usize> {
        let mut places = Vec::new();
        for atom in self.atoms.iter().filter(|atom| !atom.negated) {
            match atom.find(haystack) {
                Some(found) => places.extend(found.places),
                None => return Vec::new(),
            }
        }
        places.sort_unstable();
        places.dedup();
        places
    }

    /// True when every text that matches `self` also matches `earlier`, so that the matches of
    /// `earlier` are all that has to be searched. This holds when `self` is `earlier` with more
    /// typed after it, unless `earlier` has a word that a longer text can widen: a `!` word
    /// (`!do` excludes more than `!doc`) or an end marker (`ab$` is not a prefix of `ab$c`).
    #[must_use]
    pub fn narrows(&self, earlier: &Query) -> bool {
        self.text.starts_with(&earlier.text)
            && earlier
                .atoms
                .iter()
                .all(|atom| !atom.negated && !matches!(atom.kind, Kind::Suffix | Kind::Whole))
    }
}

fn parse_atom(word: &str) -> Option<Atom> {
    let (negated, word) = match word.strip_prefix('!') {
        Some(rest) => (true, rest),
        None => (false, word),
    };
    let (mut kind, mut word) = (Kind::Fuzzy, word);
    if let Some(rest) = word.strip_prefix('\'') {
        kind = Kind::Substring;
        word = rest;
    } else {
        let prefix = word.strip_prefix('^');
        if let Some(rest) = prefix {
            kind = Kind::Prefix;
            word = rest;
        }
        if let Some(rest) = word.strip_suffix('$') {
            kind = if prefix.is_some() {
                Kind::Whole
            } else {
                Kind::Suffix
            };
            word = rest;
        }
    }
    if word.is_empty() {
        return None;
    }
    let sensitive = word.chars().any(char::is_uppercase);
    let needle: Vec<char> = word.chars().collect();
    let folded = if sensitive {
        needle.clone()
    } else {
        needle.iter().map(|c| fold(*c)).collect()
    };
    Some(Atom {
        kind,
        negated,
        needle,
        folded,
        sensitive,
    })
}

struct Found {
    score: i32,
    places: Vec<usize>,
}

impl Atom {
    fn find(&self, haystack: &Haystack) -> Option<Found> {
        let text = if self.sensitive {
            haystack.chars()
        } else {
            &haystack.folded[..]
        };
        let wanted = &self.folded;
        let length = wanted.len();
        match self.kind {
            Kind::Fuzzy => self.fuzzy(haystack, text),
            Kind::Substring => {
                let start = find_run(text, wanted)?;
                Some(self.run(haystack, start))
            }
            Kind::Prefix => text.starts_with(wanted).then(|| self.run(haystack, 0)),
            Kind::Suffix => text
                .ends_with(wanted)
                .then(|| self.run(haystack, text.len() - length)),
            Kind::Whole => (text == wanted).then(|| {
                let mut found = self.run(haystack, 0);
                found.score += START_OF_TEXT;
                found
            }),
        }
    }

    /// A run of `needle.len()` letters that start at `start`.
    fn run(&self, haystack: &Haystack, start: usize) -> Found {
        let length = self.needle.len();
        let mut score = MATCH * to_i32(length) + NEXT_TO_PREVIOUS * to_i32(length - 1);
        score += start_bonus(haystack.chars(), start);
        Found {
            score,
            places: (start..start + length).collect(),
        }
    }

    fn fuzzy(&self, haystack: &Haystack, text: &[char]) -> Option<Found> {
        let wanted = &self.folded;
        // Forward: the earliest place where all the letters have been seen, in order.
        let mut next = 0;
        let mut end = None;
        for (index, letter) in text.iter().enumerate() {
            if *letter == wanted[next] {
                next += 1;
                if next == wanted.len() {
                    end = Some(index);
                    break;
                }
            }
        }
        let end = end?;
        // Backward from there: the latest start, which makes the window as short as it can be.
        let mut places = vec![0; wanted.len()];
        let mut want = wanted.len();
        let mut index = end + 1;
        while want > 0 {
            index -= 1;
            if text[index] == wanted[want - 1] {
                want -= 1;
                places[want] = index;
            }
        }
        Some(Found {
            score: self.fuzzy_score(haystack, &places),
            places,
        })
    }

    fn fuzzy_score(&self, haystack: &Haystack, places: &[usize]) -> i32 {
        let mut score = 0;
        let mut previous: Option<usize> = None;
        for (letter, &place) in places.iter().enumerate() {
            score += MATCH;
            if haystack.chars()[place] == self.needle[letter] {
                score += SAME_CASE;
            }
            match previous {
                Some(before) if place == before + 1 => score += NEXT_TO_PREVIOUS,
                Some(before) => {
                    let gap = to_i32(place - before - 1);
                    score -= (GAP_OPEN + GAP_EXTEND * (gap - 1)).min(GAP_LIMIT);
                    score += start_bonus(haystack.chars(), place).min(START_OF_WORD);
                }
                None => score += start_bonus(haystack.chars(), place),
            }
            previous = Some(place);
        }
        score
    }
}

/// The bonus for a match that starts at `place`: the start of the text, or of a word.
fn start_bonus(chars: &[char], place: usize) -> i32 {
    match place.checked_sub(1).map(|before| chars[before]) {
        None => START_OF_TEXT,
        Some(before) if is_boundary(before) => START_OF_WORD,
        Some(_) => 0,
    }
}

fn is_boundary(c: char) -> bool {
    c.is_whitespace() || matches!(c, '/' | '-' | '_' | '.' | ':' | '=' | ',' | ';' | '|' | '&')
}

fn find_run(text: &[char], wanted: &[char]) -> Option<usize> {
    if wanted.len() > text.len() {
        return None;
    }
    (0..=text.len() - wanted.len()).find(|&start| text[start..start + wanted.len()] == *wanted)
}

fn to_i32(n: usize) -> i32 {
    i32::try_from(n).unwrap_or(i32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn score(query: &str, text: &str) -> Option<i32> {
        Query::parse(query).score(&Haystack::new(text))
    }

    fn places(query: &str, text: &str) -> Vec<usize> {
        Query::parse(query).positions(&Haystack::new(text))
    }

    #[test]
    fn the_text_is_kept_once_when_lower_casing_changes_nothing() {
        // Most commands are lower case: one copy. Capitals or other letters that fold need the
        // original too, and the case rules still see it.
        assert!(Haystack::new("docker ps -a /tmp/ß").original.is_none());
        assert!(Haystack::new("Docker").original.is_some());
        assert_eq!(Haystack::new("Docker").chars()[0], 'D');
        assert_eq!(Haystack::new("docker").chars()[0], 'd');
        assert_eq!(Haystack::new("Ärger").len(), 5);
    }

    #[test]
    fn the_letters_must_come_in_order() {
        assert!(score("dkps", "docker ps -a").is_some());
        assert!(score("psdk", "docker ps -a").is_none());
        assert!(score("zzz", "docker ps -a").is_none());
    }

    #[test]
    fn an_empty_query_matches_everything_with_score_zero() {
        assert_eq!(score("", "anything"), Some(0));
        assert_eq!(score("   ", ""), Some(0));
        assert!(Query::parse("  ").is_empty());
    }

    #[test]
    fn case_is_ignored_until_the_word_has_a_capital() {
        assert!(score("docker", "Docker ps").is_some());
        assert!(score("Docker", "docker ps").is_none());
        assert!(score("Docker", "Docker ps").is_some());
        // The same case scores a little higher.
        assert!(score("d", "docker") > score("d", "Docker"));
    }

    #[test]
    fn a_tighter_match_beats_a_scattered_one() {
        let tight = score("git", "git status").unwrap();
        let scattered = score("git", "grep -i thing").unwrap();
        assert!(tight > scattered, "{tight} vs {scattered}");
    }

    #[test]
    fn the_start_of_a_word_beats_the_middle_of_one() {
        let word = score("ps", "docker ps").unwrap();
        let middle = score("ps", "scripts").unwrap();
        assert!(word > middle, "{word} vs {middle}");
    }

    #[test]
    fn positions_are_the_letters_of_the_tightest_window() {
        // The letters are found at the end, not spread over the first occurrences.
        assert_eq!(places("dkps", "docker ps -a"), [0, 3, 7, 8]);
        assert_eq!(places("ab", "a x ab"), [4, 5]);
        assert_eq!(places("zzz", "docker"), Vec::<usize>::new());
    }

    #[test]
    fn quote_marker_means_the_exact_text() {
        assert!(score("'ps", "docker ps").is_some());
        assert!(score("'dkps", "docker ps").is_none());
        assert_eq!(places("'ps", "docker ps"), [7, 8]);
    }

    #[test]
    fn prefix_suffix_and_whole_markers() {
        assert!(score("^dock", "docker ps").is_some());
        assert!(score("^ps", "docker ps").is_none());
        assert!(score("ps$", "docker ps").is_some());
        assert!(score("dock$", "docker ps").is_none());
        assert!(score("^ls$", "ls").is_some());
        assert!(score("^ls$", "ls -la").is_none());
        assert_eq!(places("ps$", "docker ps"), [7, 8]);
    }

    #[test]
    fn a_negated_word_excludes() {
        assert!(score("!docker", "git push").is_some());
        assert!(score("!docker", "docker ps").is_none());
        assert!(score("git !push", "git status").is_some());
        assert!(score("git !push", "git push").is_none());
        // A negated word adds nothing to the score and shows no letters.
        assert_eq!(score("!docker", "git push"), Some(0));
        assert!(places("!docker", "git push").is_empty());
    }

    #[test]
    fn every_word_must_match() {
        assert!(score("doc ps", "docker ps -a").is_some());
        assert!(score("doc zz", "docker ps -a").is_none());
        assert_eq!(places("^do ps", "docker ps"), [0, 1, 7, 8]);
    }

    #[test]
    fn a_lone_marker_asks_for_nothing() {
        assert!(score("^", "x").is_some());
        assert!(score("'", "x").is_some());
        assert!(score("!", "x").is_some());
        assert!(score("$", "x").is_some());
    }

    #[test]
    fn letters_that_are_not_ascii_work() {
        assert!(score("ärger", "Ärger machen").is_some());
        assert!(score("Ärger", "ärger machen").is_none());
        assert_eq!(places("rg", "ärger"), [1, 2]);
    }

    #[test]
    fn narrowing_is_allowed_only_when_the_longer_query_can_only_lose_matches() {
        let q = Query::parse;
        assert!(q("ab").narrows(&q("a")));
        assert!(q("ab c").narrows(&q("ab")));
        assert!(q("^ab").narrows(&q("^a")));
        assert!(q("anything").narrows(&q("")));
        // Not an extension.
        assert!(!q("b").narrows(&q("a")));
        // A negated word widens when it grows; an end marker is not a prefix.
        assert!(!q("!ab").narrows(&q("!a")));
        assert!(!q("ab$c").narrows(&q("ab$")));
        assert!(!q("^ab$x").narrows(&q("^ab$")));
    }

    /// For every pair of queries over a small alphabet where `narrows` says yes, no text that
    /// matches the longer one may miss the shorter one.
    #[test]
    fn narrows_never_loses_a_match() {
        let alphabet = ['a', 'b', 'A', ' ', '!', '\'', '^', '$'];
        let mut queries = vec![String::new()];
        for _ in 0..4 {
            let mut longer = Vec::new();
            for query in &queries {
                for c in alphabet {
                    longer.push(format!("{query}{c}"));
                }
            }
            queries.extend(longer);
            queries.sort();
            queries.dedup();
        }
        let texts: Vec<Haystack> = [
            "", "a", "b", "ab", "ba", "aab", "AB", "a b", "ab ab", "bab", "a!b", "'ab", "ab$",
            "^ab", "Aab", "b a",
        ]
        .iter()
        .map(|text| Haystack::new(text))
        .collect();
        let mut checked = 0;
        for shorter in &queries {
            let earlier = Query::parse(shorter);
            for longer in queries.iter().filter(|q| q.starts_with(shorter.as_str())) {
                let later = Query::parse(longer);
                if !later.narrows(&earlier) {
                    continue;
                }
                checked += 1;
                for text in &texts {
                    if later.score(text).is_some() {
                        assert!(
                            earlier.score(text).is_some(),
                            "{longer:?} matches {text:?} but {shorter:?} does not"
                        );
                    }
                }
            }
        }
        assert!(checked > 1000, "only {checked} pairs were checked");
    }
}
