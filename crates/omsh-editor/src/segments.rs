//! The pieces of the prompt (blueprint §10): working directory, git branch, virtual
//! environment, last status, last duration and the time.
//!
//! Everything here is plain text from plain facts, so that each rule can be tested. Nothing
//! starts a process: the git branch is read from `.git/HEAD`. (S15 makes the git part
//! asynchronous and adds the dirty marker.)

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A name from `[prompt] left` / `right` in `config.toml`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Segment {
    Cwd,
    Git,
    Venv,
    Status,
    Duration,
    Time,
}

impl Segment {
    /// The segment for a configured name. `omsh-config` already refuses other names; an
    /// unknown one gives `None` and is skipped.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "cwd" => Self::Cwd,
            "git" => Self::Git,
            "venv" => Self::Venv,
            "status" => Self::Status,
            "duration" => Self::Duration,
            "time" => Self::Time,
            _ => return None,
        })
    }
}

/// How many directories at the end of a long path are kept.
const KEPT_PARTS: usize = 3;

/// The working directory as shown in the prompt: `$HOME` becomes `~`, and a path with more than
/// three parts keeps the last three after `…/` (`.../` without Unicode).
#[must_use]
pub fn cwd(path: &Path, home: Option<&Path>, unicode: bool) -> String {
    let text = path.to_string_lossy();
    let (anchor, rest): (&str, String) = match home {
        // `strip_prefix` works on whole components, so `/home/surjo2` is not under `/home/surjo`.
        Some(home) if !home.as_os_str().is_empty() && home != Path::new("/") => {
            match path.strip_prefix(home) {
                Ok(inside) => ("~", inside.to_string_lossy().into_owned()),
                Err(_) => split_root(&text),
            }
        }
        _ => split_root(&text),
    };
    let parts: Vec<&str> = rest.split('/').filter(|part| !part.is_empty()).collect();
    if parts.len() > KEPT_PARTS {
        let ellipsis = if unicode { "…" } else { "..." };
        return format!("{ellipsis}/{}", parts[parts.len() - KEPT_PARTS..].join("/"));
    }
    if parts.is_empty() {
        return if anchor.is_empty() { "/" } else { anchor }.to_owned();
    }
    // `anchor` is `~` (then a slash follows), `/` for an absolute path.
    match anchor {
        "~" => format!("~/{}", parts.join("/")),
        _ => format!("/{}", parts.join("/")),
    }
}

fn split_root(text: &str) -> (&'static str, String) {
    ("", text.to_owned())
}

/// What `.git/HEAD` says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Head {
    /// On a branch.
    Branch(String),
    /// Detached: the first seven characters of the commit.
    Detached(String),
}

impl Head {
    #[must_use]
    pub fn label(&self) -> &str {
        match self {
            Self::Branch(name) | Self::Detached(name) => name,
        }
    }
}

/// `HEAD` is a line or two; more than this is not a git file.
const HEAD_LIMIT: u64 = 4096;

/// The branch of the repository that holds `dir`: the first `.git` found going up. A `.git`
/// *file* (a worktree or submodule) says `gitdir: <path>`.
///
/// A file system that does not answer can stall this; the time-limited, asynchronous version is
/// S15's job.
#[must_use]
pub fn git_head(dir: &Path) -> Option<Head> {
    parse_head(&read_small(&git_dir(dir)?.join("HEAD"))?)
}

/// The git directory of the repository that holds `dir`: the first `.git` found going up, or
/// the place a `.git` *file* (`gitdir: <path>`, a worktree or submodule) points to. It is not
/// checked that the directory has a `HEAD`.
#[must_use]
pub(crate) fn git_dir(dir: &Path) -> Option<PathBuf> {
    for candidate in dir.ancestors() {
        let dot_git = candidate.join(".git");
        let Ok(meta) = fs::metadata(&dot_git) else {
            continue;
        };
        if meta.is_dir() {
            return Some(dot_git);
        }
        if meta.is_file() {
            let text = read_small(&dot_git)?;
            let target = text.trim().strip_prefix("gitdir:")?.trim();
            return Some(candidate.join(target));
        }
        return None;
    }
    None
}

/// Reads a regular file of at most [`HEAD_LIMIT`] bytes. A FIFO or a device is refused before it
/// is opened, since opening one can wait for ever.
fn read_small(path: &Path) -> Option<String> {
    if !fs::metadata(path).ok()?.is_file() {
        return None;
    }
    let mut text = String::new();
    File::open(path)
        .ok()?
        .take(HEAD_LIMIT)
        .read_to_string(&mut text)
        .ok()?;
    Some(text)
}

fn parse_head(text: &str) -> Option<Head> {
    let line = text.lines().next()?.trim();
    if let Some(reference) = line.strip_prefix("ref:") {
        let reference = reference.trim();
        let name = reference
            .strip_prefix("refs/heads/")
            .or_else(|| reference.strip_prefix("refs/"))
            .unwrap_or(reference);
        return (!name.is_empty()).then(|| Head::Branch(name.to_owned()));
    }
    let is_commit = line.len() >= 7 && line.bytes().all(|b| b.is_ascii_hexdigit());
    is_commit.then(|| Head::Detached(line[..7].to_owned()))
}

/// The name of the active virtual environment: the last part of `VIRTUAL_ENV`, else
/// `CONDA_DEFAULT_ENV`. An empty value counts as unset.
#[must_use]
pub fn venv(virtual_env: Option<&Path>, conda_env: Option<&str>) -> Option<String> {
    let from_path = virtual_env
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty());
    from_path.or_else(|| conda_env.filter(|name| !name.is_empty()).map(str::to_owned))
}

/// The status of the last command, only when it failed.
#[must_use]
pub fn status(code: u8, unicode: bool) -> Option<String> {
    (code != 0).then(|| format!("{} {code}", if unicode { "✘" } else { "x" }))
}

/// A command that took longer than this has its duration shown (§10).
pub const SHOW_DURATION_OVER: Duration = Duration::from_secs(2);

/// The duration of the last command, only when it was longer than two seconds: `3s`, `1m05s`,
/// `2h03m`. Whole seconds, rounded down.
#[must_use]
pub fn duration(took: Duration) -> Option<String> {
    if took <= SHOW_DURATION_OVER {
        return None;
    }
    let total = took.as_secs();
    let (hours, minutes, seconds) = (total / 3600, total % 3600 / 60, total % 60);
    Some(if hours > 0 {
        format!("{hours}h{minutes:02}m")
    } else if minutes > 0 {
        format!("{minutes}m{seconds:02}s")
    } else {
        format!("{seconds}s")
    })
}

/// `HH:MM`.
#[must_use]
pub fn time(hour: u32, minute: u32) -> String {
    format!("{hour:02}:{minute:02}")
}

/// The current local time, for the `time` segment.
#[must_use]
pub fn now() -> String {
    use chrono::Timelike;
    let now = chrono::Local::now();
    time(now.hour(), now.minute())
}

/// The directory to show: `PWD` if it names a directory, else where the process really is.
#[must_use]
pub fn working_directory(pwd: Option<&[u8]>) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    if let Some(bytes) = pwd.filter(|bytes| !bytes.is_empty()) {
        let path = PathBuf::from(std::ffi::OsStr::from_bytes(bytes));
        if path.is_absolute() {
            return path;
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(path: &str, home: Option<&str>) -> String {
        cwd(Path::new(path), home.map(Path::new), true)
    }

    #[test]
    fn home_is_a_tilde() {
        assert_eq!(shown("/home/me", Some("/home/me")), "~");
        assert_eq!(shown("/home/me/code", Some("/home/me")), "~/code");
    }

    #[test]
    fn a_path_next_to_home_is_not_inside_it() {
        assert_eq!(shown("/home/me2/code", Some("/home/me")), "/home/me2/code");
    }

    #[test]
    fn the_root_and_short_paths_are_shown_whole() {
        assert_eq!(shown("/", Some("/home/me")), "/");
        assert_eq!(shown("/usr/share/doc", None), "/usr/share/doc");
        assert_eq!(shown("/usr", None), "/usr");
    }

    #[test]
    fn long_paths_keep_the_last_three_parts() {
        assert_eq!(shown("/usr/share/doc/foo", None), "…/share/doc/foo");
        assert_eq!(shown("/home/me/a/b/c/d", Some("/home/me")), "…/b/c/d");
        assert_eq!(shown("/home/me/a/b/c", Some("/home/me")), "~/a/b/c");
        assert_eq!(cwd(Path::new("/a/b/c/d"), None, false), ".../b/c/d");
    }

    #[test]
    fn a_root_home_or_empty_home_is_ignored() {
        assert_eq!(shown("/tmp", Some("/")), "/tmp");
        assert_eq!(shown("/tmp", Some("")), "/tmp");
    }

    #[test]
    fn bangla_names_are_kept_whole() {
        assert_eq!(shown("/home/me/বাংলা", Some("/home/me")), "~/বাংলা");
        assert_eq!(shown("/home/me/ক/খ/গ/ঘ", Some("/home/me")), "…/খ/গ/ঘ");
    }

    #[test]
    fn heads() {
        assert_eq!(
            parse_head("ref: refs/heads/main\n"),
            Some(Head::Branch("main".into()))
        );
        assert_eq!(
            parse_head("ref: refs/heads/feature/x-1\n"),
            Some(Head::Branch("feature/x-1".into()))
        );
        assert_eq!(
            parse_head("0123456789abcdef0123456789abcdef01234567\n"),
            Some(Head::Detached("0123456".into()))
        );
        assert_eq!(parse_head(""), None);
        assert_eq!(parse_head("hello\n"), None);
        assert_eq!(parse_head("ref: \n"), None);
        assert_eq!(parse_head("abc\n"), None, "too short for a commit");
    }

    struct Scratch(PathBuf);
    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("omsh-seg-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_branch_is_found_from_a_subdirectory() {
        let s = Scratch::new("branch");
        fs::create_dir_all(s.0.join("repo/.git")).unwrap();
        fs::write(s.0.join("repo/.git/HEAD"), "ref: refs/heads/dev\n").unwrap();
        fs::create_dir_all(s.0.join("repo/a/b")).unwrap();
        assert_eq!(
            git_head(&s.0.join("repo/a/b")),
            Some(Head::Branch("dev".into()))
        );
    }

    #[test]
    fn no_repository_means_no_branch() {
        let s = Scratch::new("none");
        fs::create_dir_all(s.0.join("plain/x")).unwrap();
        // The scratch directory is under /tmp, which is not inside a repository.
        assert_eq!(git_head(&s.0.join("plain/x")), None);
    }

    #[test]
    fn a_worktree_file_points_to_the_real_git_directory() {
        let s = Scratch::new("worktree");
        fs::create_dir_all(s.0.join("main/.git/worktrees/w")).unwrap();
        fs::write(
            s.0.join("main/.git/worktrees/w/HEAD"),
            "0123456789abcdef0123456789abcdef01234567\n",
        )
        .unwrap();
        fs::create_dir_all(s.0.join("w")).unwrap();
        fs::write(
            s.0.join("w/.git"),
            format!("gitdir: {}\n", s.0.join("main/.git/worktrees/w").display()),
        )
        .unwrap();
        assert_eq!(
            git_head(&s.0.join("w")),
            Some(Head::Detached("0123456".into()))
        );
    }

    #[test]
    fn a_relative_gitdir_is_relative_to_the_file() {
        let s = Scratch::new("relative");
        fs::create_dir_all(s.0.join("real")).unwrap();
        fs::write(s.0.join("real/HEAD"), "ref: refs/heads/rel\n").unwrap();
        fs::create_dir_all(s.0.join("sub")).unwrap();
        fs::write(s.0.join("sub/.git"), "gitdir: ../real\n").unwrap();
        assert_eq!(git_head(&s.0.join("sub")), Some(Head::Branch("rel".into())));
    }

    #[test]
    fn a_broken_git_file_gives_no_branch_and_no_panic() {
        let s = Scratch::new("broken");
        fs::create_dir_all(s.0.join("r/.git")).unwrap();
        fs::write(s.0.join("r/.git/HEAD"), [0xff, 0xfe, 0x00]).unwrap();
        assert_eq!(git_head(&s.0.join("r")), None);
        fs::write(s.0.join("r/.git/HEAD"), "x".repeat(10_000)).unwrap();
        assert_eq!(git_head(&s.0.join("r")), None);
    }

    #[test]
    fn a_fifo_named_head_does_not_hang() {
        use nix_free_fifo::make_fifo;
        let s = Scratch::new("fifo");
        fs::create_dir_all(s.0.join("r/.git")).unwrap();
        make_fifo(&s.0.join("r/.git/HEAD"));
        // Refused by the type check before opening; a hang here would fail the test run.
        assert_eq!(git_head(&s.0.join("r")), None);
    }

    /// `mkfifo` through the command, so the crate needs no extra dependency for a test.
    mod nix_free_fifo {
        use std::path::Path;
        use std::process::Command;
        pub fn make_fifo(path: &Path) {
            let status = Command::new("mkfifo").arg(path).status().unwrap();
            assert!(status.success());
        }
    }

    #[test]
    fn venv_names() {
        assert_eq!(
            venv(Some(Path::new("/home/me/proj/.venv")), None),
            Some(".venv".into())
        );
        assert_eq!(venv(None, Some("base")), Some("base".into()));
        assert_eq!(
            venv(Some(Path::new("/x/env1")), Some("base")),
            Some("env1".into())
        );
        assert_eq!(venv(None, Some("")), None);
        assert_eq!(venv(Some(Path::new("")), None), None);
        assert_eq!(venv(None, None), None);
    }

    #[test]
    fn status_only_when_it_failed() {
        assert_eq!(status(0, true), None);
        assert_eq!(status(1, true), Some("✘ 1".into()));
        assert_eq!(status(130, false), Some("x 130".into()));
    }

    #[test]
    fn durations_over_two_seconds() {
        let secs = Duration::from_secs;
        assert_eq!(duration(Duration::from_millis(1999)), None);
        assert_eq!(duration(secs(2)), None);
        assert_eq!(duration(Duration::from_millis(2001)), Some("2s".into()));
        assert_eq!(duration(secs(3)), Some("3s".into()));
        assert_eq!(duration(secs(59)), Some("59s".into()));
        assert_eq!(duration(secs(65)), Some("1m05s".into()));
        assert_eq!(duration(secs(3599)), Some("59m59s".into()));
        assert_eq!(duration(secs(3600 + 3 * 60 + 9)), Some("1h03m".into()));
    }

    #[test]
    fn times() {
        assert_eq!(time(7, 5), "07:05");
        assert_eq!(time(23, 59), "23:59");
        let now = now();
        assert_eq!(now.len(), 5);
        assert_eq!(&now[2..3], ":");
    }

    #[test]
    fn names_map_to_segments() {
        for (name, segment) in [
            ("cwd", Segment::Cwd),
            ("git", Segment::Git),
            ("venv", Segment::Venv),
            ("status", Segment::Status),
            ("duration", Segment::Duration),
            ("time", Segment::Time),
        ] {
            assert_eq!(Segment::from_name(name), Some(segment));
        }
        assert_eq!(Segment::from_name("nope"), None);
    }

    #[test]
    fn pwd_is_used_when_absolute() {
        assert_eq!(working_directory(Some(b"/tmp/x")), PathBuf::from("/tmp/x"));
        // Relative or empty falls back to the real directory.
        let real = std::env::current_dir().unwrap();
        assert_eq!(working_directory(Some(b"rel")), real);
        assert_eq!(working_directory(Some(b"")), real);
        assert_eq!(working_directory(None), real);
    }
}
