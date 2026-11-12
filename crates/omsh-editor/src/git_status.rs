//! The git part of the prompt: the branch and a dirty marker, found without ever making the
//! prompt wait for `git` (blueprint §10: "async, timeout 150 ms, shown when ready").
//!
//! One helper thread (`prompt-git`) is started for each prompt. It finds the repository, reads
//! `HEAD` (the branch is a file read, shown at once) and then, if the repository belongs to the
//! current user, runs `git status` to learn whether anything changed. Whatever it learns goes
//! into a shared slot. The editor waits at most [`FIRST_PAINT_WAIT`] for the slot to be full
//! before it draws the prompt; a result that comes later asks the editor to repaint the prompt in
//! place. The whole job has a deadline of [`DEADLINE`]: `git` that is still running then is
//! killed and the marker is left out (the branch alone is shown).
//!
//! The rules for running `git`, all of them the owner's:
//! - `-c core.fsmonitor=false` and no hooks: a repository can name a program in its own
//!   configuration, and the prompt must not start one;
//! - no network: `status` does not use it; `GIT_NO_LAZY_FETCH` and `protocol.allow=never` make
//!   sure of it;
//! - no marker (and `git` is not run) if the repository is not owned by the current user;
//! - `--no-optional-locks`: the prompt never rewrites `.git/index` behind the person's back.

use std::ffi::{OsStr, OsString};
use std::io::{self, Read};
use std::os::fd::AsFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::sys::signal::{Signal, killpg};
use nix::unistd::{Pid, geteuid};

use crate::segments::{Head, Repo, find_repo, head_of};

/// How long the whole job may take, from the moment the prompt is asked for.
pub(crate) const DEADLINE: Duration = Duration::from_millis(150);

/// How long the editor waits for the job before it draws the prompt without the rest.
pub(crate) const FIRST_PAINT_WAIT: Duration = Duration::from_millis(20);

/// How often the job looks at `git`'s pipe and at its cancel flag.
const SLICE: Duration = Duration::from_millis(10);

/// Helper threads that may exist at once. A new prompt cancels the job of the old one, which ends
/// within a slice; more than this means threads that do not end (a file system that does not
/// answer), and no new one is started.
const MAX_ALIVE: usize = 4;

/// The variables `git` is given (when the shell has them). Nothing else of the shell's
/// environment reaches it: not `GIT_DIR`, not `GIT_EXTERNAL_DIFF`, not a token.
const PASSED: [&str; 6] = [
    "PATH",
    "HOME",
    "XDG_CONFIG_HOME",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GIT_CONFIG_NOSYSTEM",
];

/// What the shell's variables say, for the variables in [`PASSED`].
pub(crate) type Env = Vec<(OsString, OsString)>;

/// The names of the variables the prompt asks the shell for.
#[must_use]
pub fn passed_variables() -> &'static [&'static str] {
    &PASSED
}

/// What is known about the branch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum HeadState {
    Pending,
    /// No repository, or a `HEAD` that cannot be read.
    Absent,
    Known(Head),
}

/// What is known about changes in the work tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Dirtiness {
    Pending,
    Clean,
    Dirty,
    /// `git` did not say in time, is not there, failed, or the repository is not the user's.
    Unknown,
}

/// The git part of one prompt, at one moment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Snapshot {
    pub(crate) head: HeadState,
    pub(crate) dirt: Dirtiness,
}

impl Snapshot {
    const fn pending() -> Self {
        Self {
            head: HeadState::Pending,
            dirt: Dirtiness::Pending,
        }
    }

    const fn nothing() -> Self {
        Self {
            head: HeadState::Absent,
            dirt: Dirtiness::Unknown,
        }
    }

    fn settled(&self) -> bool {
        self.head != HeadState::Pending && self.dirt != Dirtiness::Pending
    }

    /// The text of the segment, without style: `(main)` or `(main*)`. Nothing while the branch
    /// is not known. Without a clear answer from `git` there is no marker.
    pub(crate) fn text(&self) -> Option<String> {
        let HeadState::Known(head) = &self.head else {
            return None;
        };
        let mark = if self.dirt == Dirtiness::Dirty {
            "*"
        } else {
            ""
        };
        Some(format!("({}{mark})", head.label()))
    }
}

/// Finds out whether the work tree at `root` has changes. A trait so that a test can hold the
/// answer back.
pub(crate) trait GitRunner: Send + Sync {
    /// Must return by `deadline`, and soon after `cancelled()` becomes true. Never `Pending`.
    fn dirt(
        &self,
        root: &Path,
        env: &[(OsString, OsString)],
        deadline: Instant,
        cancelled: &dyn Fn() -> bool,
    ) -> Dirtiness;
}

#[derive(Debug)]
struct Slot {
    /// The prompt this slot belongs to. A job whose number is old publishes nothing.
    generation: u64,
    snapshot: Snapshot,
    /// The editor has drawn the prompt with what was in the slot at that time, so a change needs
    /// a repaint.
    painted: bool,
    /// When the newest job started.
    last_start: Option<Instant>,
}

#[derive(Debug)]
struct Shared {
    slot: Mutex<Slot>,
    changed: Condvar,
    /// Helper threads that are still running.
    alive: AtomicUsize,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Where the prompt reads the git part from, as it draws.
#[derive(Clone)]
pub(crate) struct Handle {
    source: Source,
}

#[derive(Clone)]
enum Source {
    Live {
        shared: Arc<Shared>,
        generation: u64,
    },
    #[cfg(test)]
    Fixed(Snapshot),
}

impl std::fmt::Debug for Handle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Handle")
            .field("snapshot", &self.snapshot())
            .finish()
    }
}

impl Handle {
    /// A handle that always says the same, for tests of the prompt.
    #[cfg(test)]
    pub(crate) const fn fixed(snapshot: Snapshot) -> Self {
        Self {
            source: Source::Fixed(snapshot),
        }
    }

    /// What is known now. A prompt that is not the newest has nothing to say.
    pub(crate) fn snapshot(&self) -> Snapshot {
        match &self.source {
            Source::Live { shared, generation } => {
                let slot = lock(&shared.slot);
                if slot.generation == *generation {
                    slot.snapshot.clone()
                } else {
                    Snapshot::nothing()
                }
            }
            #[cfg(test)]
            Source::Fixed(snapshot) => snapshot.clone(),
        }
    }
}

/// Starts the job for each prompt and lets the editor wait a little for it.
pub(crate) struct GitWatch {
    shared: Arc<Shared>,
    runner: Arc<dyn GitRunner>,
    repaint: Arc<dyn Fn() + Send + Sync>,
    deadline: Duration,
    /// The user whose repositories `git` may be run in.
    uid: u32,
}

impl GitWatch {
    /// The real thing: `git` is run as a process. `repaint` asks the editor to draw the prompt
    /// again.
    pub(crate) fn new(repaint: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self::with(Arc::new(ProcessRunner), repaint, DEADLINE)
    }

    pub(crate) fn with(
        runner: Arc<dyn GitRunner>,
        repaint: Arc<dyn Fn() + Send + Sync>,
        deadline: Duration,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                slot: Mutex::new(Slot {
                    generation: 0,
                    snapshot: Snapshot::nothing(),
                    painted: false,
                    last_start: None,
                }),
                changed: Condvar::new(),
                alive: AtomicUsize::new(0),
            }),
            runner,
            repaint,
            deadline,
            uid: geteuid().as_raw(),
        }
    }

    /// Pretends to be another user, to test the refusal to run `git` in a repository that is
    /// not the user's.
    #[cfg(test)]
    pub(crate) fn running_as(mut self, uid: u32) -> Self {
        self.uid = uid;
        self
    }

    /// Starts the job for a new prompt in `cwd`. The job of the previous prompt is cancelled.
    pub(crate) fn start(&self, cwd: &Path, env: &[(OsString, OsString)]) -> Handle {
        let started = Instant::now();
        let (generation, spawn) = {
            let mut slot = lock(&self.shared.slot);
            slot.generation += 1;
            slot.painted = false;
            let alive = self.shared.alive.load(Ordering::SeqCst);
            // The newest job is overdue and still there: it is stuck (a file system that does
            // not answer). Another thread would be stuck too.
            let overdue = slot
                .last_start
                .is_some_and(|at| at.elapsed() > self.deadline);
            let refuse = alive >= MAX_ALIVE || (alive > 0 && overdue);
            if refuse {
                slot.snapshot = Snapshot::nothing();
            } else {
                slot.snapshot = Snapshot::pending();
                slot.last_start = Some(started);
            }
            (slot.generation, !refuse)
        };
        if spawn {
            self.shared.alive.fetch_add(1, Ordering::SeqCst);
            let job = Job {
                shared: Arc::clone(&self.shared),
                runner: Arc::clone(&self.runner),
                repaint: Arc::clone(&self.repaint),
                generation,
                cwd: cwd.to_path_buf(),
                env: env.to_vec(),
                deadline: started + self.deadline,
                uid: self.uid,
            };
            let spawned = thread::Builder::new()
                .name("prompt-git".to_owned())
                .spawn(move || job.run());
            if spawned.is_err() {
                self.shared.alive.fetch_sub(1, Ordering::SeqCst);
                let mut slot = lock(&self.shared.slot);
                if slot.generation == generation {
                    slot.snapshot = Snapshot::nothing();
                }
            }
        }
        Handle {
            source: Source::Live {
                shared: Arc::clone(&self.shared),
                generation,
            },
        }
    }

    /// Waits until the job has said everything or `limit` has passed, and notes that the prompt
    /// is about to be drawn with what is known: from now on a change repaints.
    pub(crate) fn wait(&self, handle: &Handle, limit: Duration) {
        let generation = match &handle.source {
            Source::Live { generation, .. } => generation,
            #[cfg(test)]
            Source::Fixed(_) => return,
        };
        let end = Instant::now() + limit;
        let mut slot = lock(&self.shared.slot);
        while slot.generation == *generation && !slot.snapshot.settled() {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            slot = self
                .shared
                .changed
                .wait_timeout(slot, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        if slot.generation == *generation {
            slot.painted = true;
        }
    }
}

struct Job {
    shared: Arc<Shared>,
    runner: Arc<dyn GitRunner>,
    repaint: Arc<dyn Fn() + Send + Sync>,
    generation: u64,
    cwd: PathBuf,
    env: Env,
    deadline: Instant,
    uid: u32,
}

/// Counts a helper thread out when it ends, however it ends.
struct Alive<'a>(&'a AtomicUsize);

impl Drop for Alive<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Job {
    fn run(self) {
        let shared = Arc::clone(&self.shared);
        let _alive = Alive(&shared.alive);
        // A newer prompt has been asked for: nobody wants this job's answer any more.
        let cancelled = || lock(&self.shared.slot).generation != self.generation;

        let Some(repo) = find_repo(&self.cwd) else {
            self.publish(|s| *s = Snapshot::nothing());
            return;
        };
        let Some(head) = head_of(&repo) else {
            self.publish(|s| *s = Snapshot::nothing());
            return;
        };
        // The branch is a file read: show it now, the marker follows.
        self.publish(|s| s.head = HeadState::Known(head));
        if cancelled() {
            return;
        }
        if !owned_by(&repo, self.uid) {
            self.publish(|s| s.dirt = Dirtiness::Unknown);
            return;
        }
        let dirt = self
            .runner
            .dirt(&repo.root, &self.env, self.deadline, &cancelled);
        let dirt = if dirt == Dirtiness::Pending {
            Dirtiness::Unknown
        } else {
            dirt
        };
        self.publish(|s| s.dirt = dirt);
    }

    /// Changes the slot if this is still the newest prompt, wakes the editor if it waits, and
    /// asks for a repaint if the prompt was drawn without this news.
    fn publish(&self, change: impl FnOnce(&mut Snapshot)) {
        let repaint = {
            let mut slot = lock(&self.shared.slot);
            if slot.generation != self.generation {
                return;
            }
            let before = slot.snapshot.clone();
            change(&mut slot.snapshot);
            self.shared.changed.notify_all();
            slot.painted && slot.snapshot != before
        };
        if repaint {
            (self.repaint)();
        }
    }
}

/// True if the git directory and the work tree both belong to user `uid`. `git` itself refuses
/// repositories of other users; the prompt does not even start it for them.
pub(crate) fn owned_by(repo: &Repo, uid: u32) -> bool {
    let mine = |path: &Path| std::fs::metadata(path).is_ok_and(|meta| meta.uid() == uid);
    mine(&repo.git_dir) && mine(&repo.root)
}

/// Runs the real `git`.
struct ProcessRunner;

impl GitRunner for ProcessRunner {
    fn dirt(
        &self,
        root: &Path,
        env: &[(OsString, OsString)],
        deadline: Instant,
        cancelled: &dyn Fn() -> bool,
    ) -> Dirtiness {
        let Some(program) = find_git(env) else {
            return Dirtiness::Unknown;
        };
        let mut command = Command::new(program);
        command.env_clear();
        for (name, value) in env {
            if PASSED.iter().any(|allowed| OsStr::new(allowed) == name) {
                command.env(name, value);
            }
        }
        command
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("LC_ALL", "C")
            .args([
                "--no-optional-locks",
                "-c",
                "core.fsmonitor=false",
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "protocol.allow=never",
                "status",
                "--porcelain=v1",
                "--untracked-files=normal",
                "--ignore-submodules=dirty",
                "--no-renames",
            ])
            .current_dir(root)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .stdout(Stdio::piped())
            // A group of its own: a Ctrl+C at the prompt does not reach it, and it can be
            // killed with whatever it started.
            .process_group(0);
        let Ok(mut child) = command.spawn() else {
            return Dirtiness::Unknown;
        };
        watch(&mut child, deadline, cancelled)
    }
}

/// The first executable regular file called `git` in the `PATH` of `env`.
fn find_git(env: &[(OsString, OsString)]) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let path = env
        .iter()
        .find(|(name, _)| name == "PATH")
        .map_or(b"/usr/local/bin:/usr/bin:/bin".as_slice(), |(_, value)| {
            value.as_bytes()
        });
    path.split(|&b| b == b':')
        .filter(|dir| dir.first() == Some(&b'/'))
        .map(|dir| Path::new(OsStr::from_bytes(dir)).join("git"))
        .find(|candidate| {
            std::fs::metadata(candidate)
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        })
}

/// Reads what `git status` prints until the first byte (changes), the end (clean, if it
/// succeeded), the deadline or a cancel. The child is always ended and collected.
fn watch(child: &mut Child, deadline: Instant, cancelled: &dyn Fn() -> bool) -> Dirtiness {
    let Some(mut out) = child.stdout.take() else {
        kill(child);
        return Dirtiness::Unknown;
    };
    let mut buffer = [0u8; 64];
    loop {
        let now = Instant::now();
        if cancelled() || now >= deadline {
            kill(child);
            return Dirtiness::Unknown;
        }
        let slice = SLICE.min(deadline - now);
        let mut fds = [PollFd::new(out.as_fd(), PollFlags::POLLIN)];
        let timeout = PollTimeout::try_from(slice).unwrap_or(PollTimeout::ZERO);
        match poll(&mut fds, timeout) {
            Ok(0) | Err(Errno::EINTR) => {}
            Ok(_) => match out.read(&mut buffer) {
                // The end of the output, with nothing before it.
                Ok(0) => break,
                Ok(_) => {
                    kill(child);
                    return Dirtiness::Dirty;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => {
                    kill(child);
                    return Dirtiness::Unknown;
                }
            },
            Err(_) => {
                kill(child);
                return Dirtiness::Unknown;
            }
        }
    }
    // Nothing was printed: clean, if `git` agrees that it worked.
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Dirtiness::Clean,
            Ok(Some(_)) => return Dirtiness::Unknown,
            Ok(None) if !cancelled() && Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(1));
            }
            Ok(None) | Err(_) => {
                kill(child);
                return Dirtiness::Unknown;
            }
        }
    }
}

/// Ends `git` and whatever it started, and collects it, so that no zombie is left.
fn kill(child: &mut Child) {
    if let Ok(pid) = i32::try_from(child.id()) {
        let _ = killpg(Pid::from_raw(pid), Signal::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests;
