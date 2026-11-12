//! Completing file names: the entries of one directory that start with what was typed.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// How many directory entries are looked at at most. A folder with more than this many files is
/// not read to the end: completing must not hang on a huge folder (or a stalled mount).
const MAX_SCANNED: usize = 50_000;

/// What to complete.
pub(crate) struct Query<'a> {
    /// The shell's working directory.
    pub cwd: &'a Path,
    pub home: Option<&'a Path>,
    /// The word starts with an unquoted `~`.
    pub tilde: bool,
    /// What was typed, without quotes and backslashes.
    pub text: &'a str,
    /// Only folders (the argument of `cd`).
    pub dirs_only: bool,
    /// Folders and files that can be run (a command name that has a `/` in it).
    pub programs_only: bool,
}

/// One name that fits.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    /// The typed folder part and the name: what the word becomes (a folder without its `/`).
    pub full: String,
    /// The name alone.
    pub name: String,
    pub is_dir: bool,
}

/// The entries that fit `query`, sorted, at most `limit`. Names that are not valid UTF-8 are
/// left out. Hidden names (starting with a dot) only if a dot was typed.
pub(crate) fn list(query: &Query<'_>, limit: usize) -> Vec<Entry> {
    if query.tilde && query.text == "~" {
        // `~` alone becomes `~/`.
        return if query.home.is_some() {
            vec![Entry {
                full: "~".to_owned(),
                name: "~".to_owned(),
                is_dir: true,
            }]
        } else {
            Vec::new()
        };
    }
    let (dir_text, prefix) = match query.text.rfind('/') {
        Some(at) => query.text.split_at(at + 1),
        None => ("", query.text),
    };
    let base: PathBuf = if dir_text.is_empty() {
        if query.tilde {
            // `~user` and the like: other people's home folders are not looked up.
            return Vec::new();
        }
        query.cwd.to_path_buf()
    } else if query.tilde {
        let Some(rest) = dir_text.strip_prefix("~/") else {
            return Vec::new();
        };
        let Some(home) = query.home else {
            return Vec::new();
        };
        home.join(rest)
    } else if dir_text.starts_with('/') {
        PathBuf::from(dir_text)
    } else {
        query.cwd.join(dir_text)
    };
    let Ok(entries) = fs::read_dir(&base) else {
        return Vec::new();
    };
    let show_hidden = prefix.starts_with('.');
    let mut found = Vec::new();
    for entry in entries.flatten().take(MAX_SCANNED) {
        if found.len() >= limit {
            break;
        }
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if !name.starts_with(prefix) || (name.starts_with('.') && !show_hidden) {
            continue;
        }
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let is_dir = kind.is_dir() || (kind.is_symlink() && is_dir_through_link(&entry.path()));
        if query.dirs_only && !is_dir {
            continue;
        }
        if query.programs_only && !is_dir && !is_program(&entry.path()) {
            continue;
        }
        found.push(Entry {
            full: format!("{dir_text}{name}"),
            name,
            is_dir,
        });
    }
    found.sort_by_cached_key(|entry| (entry.name.to_lowercase(), entry.name.clone()));
    found
}

fn is_dir_through_link(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|meta| meta.is_dir())
}

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
            let dir = std::env::temp_dir().join(format!("omsh-paths-{}-{n}", std::process::id()));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn file(&self, name: &str) {
            File::create(self.0.join(name)).unwrap();
        }

        fn dir(&self, name: &str) {
            fs::create_dir_all(self.0.join(name)).unwrap();
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn query<'a>(cwd: &'a Path, text: &'a str) -> Query<'a> {
        Query {
            cwd,
            home: None,
            tilde: false,
            text,
            dirs_only: false,
            programs_only: false,
        }
    }

    fn fulls(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(|entry| entry.full.as_str()).collect()
    }

    #[test]
    fn names_in_the_working_directory_that_start_with_the_text() {
        let s = Scratch::new();
        s.file("alpha");
        s.file("alps");
        s.file("beta");
        s.dir("alder");
        let found = list(&query(&s.0, "al"), 100);
        assert_eq!(fulls(&found), ["alder", "alpha", "alps"]);
        assert!(found[0].is_dir && !found[1].is_dir);
        assert_eq!(list(&query(&s.0, ""), 100).len(), 4);
        assert!(list(&query(&s.0, "zz"), 100).is_empty());
    }

    #[test]
    fn a_folder_part_is_kept_in_front_of_the_name() {
        let s = Scratch::new();
        s.dir("src/deep");
        File::create(s.0.join("src/main.rs")).unwrap();
        let found = list(&query(&s.0, "src/"), 100);
        assert_eq!(fulls(&found), ["src/deep", "src/main.rs"]);
        assert_eq!(found[1].name, "main.rs");
        let found = list(&query(&s.0, "src/ma"), 100);
        assert_eq!(fulls(&found), ["src/main.rs"]);
    }

    #[test]
    fn an_absolute_folder_is_read_as_it_is() {
        let s = Scratch::new();
        s.file("here");
        let text = format!("{}/he", s.0.display());
        let found = list(&query(Path::new("/"), &text), 100);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "here");
    }

    #[test]
    fn hidden_names_need_a_dot() {
        let s = Scratch::new();
        s.file(".hidden");
        s.file("shown");
        assert_eq!(fulls(&list(&query(&s.0, ""), 100)), ["shown"]);
        assert_eq!(fulls(&list(&query(&s.0, "."), 100)), [".hidden"]);
        assert_eq!(fulls(&list(&query(&s.0, ".h"), 100)), [".hidden"]);
    }

    #[test]
    fn only_folders_when_asked() {
        let s = Scratch::new();
        s.file("file");
        s.dir("folder");
        let mut q = query(&s.0, "");
        q.dirs_only = true;
        assert_eq!(fulls(&list(&q, 100)), ["folder"]);
    }

    #[test]
    fn a_link_to_a_folder_counts_as_a_folder() {
        let s = Scratch::new();
        s.dir("real");
        std::os::unix::fs::symlink(s.0.join("real"), s.0.join("link")).unwrap();
        std::os::unix::fs::symlink(s.0.join("nowhere"), s.0.join("dangling")).unwrap();
        let mut q = query(&s.0, "");
        q.dirs_only = true;
        assert_eq!(fulls(&list(&q, 100)), ["link", "real"]);
    }

    #[test]
    fn programs_only_keeps_folders_and_files_that_can_be_run() {
        let s = Scratch::new();
        s.file("data");
        File::create(s.0.join("run")).unwrap();
        fs::set_permissions(s.0.join("run"), fs::Permissions::from_mode(0o755)).unwrap();
        s.dir("folder");
        let mut q = query(&s.0, "");
        q.programs_only = true;
        assert_eq!(fulls(&list(&q, 100)), ["folder", "run"]);
    }

    #[test]
    fn the_tilde_means_the_home_directory() {
        let s = Scratch::new();
        s.dir("proj");
        s.file("pad");
        // A name in the working directory that starts like `~us`: it must not be found.
        let cwd = Scratch::new();
        cwd.file("~usual");
        let mut q = query(&cwd.0, "~/p");
        q.tilde = true;
        q.home = Some(&s.0);
        let found = list(&q, 100);
        assert_eq!(fulls(&found), ["~/pad", "~/proj"]);
        q.text = "~";
        assert_eq!(fulls(&list(&q, 100)), ["~"]);
        q.text = "~us";
        assert!(list(&q, 100).is_empty(), "other users are not looked up");
        q.text = "~root/";
        assert!(list(&q, 100).is_empty());
        q.text = "~";
        q.home = None;
        assert!(list(&q, 100).is_empty(), "no home known");
    }

    #[test]
    fn a_tilde_that_is_not_one_is_just_a_name() {
        let s = Scratch::new();
        s.file("~odd");
        assert_eq!(fulls(&list(&query(&s.0, "~o"), 100)), ["~odd"]);
    }

    #[test]
    fn a_folder_that_cannot_be_read_gives_nothing() {
        let s = Scratch::new();
        assert!(list(&query(&s.0, "nosuch/x"), 100).is_empty());
        s.file("plain");
        assert!(list(&query(&s.0, "plain/x"), 100).is_empty());
    }

    #[test]
    fn the_limit_stops_the_listing_and_the_result_is_sorted() {
        let s = Scratch::new();
        for n in 0..30 {
            s.file(&format!("f{n:02}"));
        }
        let found = list(&query(&s.0, "f"), 10);
        assert_eq!(found.len(), 10);
        let names: Vec<_> = found.iter().map(|entry| entry.name.clone()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }

    #[test]
    fn names_are_sorted_ignoring_case() {
        let s = Scratch::new();
        for name in ["b", "A", "a", "B"] {
            s.file(name);
        }
        assert_eq!(fulls(&list(&query(&s.0, ""), 100)), ["A", "a", "B", "b"]);
    }

    #[test]
    fn a_name_that_is_not_utf8_is_left_out() {
        use std::os::unix::ffi::OsStrExt;
        let s = Scratch::new();
        File::create(s.0.join(std::ffi::OsStr::from_bytes(b"bad\xff"))).unwrap();
        s.file("good");
        assert_eq!(fulls(&list(&query(&s.0, ""), 100)), ["good"]);
    }
}
