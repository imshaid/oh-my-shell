//! Finding a command name in `$PATH`, with a cache.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use nix::unistd::{AccessFlags, access};

/// What dash uses when `PATH` is not set at all.
pub const DEFAULT_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// The result of looking for a command name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Lookup {
    /// Run this file. A name with a `/` in it is returned as it is: the kernel reports
    /// any problem when it runs.
    Found(PathBuf),
    /// A file with this name exists, but it is not executable, and nothing better was found.
    Denied(PathBuf),
    /// Nothing with this name.
    NotFound,
}

/// Remembers where command names were found, for one value of `$PATH`.
///
/// The cache is keyed by the `PATH` string: a lookup with a different `PATH` empties it first.
/// A hit is checked again before it is used, so a program that was deleted is searched for
/// again, and a miss is never stored, so a program that was just installed is found.
#[derive(Debug, Default)]
pub struct PathCache {
    path_value: String,
    found: HashMap<String, PathBuf>,
}

impl PathCache {
    /// An empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Forgets everything. This is what `hash -r` does.
    pub fn clear(&mut self) {
        self.found.clear();
    }

    /// How many names are remembered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.found.len()
    }

    /// True if no name is remembered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.found.is_empty()
    }

    /// Finds `name`, searching the directories of `path_value` in order.
    ///
    /// The first executable regular file wins. If there is none but a regular file with that
    /// name exists, the result is [`Lookup::Denied`], as in bash.
    pub fn lookup(&mut self, name: &str, path_value: &str) -> Lookup {
        if name.contains('/') {
            return Lookup::Found(PathBuf::from(name));
        }
        if self.path_value != path_value {
            self.found.clear();
            path_value.clone_into(&mut self.path_value);
        }
        if let Some(hit) = self.found.get(name) {
            if is_executable_file(hit) {
                return Lookup::Found(hit.clone());
            }
            self.found.remove(name);
        }
        let mut denied = None;
        for dir in path_value.split(':') {
            // An empty entry means the current directory.
            let candidate = if dir.is_empty() {
                Path::new(".").join(name)
            } else {
                Path::new(dir).join(name)
            };
            if is_executable_file(&candidate) {
                self.found.insert(name.to_owned(), candidate.clone());
                return Lookup::Found(candidate);
            }
            if denied.is_none() && is_regular_file(&candidate) {
                denied = Some(candidate);
            }
        }
        denied.map_or(Lookup::NotFound, Lookup::Denied)
    }
}

fn is_regular_file(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|meta| meta.is_file())
}

/// True for a regular file that this process may execute. `access` is used, and not the
/// permission bits, so that the owner, the group and root are all treated as the kernel will.
fn is_executable_file(path: &Path) -> bool {
    is_regular_file(path) && access(path, AccessFlags::X_OK).is_ok()
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    /// A directory that is removed when it goes out of scope.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("omsh-path-{}-{n}", std::process::id()));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn file(&self, name: &str, mode: u32) -> PathBuf {
            let path = self.0.join(name);
            File::create(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
            path
        }

        fn as_str(&self) -> &str {
            self.0.to_str().unwrap()
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_first_directory_with_the_command_wins() {
        let (a, b) = (TempDir::new(), TempDir::new());
        let in_a = a.file("tool", 0o755);
        b.file("tool", 0o755);
        let path = format!("{}:{}", a.as_str(), b.as_str());
        assert_eq!(PathCache::new().lookup("tool", &path), Lookup::Found(in_a));
    }

    #[test]
    fn a_file_without_execute_permission_is_skipped() {
        let (a, b) = (TempDir::new(), TempDir::new());
        a.file("tool", 0o644);
        let in_b = b.file("tool", 0o755);
        let path = format!("{}:{}", a.as_str(), b.as_str());
        assert_eq!(PathCache::new().lookup("tool", &path), Lookup::Found(in_b));
    }

    #[test]
    fn only_a_non_executable_file_gives_denied() {
        let a = TempDir::new();
        let file = a.file("tool", 0o644);
        assert_eq!(
            PathCache::new().lookup("tool", a.as_str()),
            Lookup::Denied(file)
        );
    }

    #[test]
    fn a_directory_is_not_a_command() {
        let a = TempDir::new();
        fs::create_dir(a.0.join("tool")).unwrap();
        assert_eq!(
            PathCache::new().lookup("tool", a.as_str()),
            Lookup::NotFound
        );
    }

    #[test]
    fn a_missing_command_is_not_found() {
        let a = TempDir::new();
        assert_eq!(
            PathCache::new().lookup("tool", a.as_str()),
            Lookup::NotFound
        );
    }

    #[test]
    fn a_name_with_a_slash_skips_the_search() {
        let mut cache = PathCache::new();
        assert_eq!(
            cache.lookup("./nothing/here", ""),
            Lookup::Found(PathBuf::from("./nothing/here"))
        );
        assert!(cache.is_empty());
    }

    #[test]
    fn an_empty_path_entry_means_the_current_directory() {
        let found = PathCache::new().lookup("Cargo.toml", "/nonexistent-omsh:");
        // Cargo.toml is a regular file but not executable, so it is `Denied`, and that proves
        // that the empty entry was searched in the current directory (the crate's own).
        assert_eq!(found, Lookup::Denied(PathBuf::from("./Cargo.toml")));
    }

    #[test]
    fn a_hit_is_remembered_and_a_removed_program_is_searched_for_again() {
        let (a, b) = (TempDir::new(), TempDir::new());
        let in_a = a.file("tool", 0o755);
        let in_b = b.file("tool", 0o755);
        let path = format!("{}:{}", a.as_str(), b.as_str());
        let mut cache = PathCache::new();

        assert_eq!(cache.lookup("tool", &path), Lookup::Found(in_a.clone()));
        assert_eq!(cache.len(), 1);

        fs::remove_file(&in_a).unwrap();
        assert_eq!(cache.lookup("tool", &path), Lookup::Found(in_b));
    }

    #[test]
    fn a_miss_is_not_remembered_so_a_new_program_is_found() {
        let a = TempDir::new();
        let mut cache = PathCache::new();
        assert_eq!(cache.lookup("tool", a.as_str()), Lookup::NotFound);

        let installed = a.file("tool", 0o755);
        assert_eq!(cache.lookup("tool", a.as_str()), Lookup::Found(installed));
    }

    #[test]
    fn a_new_path_value_empties_the_cache() {
        let (a, b) = (TempDir::new(), TempDir::new());
        let in_a = a.file("tool", 0o755);
        let in_b = b.file("tool", 0o755);
        let mut cache = PathCache::new();

        assert_eq!(cache.lookup("tool", a.as_str()), Lookup::Found(in_a));
        assert_eq!(cache.lookup("tool", b.as_str()), Lookup::Found(in_b));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn clear_forgets_everything() {
        let a = TempDir::new();
        a.file("tool", 0o755);
        let mut cache = PathCache::new();
        cache.lookup("tool", a.as_str());
        cache.clear();
        assert!(cache.is_empty());
    }
}
