//! The names of the programs in `$PATH`, for completing a command name.
//!
//! Building the index lists each directory of `PATH` once. The kernel tells `readdir` the type
//! of each entry, so no file is looked at (`stat`) while building: that is what keeps it fast
//! on a `PATH` with tens of thousands of files. Whether a name is really a program (a regular
//! file that has an execute bit) is checked only for the names that match what was typed.
//!
//! The index is kept for as long as `PATH` and the modification times of its directories do not
//! change: installing or removing a program changes the time of its directory.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// How often a build looks at its cancel flag while listing one big directory.
const CHECK_EVERY: usize = 512;

/// The programs found in the directories of one value of `PATH`.
#[derive(Debug)]
pub(crate) struct PathIndex {
    /// The value of `PATH` this was built for.
    key: Vec<u8>,
    dirs: Vec<PathBuf>,
    /// The modification time of each directory when it was listed.
    stamps: Vec<Option<SystemTime>>,
    /// `(name, index of the directory)`, sorted: by name, then in `PATH` order.
    names: Vec<(String, u32)>,
}

/// The absolute directories of `path`, in order, without repeats. An empty or relative entry
/// (which means the current directory) is left out: completion offers programs that are in
/// `PATH` proper.
fn directories(path: &[u8]) -> Vec<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let mut out: Vec<PathBuf> = Vec::new();
    for entry in path.split(|&b| b == b':') {
        if entry.first() != Some(&b'/') {
            continue;
        }
        let dir = PathBuf::from(std::ffi::OsStr::from_bytes(entry));
        if !out.contains(&dir) {
            out.push(dir);
        }
    }
    out
}

fn modified(dir: &Path) -> Option<SystemTime> {
    fs::metadata(dir).and_then(|meta| meta.modified()).ok()
}

impl PathIndex {
    /// An index with no programs, for `path`. What a cancelled build leaves behind.
    pub(crate) fn empty(path: &[u8]) -> Self {
        Self {
            key: path.to_vec(),
            dirs: Vec::new(),
            stamps: Vec::new(),
            names: Vec::new(),
        }
    }

    /// Lists the directories of `path`. `cancelled` is asked once in a while; if it says yes the
    /// build stops and gives nothing.
    pub(crate) fn build(path: &[u8], cancelled: &dyn Fn() -> bool) -> Option<Self> {
        let dirs = directories(path);
        let mut stamps = Vec::with_capacity(dirs.len());
        let mut names: Vec<(String, u32)> = Vec::new();
        for (index, dir) in dirs.iter().enumerate() {
            if cancelled() {
                return None;
            }
            // The time is taken before the listing: a change during it shows up as stale next
            // time and the directory is listed again, which is safe.
            stamps.push(modified(dir));
            let Ok(entries) = fs::read_dir(dir) else {
                continue;
            };
            for (count, entry) in entries.flatten().enumerate() {
                if count % CHECK_EVERY == CHECK_EVERY - 1 && cancelled() {
                    return None;
                }
                // `file_type` comes from the directory listing itself; a link counts, and is
                // looked at later if its name matches.
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                if !(kind.is_file() || kind.is_symlink()) {
                    continue;
                }
                let Ok(name) = entry.file_name().into_string() else {
                    continue;
                };
                if name.chars().any(char::is_control) {
                    continue;
                }
                names.push((name, u32::try_from(index).unwrap_or(u32::MAX)));
            }
        }
        names.sort_unstable();
        Some(Self {
            key: path.to_vec(),
            dirs,
            stamps,
            names,
        })
    }

    /// True if this is still the answer for `path`: same value, and no directory changed.
    pub(crate) fn is_fresh(&self, path: &[u8]) -> bool {
        self.key == path
            && self
                .dirs
                .iter()
                .zip(&self.stamps)
                .all(|(dir, stamp)| modified(dir) == *stamp)
    }

    /// How many `(name, directory)` entries the index holds.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.names.len()
    }

    /// The programs whose name starts with `prefix`, sorted, each once, at most `limit`.
    pub(crate) fn matching(&self, prefix: &str, limit: usize) -> Vec<&str> {
        let mut at = self
            .names
            .partition_point(|(name, _)| name.as_str() < prefix);
        let mut out: Vec<&str> = Vec::new();
        while let Some((name, _)) = self.names.get(at) {
            if !name.starts_with(prefix) || out.len() >= limit {
                break;
            }
            // All the entries of one name are next to each other. The name counts if any of its
            // directories holds a working program.
            let group = self.names[at..]
                .iter()
                .take_while(|(other, _)| other == name)
                .count();
            let works = self.names[at..at + group].iter().any(|(_, dir)| {
                self.dirs
                    .get(*dir as usize)
                    .is_some_and(|dir| is_program(&dir.join(name)))
            });
            if works {
                out.push(name);
            }
            at += group;
        }
        out
    }
}

impl PathIndex {
    /// Every name once, in order. Some may not be programs (see [`Self::runs`]).
    pub(crate) fn names(&self) -> impl Iterator<Item = &str> {
        let mut last: Option<&str> = None;
        self.names.iter().filter_map(move |(name, _)| {
            let new = last != Some(name.as_str());
            last = Some(name);
            new.then_some(name.as_str())
        })
    }

    /// True if `name` is a program in one of the directories: a regular file with an execute
    /// bit (a link is followed).
    pub(crate) fn runs(&self, name: &str) -> bool {
        let at = self
            .names
            .partition_point(|(other, _)| other.as_str() < name);
        self.names[at..]
            .iter()
            .take_while(|(other, _)| other == name)
            .any(|(_, dir)| {
                self.dirs
                    .get(*dir as usize)
                    .is_some_and(|dir| is_program(&dir.join(name)))
            })
    }
}

/// A regular file (a link is followed) that somebody may execute.
fn is_program(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("omsh-index-{}-{n}", std::process::id()));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn file(&self, name: &str, mode: u32) -> PathBuf {
            let path = self.0.join(name);
            File::create(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
            path
        }

        fn text(&self) -> String {
            self.0.to_str().unwrap().to_owned()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn never() -> bool {
        false
    }

    fn built(path: &str) -> PathIndex {
        PathIndex::build(path.as_bytes(), &never).unwrap()
    }

    #[test]
    fn names_that_start_with_the_prefix_are_found_sorted() {
        let a = Scratch::new();
        for name in ["git", "gitk", "grep", "ls", "gzip"] {
            a.file(name, 0o755);
        }
        let index = built(&a.text());
        assert_eq!(index.matching("g", 100), ["git", "gitk", "grep", "gzip"]);
        assert_eq!(index.matching("git", 100), ["git", "gitk"]);
        assert_eq!(index.matching("gitk", 100), ["gitk"]);
        assert!(index.matching("x", 100).is_empty());
        assert_eq!(index.matching("", 100).len(), 5);
    }

    #[test]
    fn the_prefix_is_case_sensitive() {
        let a = Scratch::new();
        a.file("Tool", 0o755);
        let index = built(&a.text());
        assert_eq!(index.matching("T", 10), ["Tool"]);
        assert!(index.matching("t", 10).is_empty());
    }

    #[test]
    fn a_file_without_an_execute_bit_is_not_a_program() {
        let a = Scratch::new();
        a.file("tool", 0o644);
        a.file("tool2", 0o755);
        let index = built(&a.text());
        assert_eq!(index.matching("tool", 10), ["tool2"]);
    }

    #[test]
    fn a_directory_is_not_a_program() {
        let a = Scratch::new();
        fs::create_dir(a.0.join("tool")).unwrap();
        let index = built(&a.text());
        assert!(index.matching("tool", 10).is_empty());
        assert_eq!(index.len(), 0, "a folder is not even listed");
    }

    #[test]
    fn a_name_in_two_directories_is_listed_once() {
        let (a, b) = (Scratch::new(), Scratch::new());
        a.file("tool", 0o755);
        b.file("tool", 0o755);
        let index = built(&format!("{}:{}", a.text(), b.text()));
        assert_eq!(index.len(), 2);
        assert_eq!(index.matching("to", 10), ["tool"]);
    }

    #[test]
    fn a_later_directory_can_hold_the_working_program() {
        let (a, b) = (Scratch::new(), Scratch::new());
        a.file("tool", 0o644);
        b.file("tool", 0o755);
        a.file("other", 0o755);
        let index = built(&format!("{}:{}", a.text(), b.text()));
        assert_eq!(index.matching("", 10), ["other", "tool"]);
    }

    #[test]
    fn a_link_to_a_program_counts_and_a_dangling_link_does_not() {
        let a = Scratch::new();
        let real = a.file("real", 0o755);
        std::os::unix::fs::symlink(&real, a.0.join("alias")).unwrap();
        std::os::unix::fs::symlink(a.0.join("nowhere"), a.0.join("dangling")).unwrap();
        let index = built(&a.text());
        assert_eq!(index.matching("", 10), ["alias", "real"]);
    }

    #[test]
    fn the_limit_is_respected() {
        let a = Scratch::new();
        for n in 0..20 {
            a.file(&format!("p{n:02}"), 0o755);
        }
        let index = built(&a.text());
        let found = index.matching("p", 5);
        assert_eq!(found, ["p00", "p01", "p02", "p03", "p04"]);
        assert_eq!(index.matching("p", 0).len(), 0);
    }

    #[test]
    fn relative_empty_and_missing_directories_are_skipped_and_repeats_are_dropped() {
        let a = Scratch::new();
        a.file("tool", 0o755);
        let path = format!(":rel/dir:{0}:/nonexistent-omsh-dir:{0}", a.text());
        let index = built(&path);
        assert_eq!(index.matching("", 10), ["tool"]);
        assert_eq!(
            index.dirs.len(),
            2,
            "one real directory and one that is missing"
        );
    }

    #[test]
    fn a_name_with_a_control_character_is_left_out() {
        let a = Scratch::new();
        a.file("bad\nname", 0o755);
        a.file("good", 0o755);
        assert_eq!(built(&a.text()).matching("", 10), ["good"]);
    }

    #[test]
    fn the_index_is_stale_when_path_or_a_directory_changes() {
        let a = Scratch::new();
        a.file("one", 0o755);
        let path = a.text();
        let index = built(&path);
        assert!(index.is_fresh(path.as_bytes()));
        assert!(!index.is_fresh(b"/usr/bin"), "another PATH");
        // The directory's time moves when a program appears. Some file systems keep a coarse
        // clock, so set the time by hand instead of waiting for it to tick.
        a.file("two", 0o755);
        let file = File::open(&a.0).unwrap();
        file.set_modified(SystemTime::now() + std::time::Duration::from_secs(5))
            .unwrap();
        assert!(!index.is_fresh(path.as_bytes()));
        assert_eq!(built(&path).matching("", 10), ["one", "two"]);
    }

    #[test]
    fn a_cancelled_build_gives_nothing() {
        let a = Scratch::new();
        a.file("tool", 0o755);
        assert!(PathIndex::build(a.text().as_bytes(), &|| true).is_none());
        // Cancelled while listing a big directory.
        for n in 0..(CHECK_EVERY * 2) {
            a.file(&format!("f{n}"), 0o755);
        }
        let calls = std::cell::Cell::new(0);
        let cancel_late = || {
            calls.set(calls.get() + 1);
            calls.get() > 1
        };
        assert!(PathIndex::build(a.text().as_bytes(), &cancel_late).is_none());
    }

    #[test]
    fn a_path_with_nothing_in_it_gives_an_empty_index() {
        let index = built("");
        assert!(index.matching("", 10).is_empty());
        assert!(index.is_fresh(b""));
    }
}
