//! Desktop entries (`*.desktop`) as a hint for a plan step whose command is not installed under
//! that name (v1.1.1, step 3).
//!
//! A model writes `google-chrome`; the program on the machine is `google-chrome-stable`, and its
//! desktop entry says so. When a step's command is not found, the entries are read (only then),
//! and if exactly one program fits the name, the step is rewritten to use it. Nothing is
//! started and nothing goes to the network: the files are only read. The rules follow the
//! freedesktop.org Desktop Entry Specification (version 1.5):
//!
//! * Where: `$XDG_DATA_HOME` (default `~/.local/share`), then each of `$XDG_DATA_DIRS` (default
//!   `/usr/local/share:/usr/share`), under `applications/`. A file in a subfolder has `-` for the
//!   `/` in its desktop file ID, and the first file with an ID wins (a `Hidden` one included).
//! * What: only the `[Desktop Entry]` group, never a `[Desktop Action ...]` group (those are
//!   things like "Quit"). `Type=Application`; `Hidden=true` and a `TryExec` that is not there
//!   leave the entry out. String escapes (`\s \n \t \r \\`) come off first, then the quotes of
//!   `Exec`, then the field codes: `%f %F %u %U` (and the deprecated ones) are removed, `%%` is a
//!   `%`, and a line with any other code is invalid, so its entry is left out.
//!
//! * Which entry: the name to fix is compared (lower case, `-desktop` and `-stable` endings
//!   ignored) with the entry's ID, its `Name=` and the name of its program. An entry that says the
//!   name by ID or `Name=` ranks above one that only runs a program of that name (a web-app
//!   shortcut that runs the browser); ambiguity is decided inside the best rank only.
//!
//! Our own rules for v1: only the program is taken (no arguments); an entry that runs a wrapper
//! (`flatpak`, `env`, `sh`, `bash`, `dash`, `zsh`) or uses `@@` markers (flatpak) is left out; and
//! the program must be a bare name or an absolute path of plain characters, so that it can go into
//! the line without quoting.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use omsh_exec::Shell;

/// How deep a folder of entries is looked into, how many files are read in all, and how large a
/// file may be. Real folders hold a few hundred small files; a limit keeps a strange one cheap.
const MAX_DEPTH: usize = 4;
const MAX_FILES: usize = 4096;
const MAX_FILE_BYTES: u64 = 64 * 1024;

/// Programs that start another program. An entry that runs one of them is not about the program
/// that we want, so it is never used for a fix.
const WRAPPERS: [&str; 6] = ["flatpak", "env", "sh", "bash", "dash", "zsh"];

/// One usable entry: what is needed to tell whether a name means it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Entry {
    /// The desktop file ID, for example `org.telegram.desktop.desktop`.
    id: String,
    /// `Name=`, for example `Google Chrome`.
    name: String,
    /// The program of `Exec=` (the first word only), as the entry writes it.
    program: String,
    /// `TryExec=`, when the entry has one.
    try_exec: Option<String>,
}

/// A command word to replace: `from` becomes `to`. `app` is the name of the application the entry
/// is for, when the entry itself says the name (not when it was found only by its program).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Fix {
    pub from: String,
    pub to: String,
    pub app: Option<String>,
}

// -- where the entries are --------------------------------------------------------------------

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|text| !text.is_empty())
}

/// The `applications` folders to read, first match wins: the user's own, then the shared ones.
/// An empty value counts as not set, and a relative folder is ignored (the specification says
/// absolute paths).
pub(super) fn applications_dirs(
    home: Option<&Path>,
    data_home: Option<&str>,
    data_dirs: Option<&str>,
) -> Vec<PathBuf> {
    let own = non_empty(data_home)
        .map(PathBuf::from)
        .or_else(|| home.map(|home| home.join(".local/share")));
    let shared = non_empty(data_dirs)
        .unwrap_or("/usr/local/share:/usr/share")
        .split(':')
        .map(PathBuf::from);
    let mut out: Vec<PathBuf> = Vec::new();
    for base in own.into_iter().chain(shared) {
        let dir = base.join("applications");
        if base.is_absolute() && !out.contains(&dir) {
            out.push(dir);
        }
    }
    out
}

/// [`applications_dirs`] for the variables of `shell` (not the process environment, which an
/// `export` in the shell does not change).
pub(super) fn dirs_of(shell: &Shell) -> Vec<PathBuf> {
    let text = |name: &str| {
        shell
            .var(name)
            .and_then(|value| std::str::from_utf8(value).ok())
    };
    let home = text("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    applications_dirs(
        home.as_deref(),
        text("XDG_DATA_HOME"),
        text("XDG_DATA_DIRS"),
    )
}

/// Reads the usable entries of `dirs`, in precedence order. A folder that is missing or cannot
/// be read gives nothing, without a word: a server, a container or WSL has none.
pub(super) fn scan(dirs: &[PathBuf]) -> Vec<Entry> {
    let mut seen = HashSet::new();
    let mut files = 0;
    let mut out = Vec::new();
    for dir in dirs {
        walk(dir, "", 0, &mut files, &mut seen, &mut out);
    }
    out
}

fn walk(
    dir: &Path,
    prefix: &str,
    depth: usize,
    files: &mut usize,
    seen: &mut HashSet<String>,
    out: &mut Vec<Entry>,
) {
    let Ok(read) = fs::read_dir(dir) else {
        return;
    };
    let mut names: Vec<(String, PathBuf)> = read
        .filter_map(Result::ok)
        .filter_map(|entry| Some((entry.file_name().into_string().ok()?, entry.path())))
        .collect();
    names.sort();
    for (name, path) in names {
        // A link to a folder is not followed: no loops, and the files are where they are.
        let Ok(kind) = fs::symlink_metadata(&path).map(|meta| meta.file_type()) else {
            continue;
        };
        if kind.is_dir() {
            if depth < MAX_DEPTH {
                walk(
                    &path,
                    &format!("{prefix}{name}-"),
                    depth + 1,
                    files,
                    seen,
                    out,
                );
            }
            continue;
        }
        if !name.ends_with(".desktop") {
            continue;
        }
        // A link to a file is followed (distributions use them).
        match fs::metadata(&path) {
            Ok(meta) if meta.is_file() && meta.len() <= MAX_FILE_BYTES => {}
            _ => continue,
        }
        if *files >= MAX_FILES {
            return;
        }
        *files += 1;
        let id = format!("{prefix}{name}");
        // The first file with an ID decides, even when it is one that is left out.
        if !seen.insert(id.clone()) {
            continue;
        }
        if let Some(entry) = fs::read_to_string(&path)
            .ok()
            .and_then(|text| parse_entry(&id, &text))
        {
            out.push(entry);
        }
    }
}

// -- reading one file -------------------------------------------------------------------------

/// The `\s \n \t \r \\` escapes of a string value. Any other backslash stays as it is.
fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// The entry in `text`, or `None` if it is not one to use (see the module notes).
pub(super) fn parse_entry(id: &str, text: &str) -> Option<Entry> {
    let (mut kind, mut name, mut exec, mut try_exec, mut hidden) = (None, None, None, None, None);
    let mut inside = false;
    let mut had_group = false;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            // Another group after the entry's own ends it; its keys (the actions) are never read.
            if had_group {
                break;
            }
            inside = line == "[Desktop Entry]";
            had_group = inside;
            continue;
        }
        if !inside {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let slot = match key.trim() {
            "Type" => &mut kind,
            "Name" => &mut name,
            "Exec" => &mut exec,
            "TryExec" => &mut try_exec,
            "Hidden" => &mut hidden,
            _ => continue,
        };
        // A key may not repeat; if it does, the first one counts.
        slot.get_or_insert_with(|| value.trim().to_owned());
    }
    if kind.as_deref() != Some("Application") || hidden.as_deref() == Some("true") {
        return None;
    }
    let name = unescape(&name?);
    let exec = unescape(&exec?);
    if name.trim().is_empty() || exec.contains("@@") {
        return None;
    }
    let program = parse_exec(&exec)?.into_iter().next()?;
    if !is_usable_program(&program) {
        return None;
    }
    Some(Entry {
        id: id.to_owned(),
        name,
        program,
        try_exec: try_exec
            .map(|value| unescape(&value))
            .filter(|value| !value.is_empty()),
    })
}

/// The words of an `Exec` value (string escapes already removed): quotes undone, `%%` made `%`,
/// and the field codes that stand for files and addresses removed. `None` if the line is invalid:
/// an unclosed quote, a backslash in a quoted word that is not one of the four the specification
/// allows, a `%` code that is not in the specification, or a code in a quoted word (undefined).
fn parse_exec(value: &str) -> Option<Vec<String>> {
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut open = false; // a word has started (a quoted empty word counts)
    let mut code = false; // the word has a field code in it
    let mut chars = value.chars();
    let mut finish = |word: &mut String, open: &mut bool, code: &mut bool| {
        // A word that was only a field code leaves no word behind; at the front it would make a
        // later word the program, so that line is not accepted.
        let was_code_only = *open && *code && word.is_empty();
        if *open && !was_code_only {
            words.push(std::mem::take(word));
        }
        let ok = !(was_code_only && words.is_empty());
        (*open, *code) = (false, false);
        word.clear();
        ok
    };
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' => {
                if !finish(&mut word, &mut open, &mut code) {
                    return None;
                }
            }
            '"' => {
                open = true;
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => match chars.next()? {
                            quoted @ ('"' | '`' | '$' | '\\') => word.push(quoted),
                            _ => return None,
                        },
                        '%' => match chars.next()? {
                            '%' => word.push('%'),
                            _ => return None,
                        },
                        other => word.push(other),
                    }
                }
            }
            '%' => {
                open = true;
                match chars.next()? {
                    '%' => word.push('%'),
                    'f' | 'F' | 'u' | 'U' | 'd' | 'D' | 'n' | 'N' | 'v' | 'm' | 'i' | 'c' | 'k' => {
                        code = true;
                    }
                    _ => return None,
                }
            }
            other => {
                open = true;
                word.push(other);
            }
        }
    }
    if !finish(&mut word, &mut open, &mut code) {
        return None;
    }
    Some(words)
}

/// Whether `program` is one we can put into a line as it is, and that is about the program
/// itself: a bare name or an absolute path, plain characters, not a wrapper.
fn is_usable_program(program: &str) -> bool {
    let plain = |c: char| c.is_ascii_alphanumeric() || "._+-/@,:".contains(c);
    let base = program.rsplit('/').next().unwrap_or("");
    let shaped = if program.contains('/') {
        program.starts_with('/')
    } else {
        program.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
    };
    shaped && !base.is_empty() && program.chars().all(plain) && !WRAPPERS.contains(&base)
}

// -- matching ---------------------------------------------------------------------------------

/// A name as it is compared: lower case, spaces as `-`, and the endings `-desktop` and
/// `-stable` taken off (again and again, so `x-desktop-stable` is `x`).
fn key(text: &str) -> String {
    let mut key: String = text
        .to_lowercase()
        .chars()
        .map(|c| if c == ' ' { '-' } else { c })
        .collect();
    while let Some(shorter) = ["-desktop", "-stable"]
        .iter()
        .find_map(|ending| key.strip_suffix(ending).map(str::to_owned))
    {
        key = shorter;
    }
    key
}

/// How well an entry fits a missing name. `Named` is better: the entry's ID or its `Name=` says
/// the name. `Program` is only that the program it runs has that name, which a launcher for
/// something else (a web-app shortcut that runs the browser) has too.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Rank {
    Named,
    Program,
}

impl Entry {
    /// How the entry fits `target` (a [`key`]), if it does.
    fn rank(&self, target: &str) -> Option<Rank> {
        let stem = self.id.strip_suffix(".desktop").unwrap_or(&self.id);
        if key(stem) == target || key(&self.name) == target {
            return Some(Rank::Named);
        }
        let base = self.program.rsplit('/').next().unwrap_or("");
        (key(base) == target).then_some(Rank::Program)
    }
}

/// The fix for the missing command `name`: the one program that the entries name for it, if it is
/// on this machine. Entries that say the name by their ID or `Name=` come before entries that only
/// run a program of that name, and only the best group that has a usable entry is looked at: a
/// web-app shortcut that runs the browser does not make the browser's own entries ambiguous.
/// Inside that group, two entries are one program when they lead to the same file **and** call it
/// by the same name (`/bin/x` and `/usr/bin/x` are one; an ID in the old style and one in the
/// reverse-DNS style that both run `x` are one). The same file under two names is two programs: a
/// multi-call binary (`ls` and `cat` in one file) does different things for each. Different
/// programs in the best group, or none at all, give `None`: the note stays and the person decides.
/// Ambiguity is never settled by falling to a worse group.
pub(super) fn find_fix(shell: &Shell, entries: &[Entry], name: &str) -> Option<Fix> {
    let target = key(name);
    if target.is_empty() {
        return None;
    }
    // The best rank so far, the first entry of it (in precedence order) with its program's file
    // and name, and whether a different program has been seen in that rank.
    let mut best: Option<(Rank, PathBuf, &str, &Entry)> = None;
    let mut ambiguous = false;
    for entry in entries {
        let Some(rank) = entry.rank(&target) else {
            continue;
        };
        if best.as_ref().is_some_and(|(top, ..)| rank > *top) {
            continue;
        }
        if entry
            .try_exec
            .as_deref()
            .is_some_and(|tried| shell.program_path(tried).is_none())
        {
            continue;
        }
        // It must exist before anything is rewritten (the same question the panel asks).
        if !shell.finds_command(&entry.program) {
            continue;
        }
        let Some(path) = shell.program_path(&entry.program) else {
            continue;
        };
        let real = fs::canonicalize(&path).unwrap_or(path);
        let called = entry.program.rsplit('/').next().unwrap_or("");
        match &best {
            Some((top, first, first_name, _)) if *top == rank => {
                if *first != real || *first_name != called {
                    ambiguous = true;
                }
            }
            // The first usable entry, or a better rank than any before: start again from it.
            _ => {
                best = Some((rank, real, called, entry));
                ambiguous = false;
            }
        }
    }
    let (rank, _, _, entry) = best?;
    if ambiguous {
        return None;
    }
    Some(Fix {
        from: name.to_owned(),
        to: entry.program.clone(),
        // The name of the application only when the entry says the name: an entry that is found
        // by its program alone may be another application (a web-app shortcut).
        app: (rank == Rank::Named).then(|| entry.name.clone()),
    })
}

#[cfg(test)]
mod tests;
