//! A limit for a whole test, and cleanup that does not depend on the test behaving.
//!
//! The body of a test runs on its own thread. If it does not finish in time (a bug in the shell
//! that makes it wait for ever, or a mistake in the test), the test fails, and every process of
//! every terminal the body started is killed, which also frees a body that is stuck on the
//! terminal. A body that is stuck elsewhere leaks its thread until the test process ends; the
//! same trade-off as `guarded()` in the unit tests of `omsh-exec`.

use std::any::Any;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
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
}

impl Ctx {
    /// A launch of the program under test, which the watchdog will clean up after.
    pub fn launch(&self) -> Launch {
        Launch::new(self.bin).registered(Arc::clone(&self.sessions))
    }

    /// The program under test, started with the usual environment, at its first prompt.
    pub fn start(&self) -> io::Result<Session> {
        self.launch().start()
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
    let ctx = Ctx {
        bin,
        sessions: Arc::clone(&sessions),
    };
    let (done, result) = mpsc::channel::<Result<io::Result<()>, Box<dyn Any + Send>>>();
    let thread = thread::spawn(move || {
        let outcome = catch_unwind(AssertUnwindSafe(|| body(&ctx)));
        let _ = done.send(outcome);
    });
    let outcome = result.recv_timeout(limit);
    // The body has dropped its terminals by now, unless it timed out.
    let left = clean_up(&sessions);
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

/// Kills every process of every session in `sessions`, waits until they are gone, and returns
/// a description of what was alive when it started (nothing, if the test cleaned up itself),
/// plus anything that would not die.
fn clean_up(sessions: &Registry) -> Vec<String> {
    let ids = sessions
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    let end = Instant::now() + Duration::from_secs(5);
    let mut found = Vec::new();
    let mut first_pass = true;
    loop {
        let mut alive = Vec::new();
        for id in &ids {
            for member in procfs::live_members(*id) {
                let _ = kill(member.pid, Signal::SIGKILL);
                alive.push(format!("{} ({})", member.name, member.pid));
            }
        }
        if first_pass {
            found.clone_from(&alive);
            first_pass = false;
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
