//! "Did you mean ...?": the names close to a command that was not found (blueprint §7.1).
//!
//! The candidates are the shell's builtins, functions and aliases and the programs in `PATH`
//! (the same index that Tab completion uses). Closeness is the *optimal string alignment*
//! distance: the number of single-character insertions, deletions, replacements and swaps of two
//! neighbours that turn one name into the other, so `gti` is one step from `git`. Names are
//! compared in lower case, and a name that differs only in capitals counts as one step.
//!
//! Short names are matched strictly, because nearly every two-letter name is one edit from some
//! other two-letter program: one edit for 2 to 4 characters, two for longer names, nothing for a
//! single character. Only the closest names are kept, at most three; among names equally close
//! the shell's own (builtins, functions, aliases) come before programs.

use std::collections::HashSet;

use crate::complete::PathIndex;

/// How many names are offered at most.
const MAX_SUGGESTIONS: usize = 3;

/// How many edits away a name for `name` may be, or `None` if it is too short to guess at.
fn allowed_edits(name: &str) -> Option<usize> {
    match name.chars().count() {
        0 | 1 => None,
        2..=4 => Some(1),
        _ => Some(2),
    }
}

/// The optimal string alignment distance of `a` and `b`, or `None` if it is more than `limit`.
fn distance(a: &[char], b: &[char], limit: usize) -> Option<usize> {
    if a.len().abs_diff(b.len()) > limit {
        return None;
    }
    // Rows of the table: two back, one back and the current one.
    let mut before: Vec<usize> = Vec::new();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut current: Vec<usize> = vec![0; b.len() + 1];
    for (i, &x) in a.iter().enumerate() {
        current[0] = i + 1;
        for (j, &y) in b.iter().enumerate() {
            let replace = previous[j] + usize::from(x != y);
            let mut best = replace.min(previous[j + 1] + 1).min(current[j] + 1);
            if i > 0 && j > 0 && x == b[j - 1] && a[i - 1] == y {
                best = best.min(before[j - 1] + 1);
            }
            current[j + 1] = best;
        }
        before = std::mem::replace(&mut previous, current.clone());
    }
    let found = previous[b.len()];
    (found <= limit).then_some(found)
}

/// The distance between two names as people see it: capitals do not count, but a name that
/// differs only in capitals is not the same name.
fn name_distance(
    typed: &[char],
    typed_lower: &[char],
    candidate: &str,
    limit: usize,
) -> Option<usize> {
    let lower: Vec<char> = candidate.chars().flat_map(char::to_lowercase).collect();
    let found = distance(typed_lower, &lower, limit)?;
    if found == 0 {
        let exact = candidate.chars().eq(typed.iter().copied());
        return (!exact).then_some(1);
    }
    Some(found)
}

/// The names `name` may have been meant as, closest first, at most three. `path` is the `PATH`
/// the lookup used; `shell_names` are the builtins, functions and aliases.
#[must_use]
pub fn suggest_command(name: &str, path: &[u8], shell_names: &[String]) -> Vec<String> {
    let Some(limit) = allowed_edits(name) else {
        return Vec::new();
    };
    // A name with a control character or a blank is not a typo of a program.
    if name.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Vec::new();
    }
    let typed: Vec<char> = name.chars().collect();
    let typed_lower: Vec<char> = name.chars().flat_map(char::to_lowercase).collect();

    // (distance, name, comes from the shell)
    let mut near: Vec<(usize, &str, bool)> = shell_names
        .iter()
        .filter_map(|candidate| {
            let found = name_distance(&typed, &typed_lower, candidate, limit)?;
            Some((found, candidate.as_str(), true))
        })
        .collect();
    let index = PathIndex::build(path, &|| false);
    if let Some(index) = &index {
        near.extend(index.names().filter_map(|candidate| {
            let found = name_distance(&typed, &typed_lower, candidate, limit)?;
            Some((found, candidate, false))
        }));
    }
    // Closest first; among equals the shell's own names (what the person defined or uses all
    // day), then the programs, each group in alphabetical order.
    near.sort_unstable_by(|a, b| (a.0, !a.2, a.1).cmp(&(b.0, !b.2, b.1)));
    let mut seen: HashSet<&str> = HashSet::new();
    near.retain(|(_, candidate, _)| seen.insert(candidate));

    let mut out: Vec<String> = Vec::new();
    let mut best: Option<usize> = None;
    for (found, candidate, from_shell) in near {
        if best.is_some_and(|best| found > best) || out.len() >= MAX_SUGGESTIONS {
            break;
        }
        // A file name in `PATH` counts only if it would run; this is looked at for the closest
        // names only, so it costs a few `stat` calls.
        let runs = from_shell || index.as_ref().is_some_and(|index| index.runs(candidate));
        if !runs {
            continue;
        }
        best = Some(found);
        out.push(candidate.to_owned());
    }
    out
}

#[cfg(test)]
mod tests;
