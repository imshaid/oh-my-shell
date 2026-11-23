//! A note for a path that a plan step opens and that does not exist (v1.1.2).
//!
//! The model writes `code ../night-before-codebase`; the folder is `../night-before`. The
//! analyzer (`omsh_safety::opens`) says which words of which commands are paths that must exist;
//! this module looks at the file system, only for those, and only when a plan is shown. The
//! result is a note in the panel with a "did you mean" taken from the real names in the folder
//! where the path first goes wrong. Nothing is rewritten and nothing is sent anywhere.
//!
//! To keep false alarms down, a path is **not** noted when the name that is missing appears in an
//! earlier step or earlier in the same step (`git clone x/repo && cd repo`, `mkdir b && cd b`),
//! when `CDPATH` may find a `cd` name, when the folder is reached through a link and `..`, or
//! when the file system answers with anything but "no such file". Each missing place is noted
//! once, because the name is already written where it was noted.

use std::collections::HashMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use omsh_editor::near_name;
use omsh_safety::opens::Opened;

/// A path that is not there and the paths that may have been meant, closest first.
pub(super) type Note = (String, Vec<String>);

/// How many notes one step gets.
const MAX_NOTES: usize = 3;
/// How many names are offered for one path.
const MAX_SUGGESTIONS: usize = 3;
/// How many entries of a folder are read. A bigger folder gives a note with no suggestion.
const MAX_ENTRIES: usize = 4096;

/// One step as this module needs it: its command text and the paths its commands open.
pub(super) struct StepPaths<'a> {
    pub command: &'a str,
    pub opens: &'a [Opened],
}

/// The notes of every step, in order. `cdpath` says whether `CDPATH` is set.
pub(super) fn notes(steps: &[StepPaths<'_>], cdpath: bool) -> Vec<Vec<Note>> {
    let mut seen = Seen::default();
    let mut earlier = String::new();
    let mut out = Vec::with_capacity(steps.len());
    for step in steps {
        let mut found: Vec<Note> = Vec::new();
        for open in step.opens {
            if found.len() >= MAX_NOTES {
                break;
            }
            if let Some(note) = note_for(open, step.command, &earlier, cdpath, &mut seen) {
                found.push(note);
            }
        }
        earlier.push_str(step.command);
        earlier.push('\n');
        out.push(found);
    }
    out
}

/// What was already looked at in one plan.
#[derive(Default)]
struct Seen {
    /// The names in a folder, read once. `None` if it could not be read or is too big.
    listed: HashMap<PathBuf, Option<Vec<String>>>,
}

/// Where a path first goes wrong.
#[derive(Debug, PartialEq, Eq)]
struct Missing {
    /// The deepest part that exists.
    parent: PathBuf,
    /// The first part that does not.
    name: String,
    /// The parts after it.
    rest: Vec<String>,
}

/// The first part of `path` (absolute, cleaned) that is not there. `None` if it all exists, or
/// if the file system says anything else than "no such file" (no permission, not a folder, ...).
fn first_missing(path: &str) -> Option<Missing> {
    let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    let mut at = PathBuf::from("/");
    for (index, part) in parts.iter().enumerate() {
        let next = at.join(part);
        match fs::symlink_metadata(&next) {
            Ok(_) => at = next,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                return Some(Missing {
                    parent: at,
                    name: (*part).to_owned(),
                    rest: parts[index + 1..].iter().map(|p| (*p).to_owned()).collect(),
                });
            }
            Err(_) => return None,
        }
    }
    None
}

fn note_for(
    open: &Opened,
    command: &str,
    earlier: &str,
    cdpath: bool,
    seen: &mut Seen,
) -> Option<Note> {
    if cdpath && open.cdpath {
        return None;
    }
    // `..` after a link means something else to the shell and to the file system.
    if open.dotdot
        && let Some(cwd) = &open.cwd
        && fs::canonicalize(cwd).ok().as_deref() != Some(Path::new(cwd))
    {
        return None;
    }
    let missing = first_missing(&open.path)?;
    let before = command.get(..open.span.start).unwrap_or(command);
    if earlier.contains(&missing.name) || before.contains(&missing.name) {
        return None;
    }
    let names = seen
        .listed
        .entry(missing.parent.clone())
        .or_insert_with(|| read_names(&missing.parent));
    let near = names
        .as_deref()
        .map(|names| suggestions(open, &missing, names))
        .unwrap_or_default();
    Some((open.src.clone(), near))
}

/// The names in `folder`, or `None` if it cannot be read or has too many. A name with a control
/// character is left out (it could write escape codes to the terminal).
fn read_names(folder: &Path) -> Option<Vec<String>> {
    let mut names = Vec::new();
    for (count, entry) in fs::read_dir(folder).ok()?.enumerate() {
        if count >= MAX_ENTRIES {
            return None;
        }
        let name = entry.ok()?.file_name().into_string().ok()?;
        if !name.is_empty() && !name.chars().any(char::is_control) {
            names.push(name);
        }
    }
    Some(names)
}

/// The paths written like the word, with the missing part replaced by a name that makes the
/// whole path exist.
fn suggestions(open: &Opened, missing: &Missing, names: &[String]) -> Vec<String> {
    // The corrected path must exist, and be a folder if it is a folder that the command needs.
    let needs_folder = !missing.rest.is_empty() || open.dir_only;
    let keep = |candidate: &str| -> bool {
        let mut path = missing.parent.join(candidate);
        for part in &missing.rest {
            path.push(part);
        }
        if needs_folder && missing.rest.is_empty() {
            return fs::metadata(&path).is_ok_and(|meta| meta.is_dir());
        }
        fs::symlink_metadata(&path).is_ok()
    };
    let parts: Vec<&str> = open.shown.split('/').collect();
    let Some(at) = parts.iter().rposition(|part| *part == missing.name) else {
        return Vec::new();
    };
    ranked(&missing.name, names)
        .into_iter()
        .filter(|candidate| keep(candidate))
        .take(MAX_SUGGESTIONS)
        .map(|candidate| {
            let mut written = parts.clone();
            written[at] = &candidate;
            quoted(&written.join("/"))
        })
        .collect()
}

/// A name without capitals, `-`, `_`, `.` and blanks.
fn plain(name: &str) -> String {
    name.chars()
        .filter(|c| !matches!(c, '-' | '_' | '.' | ' '))
        .flat_map(char::to_lowercase)
        .collect()
}

/// How much longer `long` is than `short` when it is `short` and then more, after a `-`, `_`, `.`
/// or blank (`night-before-codebase` and `night-before`); `short` has at least 3 characters.
fn tail(long: &str, short: &str) -> Option<usize> {
    let (long, short) = (long.to_lowercase(), short.to_lowercase());
    let rest = long.strip_prefix(&short)?;
    (short.chars().count() >= 3 && rest.starts_with(['-', '_', '.', ' '])).then_some(rest.len())
}

/// The names of `names` that `typed` may have been meant as, best first: the same name apart
/// from capitals and separators, then the few-edits names, then a name that is the other with a
/// tail. Within each, the closer first, then by name. Hidden names only for a hidden `typed`.
fn ranked(typed: &str, names: &[String]) -> Vec<String> {
    let wants_hidden = typed.starts_with('.');
    let typed_plain = plain(typed);
    let mut scored: Vec<(u8, usize, &String)> = Vec::new();
    for name in names {
        if name == typed || (name.starts_with('.') && !wants_hidden) {
            continue;
        }
        let tier = if !typed_plain.is_empty() && plain(name) == typed_plain {
            Some((0, 0))
        } else if let Some(distance) = near_name(typed, name) {
            Some((1, distance))
        } else {
            tail(typed, name)
                .or_else(|| tail(name, typed))
                .map(|length| (2, length))
        };
        if let Some((tier, key)) = tier {
            scored.push((tier, key, name));
        }
    }
    scored.sort();
    scored
        .into_iter()
        .map(|(_, _, name)| name.clone())
        .collect()
}

/// `text` as a shell word: unchanged if it is plain, else in single quotes (a leading `~/` stays
/// outside, so that it is still the home folder).
fn quoted(text: &str) -> String {
    let plain = |s: &str| {
        s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_./+:,@%=-".contains(c))
    };
    let single = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
    if plain(text) || text == "~" {
        return text.to_owned();
    }
    match text.strip_prefix("~/") {
        Some(rest) if plain(rest) => text.to_owned(),
        Some(rest) => format!("~/{}", single(rest)),
        None => single(text),
    }
}

#[cfg(test)]
mod tests;
