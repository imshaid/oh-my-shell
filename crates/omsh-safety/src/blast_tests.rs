//! Tests of the blast radius: what commands act on ([`targets_of`]) and how the walk counts
//! ([`measure`]) with a small fake file system, with huge, slow and stalled fake trees for the
//! limits, and on real trees.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use omsh_syntax::ast::CommandKind;
use omsh_syntax::parse;

use crate::blast::{
    FileTargets, Fs, Kind, Limits, MAX_EXAMPLES, Meta, Names, Op, Radius, StdFs, Stop, Why,
    measure, targets_of,
};
use crate::command::invocation;
use crate::word::Arg;
use crate::{Context, FsProbe, analyze};

const ME: u32 = 1000;

/// How long a call may take in the tests about the time limit. The budget is 0.3 s and the stalled
/// calls sleep 5 s, so a caller that waits for them takes longer than this and one that does not
/// stays far below it, however busy the machine is.
const WAIT_BOUND: Duration = Duration::from_secs(4);
const ROOT: u32 = 0;

fn meta(kind: Kind, size: u64, uid: u32) -> Meta {
    Meta { kind, size, uid }
}

// ---------------------------------------------------------------------------------------
// What a command acts on.

/// The targets of the first command of `line`.
fn targets(line: &str, cwd: Option<&str>, home: Option<&str>) -> Option<FileTargets> {
    let list = parse(line).unwrap();
    let command = &list.items[0].and_or.first.commands[0];
    let CommandKind::Simple(simple) = &command.kind else {
        panic!("not a simple command");
    };
    let words: Vec<Arg<'_>> = simple
        .words
        .iter()
        .map(|word| Arg::from_word(word, line, home))
        .collect();
    let inv = invocation(command.span, words)?;
    targets_of(&inv, cwd, home)
}

fn srcs(found: &FileTargets) -> Vec<&str> {
    found.targets.iter().map(|t| t.src.as_str()).collect()
}

#[test]
fn rm_acts_on_every_operand() {
    let found = targets("rm -rf a 'b c' -- -d", Some("/w"), None).unwrap();
    assert_eq!(found.op, Op::Delete { recursive: true });
    assert_eq!(srcs(&found), ["a", "'b c'", "-d"]);
    assert_eq!(found.cwd.as_deref(), Some("/w"));
    assert_eq!(found.skip, None);
    let plain = targets("rm a", None, None).unwrap();
    assert_eq!(plain.op, Op::Delete { recursive: false });
    let long = targets("rm --recursive a", None, None).unwrap();
    assert_eq!(long.op, Op::Delete { recursive: true });
    assert!(targets("rm", None, None).unwrap().targets.is_empty());
}

#[test]
fn chmod_and_chown_skip_the_mode_or_owner_and_need_a_recursive_flag() {
    let found = targets("chmod -R 755 d e", None, None).unwrap();
    assert_eq!(srcs(&found), ["d", "e"]);
    assert_eq!(
        found.op,
        Op::Permissions {
            follow_command_line: true
        }
    );
    let owner = targets("chown -R me:me d", None, None).unwrap();
    assert_eq!(srcs(&owner), ["d"]);
    assert_eq!(
        owner.op,
        Op::Permissions {
            follow_command_line: false
        }
    );
    let reference = targets("chmod -R --reference=ref d", None, None).unwrap();
    assert_eq!(srcs(&reference), ["d"]);
    assert!(targets("chmod 755 d", None, None).is_none());
    assert!(targets("chown me d", None, None).is_none());
    let h = targets("chown -RH me d", None, None).unwrap();
    assert_eq!(
        h.op,
        Op::Permissions {
            follow_command_line: true
        }
    );
    let l = targets("chgrp -RL g d", None, None).unwrap();
    assert_eq!(l.skip, Some(Why::FollowsLinks));
}

#[test]
fn find_acts_on_its_start_paths_or_the_current_folder() {
    let found = targets("find a b -name x -delete", None, None).unwrap();
    assert_eq!(srcs(&found), ["a", "b"]);
    assert_eq!(found.op, Op::Search { max_depth: None });
    let here = targets("find -name x -delete", None, None).unwrap();
    assert_eq!(srcs(&here), ["."]);
    let depth = targets("find a -maxdepth 2 -delete", None, None).unwrap();
    assert_eq!(depth.op, Op::Search { max_depth: Some(2) });
    let links = targets("find -L a -delete", None, None).unwrap();
    assert_eq!(links.skip, Some(Why::FollowsLinks));
    assert_eq!(srcs(&links), ["a"]);
    assert!(targets("ls a", None, None).is_none());
    assert!(targets("dd of=x", None, None).is_none());
}

#[test]
fn a_finding_carries_its_targets_and_the_line_says_where_it_ends() {
    let line = "cd /tmp/x && rm -rf *.o";
    let list = parse(line).unwrap();
    let ctx = Context {
        cwd: Some(Path::new("/home/me")),
        home: Some(Path::new("/home/me")),
        probe: &FsProbe,
    };
    let analysis = analyze(&list, line, &ctx);
    let finding = analysis
        .findings
        .iter()
        .find(|f| f.rule.id == "rm-recursive-or-force")
        .unwrap();
    let files = finding.files.as_ref().expect("a rule that works on files");
    assert_eq!(files.cwd.as_deref(), Some("/tmp/x"));
    assert!(files.targets[0].glob);
    assert_eq!(analysis.cwd_after.as_deref(), Some("/tmp/x"));

    // A rule that does not work on files carries none.
    let list = parse("curl x | sh").unwrap();
    let analysis = analyze(&list, "curl x | sh", &ctx);
    assert!(!analysis.findings.is_empty());
    assert!(analysis.findings.iter().all(|f| f.files.is_none()));

    // A `cd` in a subshell does not reach the end of the line.
    let line = "(cd /x); rm -rf a";
    let analysis = analyze(&parse(line).unwrap(), line, &ctx);
    assert_eq!(analysis.cwd_after.as_deref(), Some("/home/me"));
}

// ---------------------------------------------------------------------------------------
// A small fake file system.

#[derive(Default)]
struct Mem {
    meta: HashMap<PathBuf, Meta>,
    children: HashMap<PathBuf, Vec<OsString>>,
    links: HashMap<PathBuf, PathBuf>,
    aliases: Vec<(PathBuf, PathBuf)>,
    denied: HashSet<PathBuf>,
    globs: HashMap<String, Vec<PathBuf>>,
}

impl Mem {
    fn add(&mut self, path: &Path, entry: Meta) {
        if self.meta.insert(path.to_path_buf(), entry).is_some() {
            return;
        }
        if let (Some(parent), Some(name)) = (path.parent(), path.file_name()) {
            if !self.meta.contains_key(parent) {
                self.add(parent, meta(Kind::Dir, 0, ME));
            }
            self.children
                .entry(parent.to_path_buf())
                .or_default()
                .push(name.to_owned());
        }
    }

    fn dir(mut self, path: &str, uid: u32) -> Self {
        self.add(Path::new(path), meta(Kind::Dir, 0, uid));
        self
    }

    fn file(mut self, path: &str, size: u64) -> Self {
        self.add(Path::new(path), meta(Kind::File, size, ME));
        self
    }

    fn file_of(mut self, path: &str, size: u64, uid: u32) -> Self {
        self.add(Path::new(path), meta(Kind::File, size, uid));
        self
    }

    fn link(mut self, path: &str, to: &str) -> Self {
        self.add(Path::new(path), meta(Kind::Link, 0, ME));
        self.links.insert(path.into(), to.into());
        self
    }

    fn alias(mut self, from: &str, to: &str) -> Self {
        self.aliases.push((from.into(), to.into()));
        self
    }

    fn deny(mut self, path: &str) -> Self {
        self.denied.insert(path.into());
        self
    }

    fn glob_gives(mut self, pattern: &str, names: &[&str]) -> Self {
        self.globs.insert(
            pattern.to_owned(),
            names.iter().map(PathBuf::from).collect(),
        );
        self
    }

    fn real(&self, path: &Path) -> PathBuf {
        for (from, to) in &self.aliases {
            if let Ok(rest) = path.strip_prefix(from) {
                return to.join(rest);
            }
        }
        // A link used as a folder in the middle of a path leads to what it points to.
        for (link, to) in &self.links {
            if let Ok(rest) = path.strip_prefix(link)
                && !rest.as_os_str().is_empty()
            {
                return to.join(rest);
            }
        }
        path.to_path_buf()
    }
}

fn not_found() -> io::Error {
    io::ErrorKind::NotFound.into()
}

impl Fs for Mem {
    fn lstat(&self, path: &Path) -> io::Result<Meta> {
        self.meta
            .get(&self.real(path))
            .copied()
            .ok_or_else(not_found)
    }

    fn stat(&self, path: &Path) -> io::Result<Meta> {
        let path = self.real(path);
        let path = self.links.get(&path).cloned().unwrap_or(path);
        self.meta.get(&path).copied().ok_or_else(not_found)
    }

    fn read_dir(&self, path: &Path) -> io::Result<Names> {
        let path = self.real(path);
        let path = self.links.get(&path).cloned().unwrap_or(path);
        if self.denied.contains(&path) {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let names = self.children.get(&path).cloned().unwrap_or_default();
        Ok(Box::new(names.into_iter().map(Ok)))
    }

    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        let real = self.real(path);
        Ok(self.links.get(&real).cloned().unwrap_or(real))
    }

    fn glob(&self, pattern: &[u8], _active: &[bool], _base: &Path) -> Option<Vec<PathBuf>> {
        self.globs
            .get(String::from_utf8_lossy(pattern).as_ref())
            .cloned()
    }
}

/// `line` measured on `fs` from `cwd`, with plenty of entries and time.
fn count(fs: Mem, line: &str, cwd: Option<&str>) -> Radius {
    count_with(fs, line, cwd, None, 1_000_000, 600_000)
}

fn count_with(
    fs: impl Fs,
    line: &str,
    cwd: Option<&str>,
    home: Option<&str>,
    entries: u64,
    millis: u64,
) -> Radius {
    let found = targets(line, cwd, home).expect("a command with targets");
    let mut limits = Limits::starting_now(entries, Duration::from_millis(millis));
    let fs: Arc<dyn Fs> = Arc::new(fs);
    measure(&found, &mut limits, ME, &fs)
}

/// The tree most tests use.
fn tree() -> Mem {
    Mem::default()
        .dir("/home/me/work", ME)
        .file("/home/me/work/build/a.o", 100)
        .file("/home/me/work/build/lib/x.o", 50)
        .file("/home/me/work/notes.txt", 7)
        .file_of("/home/me/work/other/secret", 5, ROOT)
        .file("/home/me/other.txt", 3)
        .dir("/etc", ROOT)
        .file_of("/etc/hosts", 20, ROOT)
        .file("/etc/x", 1)
}

const WORK: Option<&str> = Some("/home/me/work");

#[test]
fn a_tree_is_counted_in_full() {
    let radius = count(tree(), "rm -rf build", WORK);
    assert_eq!(radius.files, 2);
    assert_eq!(radius.dirs, 2);
    assert_eq!(radius.others, 0);
    assert_eq!(radius.bytes, 150);
    assert_eq!(radius.outside_cwd, Some(0));
    assert_eq!(radius.not_owned, 0);
    assert_eq!(radius.stopped, None);
    assert!(radius.is_exact());
    assert!(radius.missing.is_empty() && radius.not_counted.is_empty());
    // The target first, then what is directly inside it, before what is deeper.
    let shown: Vec<_> = radius
        .examples
        .iter()
        .map(|p| p.to_str().unwrap())
        .collect();
    assert_eq!(
        shown,
        [
            "/home/me/work/build",
            "/home/me/work/build/a.o",
            "/home/me/work/build/lib",
            "/home/me/work/build/lib/x.o"
        ]
    );
}

#[test]
fn entries_outside_the_working_directory_are_counted() {
    let radius = count(tree(), "rm -rf ../other.txt", WORK);
    assert_eq!((radius.files, radius.outside_cwd), (1, Some(1)));
    let radius = count(tree(), "rm -rf /etc", WORK);
    assert_eq!((radius.entries(), radius.outside_cwd), (3, Some(3)));
    // The working directory itself is not outside it.
    let radius = count(tree(), "rm -rf .", WORK);
    assert_eq!(radius.outside_cwd, Some(0));
    assert_eq!(radius.dirs, 4);
    // Beyond a link in the folder's path is outside, even though the text looks inside.
    let fs = tree().alias("/home/me/work/via", "/etc");
    let radius = count(fs, "rm -rf via/x", WORK);
    assert_eq!((radius.files, radius.outside_cwd), (1, Some(1)));
    // Without a known working directory there is nothing to compare with.
    let radius = count(tree(), "rm -rf /etc", None);
    assert_eq!((radius.entries(), radius.outside_cwd), (3, None));
}

#[test]
fn entries_of_another_user_are_counted() {
    let radius = count(tree(), "rm -rf other", WORK);
    assert_eq!((radius.dirs, radius.files, radius.not_owned), (1, 1, 1));
    let radius = count(tree(), "rm -rf /etc", WORK);
    assert_eq!(radius.not_owned, 2);
}

#[test]
fn rm_without_r_touches_only_what_it_names() {
    let radius = count(tree(), "rm notes.txt", WORK);
    assert_eq!((radius.files, radius.dirs, radius.bytes), (1, 0, 7));
    // `rm` leaves a folder alone without -r, so nothing of it is counted.
    let radius = count(tree(), "rm build", WORK);
    assert_eq!(radius.entries(), 0);
    assert_eq!(
        radius.not_counted,
        [("build".to_owned(), Why::NeedsRecursive)]
    );
    let radius = count(tree(), "rm -f build/a.o nothing.txt", WORK);
    assert_eq!(radius.files, 1);
    assert_eq!(radius.missing, ["nothing.txt"]);
}

#[test]
fn what_cannot_be_counted_is_listed_and_never_turned_into_a_number() {
    let radius = count(tree(), "rm -rf \"$d\"/x", WORK);
    assert_eq!(radius.entries(), 0);
    assert_eq!(radius.not_counted[0].1, Why::OnlyKnownWhenRun);
    let radius = count(tree(), "rm -rf build", None);
    assert_eq!(radius.entries(), 0);
    assert_eq!(radius.not_counted[0].1, Why::UnknownDirectory);
    assert_eq!(radius.outside_cwd, None);
    let radius = count_with(tree(), "rm -rf ~/x", WORK, None, 100, 600_000);
    assert_eq!(radius.not_counted[0].1, Why::UnknownHome);
    // A pattern the file system cannot expand.
    let radius = count(tree(), "rm -rf *.o", WORK);
    assert_eq!(radius.not_counted[0].1, Why::PatternNotExpanded);
}

#[test]
fn patterns_are_expanded_by_the_file_system() {
    let fs = tree().glob_gives("*.o", &["a.o"]);
    let radius = count(fs, "rm -f *.o", Some("/home/me/work/build"));
    assert_eq!((radius.files, radius.bytes), (1, 100));
    // A pattern that matches nothing stays as it is, and there is no such file.
    let fs = tree().glob_gives("*.zz", &[]);
    let radius = count(fs, "rm -f *.zz", WORK);
    assert_eq!(radius.entries(), 0);
    assert_eq!(radius.missing, ["*.zz"]);
}

#[test]
fn a_target_inside_another_is_counted_once() {
    let once = count(tree(), "rm -rf build", WORK).entries();
    assert_eq!(once, 4);
    assert_eq!(count(tree(), "rm -rf build build", WORK).entries(), once);
    assert_eq!(
        count(tree(), "rm -rf build build/lib", WORK).entries(),
        once
    );
    assert_eq!(
        count(tree(), "rm -rf build/lib build", WORK).entries(),
        once
    );
    assert_eq!(
        count(tree(), "rm -rf build ./build/../build", WORK).entries(),
        once
    );
    // Two separate trees add up.
    assert_eq!(
        count(tree(), "rm -rf build other", WORK).entries(),
        once + 2
    );
}

#[test]
fn only_a_few_examples_are_kept() {
    let mut fs = Mem::default().dir("/w", ME);
    for i in 0..20 {
        fs = fs.file(&format!("/w/d/f{i}"), 1);
    }
    let radius = count(fs, "rm -rf d", Some("/w"));
    assert_eq!(radius.entries(), 21);
    assert_eq!(radius.examples.len(), MAX_EXAMPLES);
    assert_eq!(radius.examples[0], Path::new("/w/d"));
}

#[test]
fn links_are_removed_by_rm_but_never_followed() {
    let fs = tree().link("/home/me/work/build/ln", "/etc");
    let radius = count(fs, "rm -rf build", WORK);
    assert_eq!((radius.entries(), radius.others), (5, 1));
    assert_eq!(radius.outside_cwd, Some(0));
}

#[test]
fn chmod_and_chown_follow_links_as_their_options_say() {
    let fs = || {
        tree()
            .link("/home/me/work/build/ln", "/etc")
            .link("/home/me/work/ln", "/etc")
    };
    // A link inside the tree is skipped by chmod.
    let radius = count(fs(), "chmod -R 755 build", WORK);
    assert_eq!((radius.entries(), radius.others), (4, 0));
    // `chmod` follows a link on the command line; `chown` does not unless it is told to.
    let radius = count(fs(), "chmod -R 755 ln", WORK);
    assert_eq!(radius.entries(), 3);
    assert_eq!(radius.outside_cwd, Some(3));
    let radius = count(fs(), "chown -R me ln", WORK);
    assert_eq!(radius.entries(), 0);
    assert_eq!(radius.not_counted, [("ln".to_owned(), Why::Link)]);
    let radius = count(fs(), "chown -RH me ln", WORK);
    assert_eq!(radius.entries(), 3);
    // -L is not modelled, and says so.
    let radius = count(fs(), "chown -RL me build", WORK);
    assert_eq!(radius.entries(), 0);
    assert_eq!(
        radius.not_counted,
        [("build".to_owned(), Why::FollowsLinks)]
    );
}

#[test]
fn find_counts_what_it_searches_down_to_its_depth() {
    let radius = count(tree(), "find build -name '*.o' -delete", WORK);
    assert_eq!(radius.op, Op::Search { max_depth: None });
    assert_eq!(radius.entries(), 4);
    let radius = count(tree(), "find build -maxdepth 1 -delete", WORK);
    // build, a.o and lib; what is inside lib is deeper than the search goes.
    assert_eq!(radius.entries(), 3);
    let radius = count(tree(), "find -delete", Some("/home/me/work/build"));
    assert_eq!(radius.entries(), 4);
    let radius = count(tree(), "find -L build -delete", WORK);
    assert_eq!(
        radius.not_counted,
        [("build".to_owned(), Why::FollowsLinks)]
    );
}

#[test]
fn a_folder_that_cannot_be_read_makes_the_numbers_inexact() {
    let fs = tree().deny("/home/me/work/build/lib");
    let radius = count(fs, "rm -rf build", WORK);
    // build, a.o, lib: x.o is inside the folder that could not be read.
    assert_eq!(radius.entries(), 3);
    assert_eq!(radius.unreadable, 1);
    assert!(!radius.is_exact());
    assert_eq!(radius.stopped, None);
}

// ---------------------------------------------------------------------------------------
// The limits.

/// `/big` holds `dirs` folders with `per_dir` files each, made up when asked for, so a tree
/// of ten million entries costs nothing to have. It can also be slow, or stall for good.
struct Giant {
    dirs: u32,
    per_dir: u32,
    delay: Duration,
    stall_after: Option<u64>,
    calls: AtomicU64,
}

impl Giant {
    fn new(dirs: u32, per_dir: u32) -> Self {
        Self {
            dirs,
            per_dir,
            delay: Duration::ZERO,
            stall_after: None,
            calls: AtomicU64::new(0),
        }
    }

    fn lookup(&self, path: &Path) -> Option<Meta> {
        let rel = path.strip_prefix("/big").ok()?;
        let parts: Vec<_> = rel
            .components()
            .map(|c| c.as_os_str().to_str().unwrap_or(""))
            .collect();
        let number =
            |name: &str, prefix: char| -> Option<u32> { name.strip_prefix(prefix)?.parse().ok() };
        match parts.as_slice() {
            [] => Some(meta(Kind::Dir, 0, ME)),
            [d] if number(d, 'd').is_some_and(|n| n < self.dirs) => Some(meta(Kind::Dir, 0, ME)),
            [d, f]
                if number(d, 'd').is_some_and(|n| n < self.dirs)
                    && number(f, 'f').is_some_and(|n| n < self.per_dir) =>
            {
                Some(meta(Kind::File, 1, ME))
            }
            _ => None,
        }
    }
}

impl Fs for Giant {
    fn lstat(&self, path: &Path) -> io::Result<Meta> {
        let calls = self.calls.fetch_add(1, Ordering::Relaxed) + 1;
        if !self.delay.is_zero() {
            std::thread::sleep(self.delay);
        }
        if self.stall_after.is_some_and(|after| calls > after) {
            std::thread::sleep(Duration::from_secs(5));
        }
        self.lookup(path).ok_or_else(not_found)
    }

    fn stat(&self, path: &Path) -> io::Result<Meta> {
        self.lstat(path)
    }

    fn read_dir(&self, path: &Path) -> io::Result<Names> {
        let rel = path.strip_prefix("/big").map_err(|_| not_found())?;
        let (prefix, count) = if rel.as_os_str().is_empty() {
            ('d', self.dirs)
        } else {
            ('f', self.per_dir)
        };
        Ok(Box::new(
            (0..count).map(move |i| Ok(OsString::from(format!("{prefix}{i}")))),
        ))
    }

    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        Ok(path.to_path_buf())
    }
}

#[test]
fn ten_million_entries_stop_at_the_entry_limit_and_say_so() {
    let radius = count_with(
        Giant::new(100, 100_000),
        "rm -rf /big",
        None,
        None,
        50_000,
        600_000,
    );
    assert_eq!(radius.stopped, Some(Stop::Entries));
    assert_eq!(radius.entries(), 50_000);
    assert!(!radius.is_exact());
    // The folders come before the files inside them.
    assert_eq!(radius.dirs, 101);
}

#[test]
fn the_limit_is_exact_at_the_boundary() {
    // 1 + 10 + 10 * 99 = 1,001 entries.
    let giant = || Giant::new(10, 99);
    let exact = count_with(giant(), "rm -rf /big", None, None, 1_001, 600_000);
    assert_eq!((exact.entries(), exact.stopped), (1_001, None));
    assert!(exact.is_exact());
    let one_less = count_with(giant(), "rm -rf /big", None, None, 1_000, 600_000);
    assert_eq!(
        (one_less.entries(), one_less.stopped),
        (1_000, Some(Stop::Entries))
    );
}

#[test]
fn a_slow_file_system_stops_at_the_time_limit() {
    let mut giant = Giant::new(100, 100_000);
    giant.delay = Duration::from_millis(1);
    let started = Instant::now();
    let radius = count_with(giant, "rm -rf /big", None, None, 1_000_000_000, 300);
    assert_eq!(radius.stopped, Some(Stop::Time));
    assert!(
        started.elapsed() < WAIT_BOUND,
        "took {:?}",
        started.elapsed()
    );
}

#[test]
fn a_stalled_file_system_cannot_hold_the_caller() {
    let mut giant = Giant::new(100, 100_000);
    giant.stall_after = Some(200);
    let started = Instant::now();
    let radius = count_with(giant, "rm -rf /big", None, None, 1_000_000_000, 300);
    assert_eq!(radius.stopped, Some(Stop::Time));
    assert!(
        started.elapsed() < WAIT_BOUND,
        "took {:?}",
        started.elapsed()
    );
    // Whatever was counted before it stalled is kept ("at least"); no more than the 200 calls
    // that completed can have been counted. How many were counted before the deadline depends on
    // when the thread was scheduled, so nothing more is claimed.
    assert!(radius.entries() <= 200);

    let mut giant = Giant::new(100, 100_000);
    giant.stall_after = Some(0);
    let started = Instant::now();
    let radius = count_with(giant, "rm -rf /big", None, None, 1_000_000_000, 300);
    assert_eq!((radius.stopped, radius.entries()), (Some(Stop::Time), 0));
    assert!(started.elapsed() < WAIT_BOUND);
}

#[test]
fn the_limits_are_shared_by_the_commands_of_one_line() {
    let found = targets("rm -rf build", WORK, None).unwrap();
    let fs: Arc<dyn Fs> = Arc::new(tree());
    let mut limits = Limits::starting_now(6, Duration::from_secs(600));
    let first = measure(&found, &mut limits, ME, &fs);
    assert_eq!((first.entries(), first.stopped), (4, None));
    assert_eq!(limits.entries, 2);
    let second = measure(&found, &mut limits, ME, &fs);
    assert_eq!((second.entries(), second.stopped), (2, Some(Stop::Entries)));
    assert_eq!(limits.entries, 0);
    let third = measure(&found, &mut limits, ME, &fs);
    assert_eq!((third.entries(), third.stopped), (0, Some(Stop::Entries)));

    // Time is shared too: once the deadline has passed nothing more is counted.
    let mut late = Limits::starting_now(1_000, Duration::ZERO);
    std::thread::sleep(Duration::from_millis(2));
    let radius = measure(&found, &mut late, ME, &fs);
    assert_eq!((radius.entries(), radius.stopped), (0, Some(Stop::Time)));
}

// ---------------------------------------------------------------------------------------
// Real trees.

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("omsh-blast-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        Self(root)
    }

    fn uid(&self) -> u32 {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(&self.0).unwrap().uid()
    }

    /// `dirs` folders under `tree/`, each with `files` files of `size` bytes.
    fn fill(&self, dirs: u32, files: u32, size: usize) {
        let data = vec![b'x'; size];
        for d in 0..dirs {
            let dir = self.0.join(format!("tree/d{d}"));
            std::fs::create_dir_all(&dir).unwrap();
            for f in 0..files {
                std::fs::write(dir.join(format!("f{f}")), &data).unwrap();
            }
        }
    }

    fn measure(&self, line: &str, entries: u64, millis: u64) -> Radius {
        let cwd = self.0.to_str().unwrap();
        let found = targets(line, Some(cwd), None).unwrap();
        let mut limits = Limits::starting_now(entries, Duration::from_millis(millis));
        let fs: Arc<dyn Fs> = Arc::new(StdFs);
        measure(&found, &mut limits, self.uid(), &fs)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_real_tree_is_counted_exactly() {
    let scratch = Scratch::new();
    scratch.fill(10, 100, 7);
    std::os::unix::fs::symlink("/etc", scratch.0.join("tree/d0/ln")).unwrap();
    let radius = scratch.measure("rm -rf tree", 50_000, 600_000);
    // tree, 10 folders, 1,000 files and one link (which is not followed).
    assert_eq!(radius.dirs, 11);
    assert_eq!(radius.files, 1_000);
    assert_eq!(radius.others, 1);
    assert_eq!(radius.bytes, 7_000);
    assert_eq!(radius.outside_cwd, Some(0));
    assert_eq!(radius.not_owned, 0);
    assert!(radius.is_exact());
}

#[test]
fn a_real_tree_over_the_limit_stops_at_exactly_the_limit() {
    let scratch = Scratch::new();
    scratch.fill(60, 1_000, 0);
    let radius = scratch.measure("rm -rf tree", 50_000, 600_000);
    assert_eq!(radius.stopped, Some(Stop::Entries));
    assert_eq!(radius.entries(), 50_000);
}

#[test]
fn a_real_name_that_does_not_exist_is_reported_as_missing() {
    let scratch = Scratch::new();
    scratch.fill(1, 3, 1);
    let radius = scratch.measure("rm -f tree/d0/f0 tree/d0/nothing", 100, 600_000);
    assert_eq!(radius.files, 1);
    assert_eq!(radius.missing, ["tree/d0/nothing"]);
}
