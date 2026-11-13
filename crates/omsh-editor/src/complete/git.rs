//! Branch, tag and remote names for completing `git` arguments.
//!
//! The names are read from the files of the repository (`refs/heads`, `refs/remotes`,
//! `refs/tags` and `packed-refs`). **No process is started**: the shell never runs `git` for
//! completion, so it works without git installed and cannot be tricked by a repository's
//! configuration into running a program.

use std::fs::{self, File};
use std::io::Read;
use std::path::Path;

use crate::segments::git_dir;

/// How deep a branch name like `feature/a/b` may nest.
const MAX_DEPTH: usize = 12;
/// How many reference files are read from one tree.
const MAX_REFS: usize = 20_000;
/// `packed-refs` is read up to this size.
const PACKED_LIMIT: u64 = 8 * 1024 * 1024;

/// The names a repository has.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Refs {
    /// Local branches: `main`, `feature/x`.
    pub branches: Vec<String>,
    /// Branches of remotes: `origin/main`.
    pub remote_branches: Vec<String>,
    /// The names of the remotes: `origin`.
    pub remotes: Vec<String>,
    pub tags: Vec<String>,
}

/// Reads the references of the repository that holds `cwd`. `None` if there is none.
pub(crate) fn refs(cwd: &Path) -> Option<Refs> {
    let git = git_dir(cwd)?;
    // A linked worktree keeps its own HEAD but shares the references with the main copy.
    let common = match fs::read_to_string(git.join("commondir")) {
        Ok(text) if !text.trim().is_empty() => git.join(text.trim()),
        _ => git,
    };
    let mut found = Refs::default();
    let mut budget = MAX_REFS;
    walk(
        &common.join("refs/heads"),
        "",
        0,
        &mut budget,
        &mut found.branches,
    );
    walk(
        &common.join("refs/remotes"),
        "",
        0,
        &mut budget,
        &mut found.remote_branches,
    );
    walk(
        &common.join("refs/tags"),
        "",
        0,
        &mut budget,
        &mut found.tags,
    );
    if let Some(text) = read_limited(&common.join("packed-refs")) {
        for line in text.lines() {
            // `# pack-refs …` is the header, `^sha` the target of the tag on the line before.
            if line.starts_with(['#', '^']) {
                continue;
            }
            let Some((_, name)) = line.split_once(' ') else {
                continue;
            };
            let name = name.trim();
            if let Some(branch) = name.strip_prefix("refs/heads/") {
                found.branches.push(branch.to_owned());
            } else if let Some(branch) = name.strip_prefix("refs/remotes/") {
                found.remote_branches.push(branch.to_owned());
            } else if let Some(tag) = name.strip_prefix("refs/tags/") {
                found.tags.push(tag.to_owned());
            }
        }
    }
    found.remote_branches.retain(|name| !is_remote_head(name));
    for list in [
        &mut found.branches,
        &mut found.remote_branches,
        &mut found.tags,
    ] {
        list.retain(|name| is_usable(name));
        list.sort_unstable();
        list.dedup();
    }
    found.remotes = found
        .remote_branches
        .iter()
        .filter_map(|name| name.split_once('/').map(|(remote, _)| remote.to_owned()))
        .collect();
    found.remotes.dedup();
    Some(found)
}

/// `origin/HEAD` is a pointer, not a branch.
fn is_remote_head(name: &str) -> bool {
    name.rsplit('/').next() == Some("HEAD")
}

/// A name without control characters, and not a leftover lock file.
fn is_usable(name: &str) -> bool {
    !name.is_empty() && !name.ends_with(".lock") && !name.chars().any(char::is_control)
}

/// Adds the names of the files under `dir` to `out`, with the folder names in front.
fn walk(dir: &Path, prefix: &str, depth: usize, budget: &mut usize, out: &mut Vec<String>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if *budget == 0 {
            return;
        }
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let full = format!("{prefix}{name}");
        if kind.is_dir() {
            walk(&entry.path(), &format!("{full}/"), depth + 1, budget, out);
        } else if kind.is_file() {
            *budget -= 1;
            out.push(full);
        }
    }
}

fn read_limited(path: &Path) -> Option<String> {
    let mut text = String::new();
    File::open(path)
        .ok()?
        .take(PACKED_LIMIT)
        .read_to_string(&mut text)
        .ok()?;
    Some(text)
}

/// The commands of git that are offered at the subcommand position.
pub(crate) const SUBCOMMANDS: &[&str] = &[
    "add",
    "bisect",
    "blame",
    "branch",
    "checkout",
    "cherry",
    "cherry-pick",
    "clean",
    "clone",
    "commit",
    "config",
    "describe",
    "diff",
    "fetch",
    "grep",
    "init",
    "log",
    "merge",
    "mv",
    "pull",
    "push",
    "rebase",
    "reflog",
    "remote",
    "reset",
    "restore",
    "revert",
    "rm",
    "show",
    "stash",
    "status",
    "switch",
    "tag",
    "worktree",
];

/// Where in a `git` command line the cursor is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Place {
    /// The name of the git command.
    Subcommand,
    /// An argument that is a branch, tag or commit name.
    Ref,
    /// An argument that is the name of a remote.
    Remote,
    /// Anything else: file names.
    Other,
}

/// Global options of `git` that take the next word as their value.
const OPTIONS_WITH_VALUE: &[&str] = &["-C", "-c", "--git-dir", "--work-tree", "--namespace"];

/// What the next argument of `git` is, given the arguments in front of it (without quotes).
pub(crate) fn place(before: &[String]) -> Place {
    let mut words = before.iter();
    let mut subcommand = None;
    while let Some(word) = words.next() {
        if OPTIONS_WITH_VALUE.contains(&word.as_str()) {
            words.next();
        } else if !word.starts_with('-') {
            subcommand = Some(word.as_str());
            break;
        }
    }
    let Some(subcommand) = subcommand else {
        return Place::Subcommand;
    };
    let positional = words.filter(|word| !word.starts_with('-')).count();
    match subcommand {
        "push" | "pull" | "fetch" if positional == 0 => Place::Remote,
        "push" | "pull" | "fetch" | "checkout" | "switch" | "branch" | "merge" | "rebase"
        | "cherry" | "cherry-pick" | "diff" | "log" | "show" | "reset" | "revert" | "tag" => {
            Place::Ref
        }
        _ => Place::Other,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("omsh-git-{}-{n}", std::process::id()));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn write(&self, relative: &str, text: &str) {
            let path = self.0.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn words(list: &[&str]) -> Vec<String> {
        list.iter().map(|word| (*word).to_owned()).collect()
    }

    #[test]
    fn loose_references_are_read_with_their_folders() {
        let s = Scratch::new();
        s.write("r/.git/refs/heads/main", "x\n");
        s.write("r/.git/refs/heads/feature/login", "x\n");
        s.write("r/.git/refs/remotes/origin/main", "x\n");
        s.write(
            "r/.git/refs/remotes/origin/HEAD",
            "ref: refs/remotes/origin/main\n",
        );
        s.write("r/.git/refs/remotes/upstream/dev", "x\n");
        s.write("r/.git/refs/tags/v1", "x\n");
        s.write("r/.git/refs/heads/stale.lock", "x\n");
        fs::create_dir_all(s.0.join("r/sub/dir")).unwrap();
        let found = refs(&s.0.join("r/sub/dir")).unwrap();
        assert_eq!(found.branches, ["feature/login", "main"]);
        assert_eq!(found.remote_branches, ["origin/main", "upstream/dev"]);
        assert_eq!(found.remotes, ["origin", "upstream"]);
        assert_eq!(found.tags, ["v1"]);
    }

    #[test]
    fn packed_references_are_read_and_mixed_with_loose_ones() {
        let s = Scratch::new();
        s.write("r/.git/refs/heads/main", "x\n");
        s.write(
            "r/.git/packed-refs",
            "# pack-refs with: peeled fully-peeled sorted\n\
             1111 refs/heads/main\n\
             2222 refs/heads/old/topic\n\
             3333 refs/remotes/origin/main\n\
             4444 refs/tags/v2\n\
             ^5555\n\
             6666 refs/stash\n\
             garbage-line\n",
        );
        let found = refs(&s.0.join("r")).unwrap();
        assert_eq!(found.branches, ["main", "old/topic"], "main once");
        assert_eq!(found.remote_branches, ["origin/main"]);
        assert_eq!(found.tags, ["v2"]);
    }

    #[test]
    fn a_worktree_reads_the_references_of_the_main_copy() {
        let s = Scratch::new();
        s.write("main/.git/refs/heads/shared", "x\n");
        s.write("main/.git/worktrees/w/HEAD", "ref: refs/heads/shared\n");
        s.write("main/.git/worktrees/w/commondir", "../..\n");
        s.write(
            "w/.git",
            &format!("gitdir: {}\n", s.0.join("main/.git/worktrees/w").display()),
        );
        let found = refs(&s.0.join("w")).unwrap();
        assert_eq!(found.branches, ["shared"]);
    }

    #[test]
    fn no_repository_or_an_empty_one_gives_what_there_is() {
        let s = Scratch::new();
        fs::create_dir_all(s.0.join("plain")).unwrap();
        // The scratch folder is under the temporary folder, outside any repository.
        assert!(refs(&s.0.join("plain")).is_none());
        fs::create_dir_all(s.0.join("empty/.git")).unwrap();
        assert_eq!(refs(&s.0.join("empty")).unwrap(), Refs::default());
    }

    #[test]
    fn a_name_with_a_control_character_is_left_out() {
        let s = Scratch::new();
        s.write(
            "r/.git/packed-refs",
            "1 refs/heads/ok\n2 refs/heads/bad\u{1b}name\n",
        );
        assert_eq!(refs(&s.0.join("r")).unwrap().branches, ["ok"]);
    }

    #[test]
    fn a_very_deep_tree_is_cut() {
        let s = Scratch::new();
        let deep = (0..30)
            .map(|n| format!("d{n}"))
            .collect::<Vec<_>>()
            .join("/");
        s.write(&format!("r/.git/refs/heads/{deep}/leaf"), "x\n");
        s.write("r/.git/refs/heads/top", "x\n");
        assert_eq!(refs(&s.0.join("r")).unwrap().branches, ["top"]);
    }

    #[test]
    fn cherry_is_offered_as_a_subcommand_and_the_list_stays_sorted() {
        assert!(SUBCOMMANDS.contains(&"cherry"));
        let mut sorted = SUBCOMMANDS.to_vec();
        sorted.sort_unstable();
        assert_eq!(sorted, SUBCOMMANDS);
    }

    #[test]
    fn the_place_of_the_cursor_in_a_git_command() {
        let p = |list: &[&str]| place(&words(list));
        assert_eq!(p(&[]), Place::Subcommand);
        assert_eq!(p(&["-C", "dir"]), Place::Subcommand);
        assert_eq!(p(&["--no-pager"]), Place::Subcommand);
        assert_eq!(p(&["checkout"]), Place::Ref);
        assert_eq!(p(&["cherry"]), Place::Ref);
        assert_eq!(p(&["-C", "dir", "checkout"]), Place::Ref);
        assert_eq!(p(&["-c", "a.b=c", "switch", "-c"]), Place::Ref);
        assert_eq!(p(&["push"]), Place::Remote);
        assert_eq!(p(&["push", "-u"]), Place::Remote);
        assert_eq!(p(&["push", "origin"]), Place::Ref);
        assert_eq!(p(&["fetch"]), Place::Remote);
        assert_eq!(p(&["add"]), Place::Other);
        assert_eq!(p(&["commit", "-m"]), Place::Other);
    }
}
