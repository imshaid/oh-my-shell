//! A limit for a whole test, and cleanup that does not depend on the test behaving.
//!
//! The body of a test runs on its own thread. If it does not finish in time (a bug in the shell
//! that makes it wait for ever, or a mistake in the test), the test fails, and every process of
//! every terminal the body started is killed, which also frees a body that is stuck on the
//! terminal. A body that is stuck elsewhere leaks its thread until the test process ends; the
//! same trade-off as `guarded()` in the unit tests of `omsh-exec`.

use std::any::Any;
use std::fs;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use nix::sys::signal::{Signal, kill};

use crate::procfs;
use crate::session::{Launch, Registry, Session};

/// The limit for a whole test. A single wait inside a test is much shorter
/// ([`crate::SHORT`]).
pub const WHOLE_TEST: Duration = Duration::from_secs(60);

/// What the body of a test uses to start terminals. Every one is known to the watchdog.
pub struct Ctx {
    bin: &'static str,
    sessions: Registry,
    home: PathBuf,
}

impl Ctx {
    /// A launch of the program under test, which the watchdog will clean up after. Its `HOME`
    /// is [`Ctx::home`], so the files the shell writes there belong to this test alone.
    pub fn launch(&self) -> Launch {
        Launch::new(self.bin)
            .registered(Arc::clone(&self.sessions))
            .home(&self.home)
    }

    /// Like [`launch`](Self::launch), for another program than the one under test (a copy of it
    /// that a test installed, S29b). The watchdog cleans up after it in the same way. The
    /// environment starts as for any launch; [`crate::sandbox::Sandbox`] is the way to set the
    /// folders of such a program.
    pub fn launch_program(&self, program: &Path) -> Launch {
        Launch::new(&program.to_string_lossy())
            .registered(Arc::clone(&self.sessions))
            .home(&self.home)
    }

    /// The home directory of this test: empty at the start, removed at the end. Put
    /// `.config/oh-my-shell/config.toml` or `init.omsh` here before starting the shell.
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// The program under test, started with the usual environment, at its first prompt.
    pub fn start(&self) -> io::Result<Session> {
        self.launch().start()
    }

    /// Where the shell keeps its history database (`XDG_DATA_HOME` is `.local/share` under
    /// [`Ctx::home`] for every launch).
    pub fn history_file(&self) -> PathBuf {
        self.home.join(".local/share/oh-my-shell/history.db")
    }

    /// Waits, for at most [`crate::SHORT`], until the shell's history writer has made the
    /// database and switched it to WAL mode. **Call this before opening or reading the history
    /// file from a test while the shell runs**: the shell makes the file on a thread of its own,
    /// after the first prompt (see [`crate::files`]).
    ///
    /// # Panics
    ///
    /// When the file is not ready in time; the message says what is there.
    pub fn wait_for_history_file(&self) {
        crate::files::wait_for_wal_database(&self.history_file(), crate::SHORT);
    }
}

/// Runs the body of a test, with the limit [`WHOLE_TEST`]. `bin` is the program under test:
/// `env!("CARGO_BIN_EXE_omsh-pty-shell")` in an integration test.
///
/// Panics if the body panics, returns an error, runs out of time, or leaves a process behind.
pub fn pty_test(bin: &'static str, body: impl FnOnce(&Ctx) -> io::Result<()> + Send + 'static) {
    pty_test_within(bin, WHOLE_TEST, body);
}

/// [`pty_test`] with a limit of your choice (the harness tests use a short one).
pub fn pty_test_within(
    bin: &'static str,
    limit: Duration,
    body: impl FnOnce(&Ctx) -> io::Result<()> + Send + 'static,
) {
    let sessions: Registry = Arc::new(Mutex::new(Vec::new()));
    let home = HomeDir::new()
        .unwrap_or_else(|error| panic!("cannot make the home directory of the test: {error}"));
    let ctx = Ctx {
        bin,
        sessions: Arc::clone(&sessions),
        home: home.0.clone(),
    };
    let (done, result) = mpsc::channel::<Result<io::Result<()>, Box<dyn Any + Send>>>();
    let thread = thread::spawn(move || {
        let outcome = catch_unwind(AssertUnwindSafe(|| body(&ctx)));
        let _ = done.send(outcome);
    });
    let outcome = result.recv_timeout(limit);
    // The body has dropped its terminals by now, unless it timed out. Whatever is still alive
    // after a short window is a leftover; a body that timed out has live processes by design.
    let window = if outcome.is_err() {
        Duration::ZERO
    } else {
        LEFTOVER_WINDOW
    };
    let left = clean_up(&sessions, window);
    match outcome {
        Ok(Ok(Ok(()))) => {
            let _ = thread.join();
            assert!(left.is_empty(), "processes were left behind: {left:?}");
        }
        Ok(Ok(Err(error))) => panic!("the test returned an error: {error}"),
        Ok(Err(panic)) => resume_unwind(panic),
        Err(_) => panic!("the test did not finish in {limit:?} (its processes were killed)"),
    }
}

/// The home directory of one test, removed when the test function ends (also when it panics,
/// which is after the processes were killed).
struct HomeDir(PathBuf);

impl HomeDir {
    fn new() -> io::Result<Self> {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "omsh-pty-home-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir)?;
        Ok(Self(dir))
    }
}

impl Drop for HomeDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// How long the watchdog gives the processes of a finished test to be gone before it calls any of
/// them a leftover. The harness kills the whole session when a terminal is dropped, but a killed
/// process does not vanish at that instant: it may be in the middle of a system call, or be a
/// zombie that its parent has not collected yet, or be created by a program that was killed while
/// it was forking. Looking at one instant reported such a process as left behind, and CI failed
/// once on a `git` (S19b follow-up). A process that is still alive after this time is real.
pub const LEFTOVER_WINDOW: Duration = Duration::from_secs(1);

/// How often [`leftovers`] looks.
const LEFTOVER_STEP: Duration = Duration::from_millis(10);

/// The processes of `ids` (session ids) that are still alive when `window` has passed, as
/// `name (pid)`. Looks every [`LEFTOVER_STEP`] and returns as soon as none is alive, so a test that
/// cleaned up after itself costs nothing. Zombies do not count (see [`procfs::live_members`]).
/// Nothing is killed here.
pub(crate) fn leftovers(ids: &[u32], window: Duration) -> Vec<String> {
    let end = Instant::now() + window;
    loop {
        let alive: Vec<String> = ids
            .iter()
            .flat_map(|id| procfs::live_members(*id))
            .map(|member| format!("{} ({})", member.name, member.pid))
            .collect();
        if alive.is_empty() || Instant::now() >= end {
            return alive;
        }
        thread::sleep(LEFTOVER_STEP);
    }
}

/// Reports what is still alive in the sessions of `sessions` after `window` (see [`leftovers`]),
/// then kills it and waits until it is gone, plus anything that would not die. The window is
/// zero when the body timed out: its processes are expected to be alive then.
fn clean_up(sessions: &Registry, window: Duration) -> Vec<String> {
    let ids = sessions
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    let mut found = leftovers(&ids, window);
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        let mut alive = Vec::new();
        for id in &ids {
            for member in procfs::live_members(*id) {
                let _ = kill(member.pid, Signal::SIGKILL);
                alive.push(format!("{} ({})", member.name, member.pid));
            }
        }
        if alive.is_empty() {
            return found;
        }
        if Instant::now() >= end {
            found.push(format!("would not die: {alive:?}"));
            return found;
        }
        thread::sleep(Duration::from_millis(20));
    }
}
