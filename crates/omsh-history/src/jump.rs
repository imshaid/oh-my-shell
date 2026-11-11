//! Ordering of the visited folders for `j` (blueprint §10, smart jump).
//!
//! A pure function: the store gives the folders, the clock is a parameter, and the answer is
//! the matching paths, best first. Nothing here touches the file system; the builtin checks
//! that a folder still exists before it changes into it.

use crate::store::Dir;

const HOUR_MS: i64 = 3_600_000;
const DAY_MS: i64 = 24 * HOUR_MS;
const WEEK_MS: i64 = 7 * DAY_MS;

/// How much a folder's visits count, by how long ago the last one was. Four times the weights
/// of zoxide (4, 2, 0.5, 0.25 become 16, 8, 2, 1) so that the arithmetic stays in integers.
fn weight(age_ms: i64) -> i64 {
    if age_ms < HOUR_MS {
        16
    } else if age_ms < DAY_MS {
        8
    } else if age_ms < WEEK_MS {
        2
    } else {
        1
    }
}

/// Whether `path` matches the words: every word is in the path, in order, capitals ignored,
/// and the last word is in the last component (the folder's own name). `words` must not be
/// empty.
fn matches(path: &str, words: &[String]) -> bool {
    let Some((last, before)) = words.split_last() else {
        return false;
    };
    let lower = path.to_lowercase();
    let mut at = 0;
    for word in before {
        let word = word.to_lowercase();
        match lower[at..].find(&word) {
            Some(found) => at += found + word.len(),
            None => return false,
        }
    }
    // The name of the folder starts after the last `/`.
    let name = lower.rfind('/').map_or(0, |slash| slash + 1);
    let from = at.max(name);
    lower
        .get(from..)
        .is_some_and(|rest| rest.contains(&last.to_lowercase()))
}

/// The paths that match `words`, the most promising first: visits times the weight of the age
/// of the last visit, then the newest visit, then the path. No words, no match.
#[must_use]
pub fn rank(dirs: &[Dir], words: &[String], now_ms: i64) -> Vec<String> {
    let mut found: Vec<(i64, &Dir)> = dirs
        .iter()
        .filter(|dir| matches(&dir.path, words))
        .map(|dir| {
            let age = now_ms.saturating_sub(dir.last_ms).max(0);
            (dir.visits.saturating_mul(weight(age)), dir)
        })
        .collect();
    found.sort_by(|(a, x), (b, y)| {
        b.cmp(a)
            .then(y.last_ms.cmp(&x.last_ms))
            .then(x.path.cmp(&y.path))
    });
    found.into_iter().map(|(_, dir)| dir.path.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_000 * DAY_MS;

    fn dir(path: &str, visits: i64, age_ms: i64) -> Dir {
        Dir {
            path: path.to_owned(),
            visits,
            last_ms: NOW - age_ms,
        }
    }

    fn words(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_owned).collect()
    }

    fn ranked(dirs: &[Dir], query: &str) -> Vec<String> {
        rank(dirs, &words(query), NOW)
    }

    #[test]
    fn no_words_match_nothing() {
        assert!(ranked(&[dir("/a", 1, 0)], "").is_empty());
    }

    #[test]
    fn every_word_must_be_in_the_path_in_order() {
        let dirs = [
            dir("/home/me/work/api", 1, 0),
            dir("/home/me/api/work", 1, 0),
        ];
        assert_eq!(ranked(&dirs, "work api"), ["/home/me/work/api"]);
        assert_eq!(ranked(&dirs, "api work"), ["/home/me/api/work"]);
        assert!(ranked(&dirs, "nothing").is_empty());
    }

    #[test]
    fn the_last_word_must_be_in_the_folders_own_name() {
        let dirs = [dir("/home/me/proj/src", 1, 0), dir("/home/me/other", 1, 0)];
        // "proj" is a parent, not the name of the folder.
        assert!(ranked(&dirs, "proj").is_empty());
        assert_eq!(ranked(&dirs, "proj src"), ["/home/me/proj/src"]);
        assert_eq!(ranked(&dirs, "oth"), ["/home/me/other"]);
    }

    #[test]
    fn capitals_are_ignored() {
        let dirs = [dir("/Home/Me/Alpha-Proj", 1, 0)];
        assert_eq!(ranked(&dirs, "alpha"), ["/Home/Me/Alpha-Proj"]);
        assert_eq!(ranked(&dirs, "ME ALPHA"), ["/Home/Me/Alpha-Proj"]);
    }

    #[test]
    fn visits_and_recent_use_decide_the_order() {
        let dirs = [
            dir("/a/proj-old", 10, 30 * DAY_MS),
            dir("/a/proj-busy", 5, 2 * HOUR_MS),
            dir("/a/proj-now", 1, 0),
        ];
        // 10 * 1, 5 * 8, 1 * 16
        assert_eq!(
            ranked(&dirs, "proj"),
            ["/a/proj-busy", "/a/proj-now", "/a/proj-old"]
        );
    }

    #[test]
    fn the_age_weights_change_at_an_hour_a_day_and_a_week() {
        assert_eq!(weight(HOUR_MS - 1), 16);
        assert_eq!(weight(HOUR_MS), 8);
        assert_eq!(weight(DAY_MS - 1), 8);
        assert_eq!(weight(DAY_MS), 2);
        assert_eq!(weight(WEEK_MS - 1), 2);
        assert_eq!(weight(WEEK_MS), 1);
    }

    #[test]
    fn a_tie_goes_to_the_newer_visit_then_the_path() {
        let dirs = [
            dir("/b/x", 4, 30 * DAY_MS),
            dir("/a/x", 4, 30 * DAY_MS),
            dir("/c/x", 4, 31 * DAY_MS),
        ];
        assert_eq!(ranked(&dirs, "x"), ["/a/x", "/b/x", "/c/x"]);
    }
}
