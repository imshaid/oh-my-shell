//! Blast radius (§12.3): what a file command would touch, counted before it runs.
//!
//! Two steps. [`targets_of`] reads a command and says which paths it acts on and how (this
//! needs only the syntax). [`measure`] then looks at the real file system, under limits: at most
//! so many entries and so much time, whichever comes first. When a limit stops the walk the
//! result says so ([`Radius::stopped`]) and every number is "at least". Nothing is guessed:
//! words only known at run time, patterns that cannot be expanded and missing files are listed
//! as *not counted*, not turned into numbers.
//!
//! The walk runs on a helper thread that owns everything it needs. If the file system stalls
//! (a dead network mount), the caller still returns at the deadline with what was counted so
//! far; only the helper thread is left behind.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::command::Invocation;
use crate::paths;
use crate::word::Arg;

/// How many example paths are kept.
pub const MAX_EXAMPLES: usize = 5;

/// How long the caller waits past the walk's own deadline for the helper thread to report.
const GRACE: Duration = Duration::from_millis(25);

/// How often (in entries) the helper thread looks at the clock.
const CHECK_EVERY: u64 = 64;

/// How often (in entries) the helper thread shares its counts with the caller.
const PUBLISH_EVERY: u64 = 16;

/// What the command does to the targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    /// Removes them (`rm`). Without `-r` only the targets themselves are removed.
    Delete {
        /// `-r`, `-R` or `--recursive`.
        recursive: bool,
    },
    /// Changes the owner or permissions of everything under them (`chmod -R`, `chown -R`).
    Permissions {
        /// True if a link on the command line is followed (`chmod`; `chown -H`).
        follow_command_line: bool,
    },
    /// Looks through them (`find`). It removes only what its tests match, so the number is
    /// how much it searches, never how much it deletes.
    Search {
        /// A `-maxdepth` that is the same on every run.
        max_depth: Option<u32>,
    },
}

/// Why a word could not be counted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    /// Part of it is only known when the command runs.
    OnlyKnownWhenRun,
    /// A relative path, and the working directory at that point is not known.
    UnknownDirectory,
    /// `~` of a home folder that is not known.
    UnknownHome,
    /// A pattern that cannot be expanded here.
    PatternNotExpanded,
    /// The command follows symbolic links in a way that is not modelled.
    FollowsLinks,
    /// A folder given to `rm` without `-r`: `rm` refuses it.
    NeedsRecursive,
    /// A symbolic link given to a command that may act on the link or on what it points to.
    Link,
    /// Its information could not be read.
    Unreadable,
}

impl Why {
    /// The reason in plain words.
    #[must_use]
    pub const fn text(self) -> &'static str {
        match self {
            Self::OnlyKnownWhenRun => "only known when the command runs",
            Self::UnknownDirectory => "the working directory is not known at that point",
            Self::UnknownHome => "the home folder is not known",
            Self::PatternNotExpanded => "the pattern could not be expanded",
            Self::FollowsLinks => "the command follows symbolic links, which is not counted",
            Self::NeedsRecursive => "a folder, which rm leaves alone without -r",
            Self::Link => "a symbolic link; what happens to it depends on the options",
            Self::Unreadable => "it could not be read",
        }
    }
}

/// Why a walk stopped before the end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// The entry limit was reached.
    Entries,
    /// The time limit was reached.
    Time,
}

/// One path word of a command.
#[derive(Clone, Debug)]
pub struct Target {
    /// The word as written.
    pub src: String,
    /// The text known before the command runs.
    pub text: String,
    /// See [`Arg::active`].
    pub active: Vec<bool>,
    /// True if it has an unquoted pattern character.
    pub glob: bool,
    /// True if some part is only known at run time.
    pub unknown: bool,
}

impl Target {
    fn new(arg: &Arg<'_>) -> Self {
        Self {
            src: arg.src.clone(),
            text: arg.text.clone(),
            active: arg.active.clone(),
            glob: arg.glob,
            unknown: arg.unknown,
        }
    }

    fn here() -> Self {
        Self {
            src: ".".to_owned(),
            text: ".".to_owned(),
            active: vec![false],
            glob: false,
            unknown: false,
        }
    }
}

/// What a command acts on: read from the syntax alone.
#[derive(Clone, Debug)]
pub struct FileTargets {
    /// What it does.
    pub op: Op,
    /// The working directory where it runs, if known.
    pub cwd: Option<String>,
    /// What `~` means there, if known.
    pub home: Option<String>,
    /// The path words.
    pub targets: Vec<Target>,
    /// Set if the whole command is of a kind that is not counted.
    pub skip: Option<Why>,
}

/// The paths `inv` acts on, if it is a command the blast radius knows: `rm`, recursive
/// `chmod`/`chown`/`chgrp`, and `find`.
#[must_use]
pub fn targets_of(
    inv: &Invocation<'_>,
    cwd: Option<&str>,
    home: Option<&str>,
) -> Option<FileTargets> {
    let options = inv.options();
    let make = |op, targets: Vec<Target>, skip| FileTargets {
        op,
        cwd: cwd.map(str::to_owned),
        home: home.map(str::to_owned),
        targets,
        skip,
    };
    match inv.name.as_str() {
        "rm" => {
            let recursive =
                options.has_short('r') || options.has_short('R') || options.has_long("recursive");
            let targets = options
                .operands
                .iter()
                .map(|arg| Target::new(arg))
                .collect();
            Some(make(Op::Delete { recursive }, targets, None))
        }
        "chmod" | "chown" | "chgrp" => {
            if !(options.has_short('R') || options.has_long("recursive")) {
                return None;
            }
            // The first operand is the mode or the owner, unless `--reference` gives it.
            let skip = usize::from(!options.has_long("reference"));
            let targets = options
                .operands
                .iter()
                .skip(skip)
                .map(|arg| Target::new(arg))
                .collect();
            let chmod = inv.name == "chmod";
            let follow = chmod || options.has_short('H');
            let why = (!chmod && options.has_short('L')).then_some(Why::FollowsLinks);
            Some(make(
                Op::Permissions {
                    follow_command_line: follow,
                },
                targets,
                why,
            ))
        }
        "find" => {
            let find = crate::find::parse(inv);
            let mut targets: Vec<Target> = find.starts.iter().map(|arg| Target::new(arg)).collect();
            if targets.is_empty() {
                targets.push(Target::here());
            }
            let why = find.follows_links.then_some(Why::FollowsLinks);
            Some(make(
                Op::Search {
                    max_depth: find.max_depth,
                },
                targets,
                why,
            ))
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------
// The file system, as the walk sees it.

/// What kind of thing an entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A regular file.
    File,
    /// A folder.
    Dir,
    /// A symbolic link.
    Link,
    /// A device, socket, pipe.
    Other,
}

/// The facts about one entry that the walk uses.
#[derive(Clone, Copy, Debug)]
pub struct Meta {
    /// What it is.
    pub kind: Kind,
    /// Its size in bytes (apparent size).
    pub size: u64,
    /// Its owner.
    pub uid: u32,
}

/// The names in a folder.
pub type Names = Box<dyn Iterator<Item = io::Result<OsString>> + Send>;

/// The file system. The real one is [`StdFs`]; tests use a fake with huge or stalled trees.
pub trait Fs: Send + Sync + 'static {
    /// Information about `path`, not following a final link.
    fn lstat(&self, path: &Path) -> io::Result<Meta>;
    /// Information about `path`, following links.
    fn stat(&self, path: &Path) -> io::Result<Meta>;
    /// The names in a folder.
    fn read_dir(&self, path: &Path) -> io::Result<Names>;
    /// `path` with links resolved.
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf>;
    /// The names that a pattern matches (see [`crate::Probe::glob`]). `None` if this file
    /// system cannot expand patterns.
    fn glob(&self, _pattern: &[u8], _active: &[bool], _base: &Path) -> Option<Vec<PathBuf>> {
        None
    }
}

/// The real file system, through `std`. It cannot expand patterns.
#[derive(Clone, Copy, Debug, Default)]
pub struct StdFs;

impl From<std::fs::Metadata> for Meta {
    fn from(meta: std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        let kind = if meta.is_dir() {
            Kind::Dir
        } else if meta.is_file() {
            Kind::File
        } else if meta.file_type().is_symlink() {
            Kind::Link
        } else {
            Kind::Other
        };
        Self {
            kind,
            size: meta.len(),
            uid: meta.uid(),
        }
    }
}

impl Fs for StdFs {
    fn lstat(&self, path: &Path) -> io::Result<Meta> {
        std::fs::symlink_metadata(path).map(Meta::from)
    }

    fn stat(&self, path: &Path) -> io::Result<Meta> {
        std::fs::metadata(path).map(Meta::from)
    }

    fn read_dir(&self, path: &Path) -> io::Result<Names> {
        let entries = std::fs::read_dir(path)?;
        Ok(Box::new(entries.map(|entry| entry.map(|e| e.file_name()))))
    }

    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        std::fs::canonicalize(path)
    }
}

// ---------------------------------------------------------------------------------------
// The result.

/// What a command would touch, as far as the walk got.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Radius {
    /// What the command does.
    pub op: Op,
    /// Regular files.
    pub files: u64,
    /// Folders.
    pub dirs: u64,
    /// Links, devices and the like.
    pub others: u64,
    /// The size of the regular files together.
    pub bytes: u64,
    /// How many entries are outside the working directory; `None` if it is not known.
    pub outside_cwd: Option<u64>,
    /// How many entries belong to another user.
    pub not_owned: u64,
    /// The first few entries (the targets first, then what is directly inside).
    pub examples: Vec<PathBuf>,
    /// Set if a limit ended the walk early: every number is then "at least".
    pub stopped: Option<Stop>,
    /// Folders or entries that could not be read: what is in them is not counted.
    pub unreadable: u64,
    /// Words that name nothing that exists.
    pub missing: Vec<String>,
    /// Words that were left out, with the reason.
    pub not_counted: Vec<(String, Why)>,
}

impl Radius {
    fn new(op: Op, cwd_known: bool) -> Self {
        Self {
            op,
            files: 0,
            dirs: 0,
            others: 0,
            bytes: 0,
            outside_cwd: cwd_known.then_some(0),
            not_owned: 0,
            examples: Vec::new(),
            stopped: None,
            unreadable: 0,
            missing: Vec::new(),
            not_counted: Vec::new(),
        }
    }

    /// Every entry counted.
    #[must_use]
    pub const fn entries(&self) -> u64 {
        self.files + self.dirs + self.others
    }

    /// True if the numbers are exact: the walk reached the end and read every folder.
    #[must_use]
    pub const fn is_exact(&self) -> bool {
        self.stopped.is_none() && self.unreadable == 0
    }
}

/// What is left of the limits. One value covers a whole line, so that a line of many commands
/// cannot take many times the time.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Entries that may still be counted.
    pub entries: u64,
    /// When the walk must have stopped.
    pub deadline: Instant,
}

impl Limits {
    /// Limits that start now.
    #[must_use]
    pub fn starting_now(max_entries: u64, max_time: Duration) -> Self {
        Self {
            entries: max_entries,
            deadline: Instant::now() + max_time,
        }
    }

    fn used_up(&self) -> Option<Stop> {
        if self.entries == 0 {
            Some(Stop::Entries)
        } else if Instant::now() >= self.deadline {
            Some(Stop::Time)
        } else {
            None
        }
    }
}

/// Counts what `targets` would touch, within `limits` (which are reduced by what was used).
/// `uid` is the user whose files count as "owned".
pub fn measure(targets: &FileTargets, limits: &mut Limits, uid: u32, fs: &Arc<dyn Fs>) -> Radius {
    let mut empty = Radius::new(targets.op, targets.cwd.is_some());
    if let Some(stop) = limits.used_up() {
        empty.stopped = Some(stop);
        return empty;
    }
    let shared = Arc::new(Mutex::new(empty.clone()));
    let cancelled = Arc::new(AtomicBool::new(false));
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let work = Walk {
        targets: targets.clone(),
        limits: *limits,
        uid,
        fs: Arc::clone(fs),
        shared: Arc::clone(&shared),
        cancelled: Arc::clone(&cancelled),
    };
    let spawned = std::thread::Builder::new()
        .name("omsh-blast".to_owned())
        .spawn(move || {
            work.run();
            let _ = done_tx.send(());
        });
    let wait = limits.deadline.saturating_duration_since(Instant::now()) + GRACE;
    let stalled = spawned.is_err() || done_rx.recv_timeout(wait).is_err();
    cancelled.store(true, Ordering::Relaxed);
    let mut radius = shared.lock().map_or_else(
        |poisoned| poisoned.into_inner().clone(),
        |guard| guard.clone(),
    );
    if stalled && radius.stopped.is_none() {
        radius.stopped = Some(Stop::Time);
    }
    limits.entries = limits.entries.saturating_sub(radius.entries());
    radius
}

// ---------------------------------------------------------------------------------------
// The walk.

struct Walk {
    targets: FileTargets,
    limits: Limits,
    uid: u32,
    fs: Arc<dyn Fs>,
    shared: Arc<Mutex<Radius>>,
    cancelled: Arc<AtomicBool>,
}

/// One entry waiting to be looked at.
struct Item {
    path: PathBuf,
    depth: u32,
    outside: bool,
    /// The information already read (for a target); a name found in a folder is looked up
    /// when its turn comes.
    meta: Option<Meta>,
}

/// A target that was found and checked.
struct Root {
    path: PathBuf,
    outside: bool,
    meta: Meta,
}

impl Walk {
    fn publish(&self, radius: &Radius) {
        if let Ok(mut guard) = self.shared.lock() {
            *guard = radius.clone();
        }
    }

    fn run(self) {
        let mut radius = Radius::new(self.targets.op, self.targets.cwd.is_some());
        let roots = self.roots(&mut radius);
        self.publish(&radius);
        self.walk(roots, &mut radius);
        self.publish(&radius);
    }

    /// Resolves each word to the entries it names.
    fn roots(&self, radius: &mut Radius) -> Vec<Root> {
        let cwd = self.targets.cwd.as_deref();
        let cwd_real = cwd.map(|cwd| {
            self.fs
                .canonicalize(Path::new(cwd))
                .unwrap_or_else(|_| PathBuf::from(cwd))
        });
        let mut found: Vec<Root> = Vec::new();
        for target in &self.targets.targets {
            if let Some(why) = self.targets.skip {
                radius.not_counted.push((target.src.clone(), why));
                continue;
            }
            if target.unknown {
                radius
                    .not_counted
                    .push((target.src.clone(), Why::OnlyKnownWhenRun));
                continue;
            }
            if target.text.starts_with('~') && (target.text == "~" || target.text.starts_with("~/"))
            {
                radius
                    .not_counted
                    .push((target.src.clone(), Why::UnknownHome));
                continue;
            }
            let base: Option<String> = if target.text.starts_with('/') {
                Some("/".to_owned())
            } else {
                cwd.map(str::to_owned)
            };
            let Some(base) = base else {
                radius
                    .not_counted
                    .push((target.src.clone(), Why::UnknownDirectory));
                continue;
            };
            let paths: Vec<PathBuf> = if target.glob {
                match self
                    .fs
                    .glob(target.text.as_bytes(), &target.active, Path::new(&base))
                {
                    Some(list) => list
                        .into_iter()
                        .map(|path| PathBuf::from(paths::resolve(&base, &path.to_string_lossy())))
                        .collect(),
                    None => {
                        radius
                            .not_counted
                            .push((target.src.clone(), Why::PatternNotExpanded));
                        continue;
                    }
                }
            } else {
                vec![PathBuf::from(paths::resolve(&base, &target.text))]
            };
            if paths.is_empty() {
                radius.missing.push(target.src.clone());
                continue;
            }
            for path in paths {
                if self.cancelled.load(Ordering::Relaxed) {
                    return found;
                }
                self.root(target, path, cwd_real.as_deref(), radius, &mut found);
            }
        }
        // A target inside another one adds nothing (`rm -r a a/b`); the same path twice, too.
        found.sort_by(|a, b| a.path.cmp(&b.path));
        found.dedup_by(|b, a| b.path.starts_with(&a.path));
        found
    }

    fn root(
        &self,
        target: &Target,
        path: PathBuf,
        cwd_real: Option<&Path>,
        radius: &mut Radius,
        found: &mut Vec<Root>,
    ) {
        let follow = matches!(
            self.targets.op,
            Op::Permissions {
                follow_command_line: true
            }
        );
        let meta = match self.fs.lstat(&path) {
            Ok(meta) => meta,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                radius.missing.push(target.src.clone());
                return;
            }
            Err(_) => {
                radius
                    .not_counted
                    .push((target.src.clone(), Why::Unreadable));
                return;
            }
        };
        let followed = meta.kind == Kind::Link && matches!(self.targets.op, Op::Permissions { .. });
        let meta = if meta.kind == Kind::Link {
            match self.targets.op {
                Op::Permissions { .. } if !follow => {
                    radius.not_counted.push((target.src.clone(), Why::Link));
                    return;
                }
                Op::Permissions { .. } => match self.fs.stat(&path) {
                    Ok(meta) => meta,
                    Err(_) => {
                        radius.missing.push(target.src.clone());
                        return;
                    }
                },
                _ => meta,
            }
        } else {
            meta
        };
        if matches!(self.targets.op, Op::Delete { recursive: false }) && meta.kind == Kind::Dir {
            radius
                .not_counted
                .push((target.src.clone(), Why::NeedsRecursive));
            return;
        }
        let outside = cwd_real.map(|cwd| {
            if followed {
                // What the command changes is what the link points to, not the link.
                self.fs
                    .canonicalize(&path)
                    .map_or(true, |real| !real.starts_with(cwd))
            } else {
                !self.inside(&path, cwd)
            }
        });
        found.push(Root {
            path,
            outside: outside.unwrap_or(false),
            meta,
        });
    }

    /// True if `path`, with the links in its folder resolved, is the working directory or
    /// below it.
    fn inside(&self, path: &Path, cwd_real: &Path) -> bool {
        let real = match (path.parent(), path.file_name()) {
            (Some(parent), Some(name)) => self
                .fs
                .canonicalize(parent)
                .map_or_else(|_| path.to_path_buf(), |parent| parent.join(name)),
            _ => path.to_path_buf(),
        };
        real.starts_with(cwd_real)
    }

    fn walk(&self, roots: Vec<Root>, radius: &mut Radius) {
        let recurse = !matches!(self.targets.op, Op::Delete { recursive: false });
        let max_depth = match self.targets.op {
            Op::Search { max_depth } => max_depth,
            _ => None,
        };
        let skip_links = matches!(self.targets.op, Op::Permissions { .. });
        let mut queue: VecDeque<Item> = roots
            .into_iter()
            .map(|root| Item {
                path: root.path,
                depth: 0,
                outside: root.outside,
                meta: Some(root.meta),
            })
            .collect();
        let mut popped: u64 = 0;
        let mut counted: u64 = 0;
        let mut since_publish: u64 = 0;
        while let Some(item) = queue.pop_front() {
            if popped.is_multiple_of(CHECK_EVERY) {
                if self.cancelled.load(Ordering::Relaxed) {
                    return;
                }
                if Instant::now() >= self.limits.deadline {
                    radius.stopped = Some(Stop::Time);
                    return;
                }
            }
            popped += 1;
            if counted >= self.limits.entries {
                radius.stopped = Some(Stop::Entries);
                return;
            }
            // Names found in a folder are looked at only now, so that a wide tree is counted
            // from the start and not read to the end before the first number.
            let meta = match item.meta {
                Some(meta) => meta,
                None => match self.fs.lstat(&item.path) {
                    Ok(meta) => meta,
                    // Gone since the folder was read: nothing to count.
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    Err(_) => {
                        radius.unreadable += 1;
                        continue;
                    }
                },
            };
            if item.depth > 0 && skip_links && meta.kind == Kind::Link {
                continue;
            }
            counted += 1;
            since_publish += 1;
            self.count(&item, meta, radius);
            if since_publish >= PUBLISH_EVERY {
                self.publish(radius);
                since_publish = 0;
            }
            if meta.kind != Kind::Dir || !recurse {
                continue;
            }
            if max_depth.is_some_and(|max| item.depth >= max) {
                continue;
            }
            let names = match self.fs.read_dir(&item.path) {
                Ok(names) => names,
                Err(_) => {
                    radius.unreadable += 1;
                    continue;
                }
            };
            for name in names {
                let Ok(name) = name else {
                    radius.unreadable += 1;
                    continue;
                };
                queue.push_back(Item {
                    path: item.path.join(name),
                    depth: item.depth + 1,
                    outside: item.outside,
                    meta: None,
                });
                // A huge folder must not hold the walk past its limits. More names waiting
                // than entries left means the entry limit will be reached anyway.
                if queue.len() as u64 > self.limits.entries.saturating_add(CHECK_EVERY)
                    || self.cancelled.load(Ordering::Relaxed)
                {
                    break;
                }
            }
        }
    }

    fn count(&self, item: &Item, meta: Meta, radius: &mut Radius) {
        match meta.kind {
            Kind::File => {
                radius.files += 1;
                radius.bytes = radius.bytes.saturating_add(meta.size);
            }
            Kind::Dir => radius.dirs += 1,
            Kind::Link | Kind::Other => radius.others += 1,
        }
        if item.outside
            && let Some(outside) = &mut radius.outside_cwd
        {
            *outside += 1;
        }
        if meta.uid != self.uid {
            radius.not_owned += 1;
        }
        if radius.examples.len() < MAX_EXAMPLES {
            radius.examples.push(item.path.clone());
        }
    }
}
