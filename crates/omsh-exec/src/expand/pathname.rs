//! Pathname expansion: turning a pattern such as `src/*.rs` into the files that match it.

use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use super::pattern::Pattern;
use super::{Field, Kind};

/// The names that match the pattern in `field`, sorted. Empty if none match, or if the field
/// holds no pattern characters: the word then stays as it is.
///
/// Only characters that were not quoted are special. A name that starts with `.` matches only
/// a pattern that starts with a literal `.`, and `.` and `..` never match a pattern.
pub(crate) fn expand(field: &Field, base: Option<&Path>) -> Vec<Vec<u8>> {
    // Where a relative name is looked for: the working directory of a copy of the shell
    // when it has its own. The results stay as written, relative.
    let at = |name: &[u8]| -> PathBuf {
        let path = Path::new(OsStr::from_bytes(name));
        match base {
            Some(base) if path.is_relative() => base.join(path),
            _ => path.to_path_buf(),
        }
    };
    let active: Vec<bool> = field.kinds.iter().map(|k| *k != Kind::Quoted).collect();
    let has_pattern = field
        .bytes
        .iter()
        .zip(&active)
        .any(|(b, a)| *a && matches!(b, b'*' | b'?' | b'['));
    if !has_pattern {
        return Vec::new();
    }
    let bytes = &field.bytes;

    // The pieces between slashes. Empty ones (`a//b`) are kept: the result keeps the slashes
    // as they were written, as dash does. A final empty one means "directories only".
    let mut parts = Vec::new();
    let mut start = 0;
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'/' {
            parts.push((start, i));
            start = i + 1;
        }
    }
    parts.push((start, bytes.len()));

    // Each result so far, with a `/` at the end of every piece but the last.
    let mut found = vec![Vec::new()];
    let mut check_at_end = false;
    for (index, &(s, e)) in parts.iter().enumerate() {
        let last = index + 1 == parts.len();
        let slash: &[u8] = if last { b"" } else { b"/" };
        if s == e {
            // A leading slash, a doubled one, or the final one.
            check_at_end = last;
            for base in &mut found {
                base.extend_from_slice(slash);
            }
            continue;
        }
        let pattern = Pattern::new(&bytes[s..e], &active[s..e]);
        let mut next = Vec::new();
        if pattern.is_literal() {
            // A name that is not there is noticed by the check at the end, or by the next
            // piece, which has to open the directory.
            check_at_end = true;
            for base in &found {
                next.push([base.as_slice(), &bytes[s..e], slash].concat());
            }
        } else {
            check_at_end = false;
            for base in &found {
                let dir = if base.is_empty() {
                    b"."
                } else {
                    base.as_slice()
                };
                let Ok(entries) = fs::read_dir(at(dir)) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let name = name.as_bytes();
                    if name.first() == Some(&b'.') && !pattern.starts_with_dot() {
                        continue;
                    }
                    if pattern.matches(name) {
                        next.push([base.as_slice(), name, slash].concat());
                    }
                }
            }
        }
        found = next;
        if found.is_empty() {
            return found;
        }
    }
    if check_at_end {
        // Also what makes `dir/*/` keep directories only: a file cannot be opened as one.
        found.retain(|path| exists(&at(path)));
    }
    found.sort();
    found
}

fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::os::unix::ffi::OsStringExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    /// A directory with some files in it, removed at the end.
    struct Tree(PathBuf);

    impl Tree {
        fn new(files: &[&str]) -> Self {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!("omsh-glob-{}-{n}", std::process::id()));
            fs::create_dir_all(&root).unwrap();
            for file in files {
                let path = root.join(file);
                if file.ends_with('/') {
                    fs::create_dir_all(&path).unwrap();
                } else {
                    fs::create_dir_all(path.parent().unwrap()).unwrap();
                    File::create(path).unwrap();
                }
            }
            Self(root)
        }

        /// Expands `pattern` (relative to the tree), all characters unquoted.
        fn glob(&self, pattern: &str) -> Vec<String> {
            let full = format!("{}/{pattern}", self.0.display());
            let field = Field {
                kinds: vec![Kind::Literal; full.len()],
                bytes: full.into_bytes(),
                forced: false,
            };
            let prefix = format!("{}/", self.0.display());
            expand(&field, None)
                .into_iter()
                .map(|p| {
                    String::from_utf8_lossy(&p)
                        .strip_prefix(&prefix)
                        .unwrap()
                        .to_owned()
                })
                .collect()
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_star_matches_names_and_the_result_is_sorted() {
        let tree = Tree::new(&["b.rs", "a.rs", "c.txt"]);
        assert_eq!(tree.glob("*.rs"), ["a.rs", "b.rs"]);
        assert_eq!(tree.glob("?.txt"), ["c.txt"]);
        assert_eq!(tree.glob("[ab].rs"), ["a.rs", "b.rs"]);
        assert_eq!(tree.glob("[!a].rs"), ["b.rs"]);
    }

    #[test]
    fn no_match_gives_nothing() {
        let tree = Tree::new(&["a.rs"]);
        assert!(tree.glob("*.txt").is_empty());
        assert!(tree.glob("nodir/*").is_empty());
    }

    #[test]
    fn a_field_without_pattern_characters_is_left_alone() {
        let field = Field {
            bytes: b"plain".to_vec(),
            kinds: vec![Kind::Literal; 5],
            forced: false,
        };
        assert!(expand(&field, None).is_empty());
    }

    #[test]
    fn a_quoted_star_is_not_a_pattern() {
        let tree = Tree::new(&["a.rs"]);
        let full = format!("{}/*.rs", tree.0.display());
        let mut kinds = vec![Kind::Literal; full.len()];
        let star = full.find('*').unwrap();
        kinds[star] = Kind::Quoted;
        let field = Field {
            bytes: full.into_bytes(),
            kinds,
            forced: true,
        };
        assert!(expand(&field, None).is_empty());
    }

    #[test]
    fn hidden_names_need_a_literal_dot() {
        let tree = Tree::new(&[".hidden", "shown"]);
        assert_eq!(tree.glob("*"), ["shown"]);
        assert_eq!(tree.glob(".*"), [".hidden"]);
        assert_eq!(tree.glob(".h*"), [".hidden"]);
        assert_eq!(tree.glob("[.]h*"), Vec::<String>::new());
    }

    #[test]
    fn dot_and_dot_dot_never_match() {
        let tree = Tree::new(&[".a"]);
        assert_eq!(tree.glob(".*"), [".a"]);
        // `..` has two dots, so `.?` could only match it, and it is never listed.
        assert_eq!(tree.glob(".?"), [".a"]);
        assert!(tree.glob(".[.]").is_empty());
    }

    #[test]
    fn patterns_can_cross_directories() {
        let tree = Tree::new(&["d1/x.c", "d2/x.c", "d2/y.h", "f"]);
        assert_eq!(tree.glob("*/x.c"), ["d1/x.c", "d2/x.c"]);
        assert_eq!(tree.glob("d*/*.h"), ["d2/y.h"]);
        assert_eq!(tree.glob("d2/*"), ["d2/x.c", "d2/y.h"]);
        // A file in the middle of a path cannot be opened as a directory.
        assert!(tree.glob("f/*").is_empty());
    }

    #[test]
    fn a_trailing_slash_keeps_directories_only() {
        let tree = Tree::new(&["dir/", "file"]);
        assert_eq!(tree.glob("*/"), ["dir/"]);
        assert_eq!(tree.glob("*"), ["dir", "file"]);
    }

    #[test]
    fn a_name_that_is_not_utf8_is_found() {
        let tree = Tree::new(&[]);
        let name = std::ffi::OsString::from_vec(b"bad\xffname".to_vec());
        File::create(tree.0.join(name)).unwrap();
        let full = format!("{}/bad*", tree.0.display());
        let field = Field {
            kinds: vec![Kind::Literal; full.len()],
            bytes: full.into_bytes(),
            forced: false,
        };
        let found = expand(&field, None);
        assert_eq!(found.len(), 1);
        assert!(found[0].ends_with(b"bad\xffname"));
    }

    #[test]
    fn a_multibyte_name_matches_a_question_mark() {
        let tree = Tree::new(&["বাংলা.txt"]);
        // Five characters, fifteen bytes.
        assert_eq!(tree.glob("?????.txt"), ["বাংলা.txt"]);
        assert_eq!(tree.glob("বা???.txt"), ["বাংলা.txt"]);
        assert!(tree.glob("????.txt").is_empty());
    }
}
