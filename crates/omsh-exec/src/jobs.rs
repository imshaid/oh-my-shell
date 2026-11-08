//! Jobs: the programs of one pipeline (or one command) as one process group.
//!
//! The terminal sends Ctrl+C to a whole process group, so everything that belongs together
//! must be in one. `a | b | c` is one job, and a lone `sleep 5` is one too.
//!
//! The usual way to build a job is to fork first and let each child run shell code. This shell
//! does not (§4.3 and the Decision Log): a pipeline stage is a thread that runs on a copy of the
//! shell and forks only when it reaches a program. A stage such as `{ sleep 1; cat; }` starts
//! its programs late, and several stages start theirs from different threads. So the job is a
//! small shared object (`Arc<Job>`) that every stage holds, and it says which group to join.
//!
//! - The first program started becomes the *leader*: it makes a new group and, for a job in the
//!   foreground, takes the terminal.
//! - Every later program joins that group. Starts are one at a time (a lock held from the
//!   `fork` until the program is running), so two stages cannot both think they lead.
//! - The leader's process is kept as a zombie until the job is over (see
//!   `spawn::wait_member`). A group exists as long as any member does, a zombie included. If the
//!   leader had been reaped when it exited, `true | { sleep 1; cat; }` would find no group to
//!   join, and its pid could even be handed to an unrelated process.
//!
//! A *background* job (`cmd &`) is the same object, with two differences. It never takes the
//! terminal, and it lives in the job table (`jobtable.rs`), so its state is kept here: running,
//! stopped or finished. The thread that runs the job's shell code waits for its programs, as the
//! stages of a pipeline do; nobody calls `waitpid(-1)`, which would steal their children.

use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use nix::errno::Errno;
use nix::sys::signal::{Signal, kill, killpg};
use nix::sys::wait::{WaitPidFlag, waitpid};
use nix::unistd::Pid;

use crate::io::{FdPlan, Io};
use crate::jobtable::Bell;
use crate::signals;
use crate::spawn::{self, ChildGroup, ExecRequest, SpawnFailure, Waited};
use crate::status::{ExitStatus, Termination};
use crate::terminal;

/// What the stop of a foreground job tells the user, until S09b can keep stopped jobs.
const STOPPED_NOTE: &str = "stopped; job control comes in a later version, so it was continued";

/// Where a background job is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    Running,
    /// Stopped by this signal (`SIGTTIN` when it read the terminal, `SIGSTOP`, ...).
    Stopped(i32),
    /// Over, with this status.
    Finished(ExitStatus),
}

/// A process group made of the programs of one pipeline.
#[derive(Debug)]
pub(crate) struct Job {
    /// True if the job owns the terminal while it runs.
    foreground: bool,
    /// True if its programs get a process group of their own. A foreground job always does; a
    /// background job does when the shell has job control.
    grouped: bool,
    /// The signal that told the job to stop (`kill %n`), or 0. A background job whose shell
    /// code is running on a thread of the shell reads this where a foreground job reads the
    /// Ctrl+C flag.
    cancelled: AtomicI32,
    /// Rung when the state changes, for a job in the table.
    bell: Option<Arc<Bell>>,
    state: Mutex<State>,
}

#[derive(Debug)]
struct State {
    /// The group, once the first program has started. It is the pid of the leader.
    pgid: Option<Pid>,
    /// The first program that was started.
    leader: Option<Pid>,
    /// The programs that have started and not ended yet.
    live: Vec<Pid>,
    /// A member was killed by a signal or stopped: it may have left the terminal in a
    /// strange state.
    hurt: bool,
    /// The user has been told that the job stopped.
    noted: bool,
    phase: Phase,
    /// How the last program that ended did so.
    last: Option<Termination>,
}

/// A program of a job that has started.
#[derive(Debug)]
pub(crate) struct Member {
    pid: Pid,
    /// The leader of a group. Its process stays a zombie until the job is dropped.
    anchor: bool,
}

impl Job {
    fn new(foreground: bool, grouped: bool, bell: Option<Arc<Bell>>) -> Arc<Self> {
        Arc::new(Self {
            foreground,
            grouped,
            cancelled: AtomicI32::new(0),
            bell,
            state: Mutex::new(State {
                pgid: None,
                leader: None,
                live: Vec::new(),
                hurt: false,
                noted: false,
                phase: Phase::Running,
                last: None,
            }),
        })
    }

    /// A job that will own the terminal while it runs.
    pub(crate) fn foreground() -> Arc<Self> {
        Self::new(true, true, None)
    }

    /// A job that runs without the terminal. With `grouped` its programs get a process group
    /// of their own. `bell` is rung whenever the job changes.
    pub(crate) fn background(grouped: bool, bell: Arc<Bell>) -> Arc<Self> {
        Self::new(false, grouped, Some(bell))
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn ring(&self) {
        if let Some(bell) = &self.bell {
            bell.ring();
        }
    }

    pub(crate) const fn is_background(&self) -> bool {
        !self.foreground
    }

    pub(crate) const fn is_grouped(&self) -> bool {
        self.grouped
    }

    /// The program that was started first, if any has.
    pub(crate) fn leader(&self) -> Option<Pid> {
        self.state().leader
    }

    pub(crate) fn phase(&self) -> Phase {
        self.state().phase
    }

    pub(crate) fn last_ending(&self) -> Option<Termination> {
        self.state().last
    }

    /// True if `pid` is a program of this job that is still running.
    pub(crate) fn has_live(&self, pid: Pid) -> bool {
        self.state().live.contains(&pid)
    }

    /// The signal a user sent to stop the job, if one was.
    pub(crate) fn cancelled_by(&self) -> Option<i32> {
        match self.cancelled.load(Ordering::SeqCst) {
            0 => None,
            signal => Some(signal),
        }
    }

    /// True if the shell code of the job has been told to stop.
    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancelled_by().is_some()
    }

    /// Marks the job as continued after a stop. A `SIGCONT` sent by the user does not come
    /// back through `wait`, so whoever sends it says so.
    pub(crate) fn mark_continued(&self) {
        let mut state = self.state();
        if matches!(state.phase, Phase::Stopped(_)) {
            state.phase = Phase::Running;
        }
        drop(state);
        self.ring();
    }

    /// The programs of the job that are running.
    #[cfg(test)]
    pub(crate) fn live_pids(&self) -> Vec<Pid> {
        self.state().live.clone()
    }

    /// Sets the phase without any program behind it.
    #[cfg(test)]
    pub(crate) fn state_for_test(&self, phase: Phase) {
        self.state().phase = phase;
        self.ring();
    }

    /// The job's shell code has ended with this status.
    pub(crate) fn finish_background(&self, status: ExitStatus) {
        self.state().phase = Phase::Finished(status);
        self.ring();
    }

    /// Sends `signal` to every program of the job that is running. For `SIGTERM` and `SIGHUP`
    /// to a stopped job, `SIGCONT` follows: a stopped process holds those signals until it is
    /// continued, so it would never act on them. Also tells the shell code of the job to stop
    /// when the signal is one that ends things.
    ///
    /// # Errors
    ///
    /// Returns the error of the system call. A job with nothing running is not an error.
    pub(crate) fn signal(&self, signal: Signal) -> Result<(), Errno> {
        if matches!(
            signal,
            Signal::SIGTERM | Signal::SIGINT | Signal::SIGHUP | Signal::SIGKILL
        ) {
            self.cancelled.store(signal as i32, Ordering::SeqCst);
        }
        let (target, live, stopped) = {
            let state = self.state();
            (
                state.pgid,
                state.live.clone(),
                matches!(state.phase, Phase::Stopped(_)),
            )
        };
        let send = |signal: Signal| -> Result<(), Errno> {
            let result = match (self.grouped, target) {
                (true, Some(group)) => killpg(group, signal),
                (true, None) => Ok(()),
                (false, _) => live.iter().try_for_each(|pid| kill(*pid, signal)),
            };
            // Nothing left to signal is not a failure.
            match result {
                Err(Errno::ESRCH) => Ok(()),
                other => other,
            }
        };
        send(signal)?;
        if signal == Signal::SIGCONT {
            self.mark_continued();
        } else if stopped && matches!(signal, Signal::SIGTERM | Signal::SIGHUP) {
            send(Signal::SIGCONT)?;
            self.mark_continued();
        }
        Ok(())
    }

    /// Starts a program as a member of the job.
    ///
    /// # Errors
    ///
    /// Returns [`SpawnFailure`] if the program could not be started. The job is then as it was
    /// before: the next program becomes the leader if there was none.
    pub(crate) fn start(
        &self,
        request: &ExecRequest<'_>,
        plan: &mut FdPlan,
    ) -> Result<Member, SpawnFailure> {
        // Held until the program runs, so that members start one at a time.
        let mut state = self.state();
        let leading = self.grouped && state.pgid.is_none();
        let group = self.grouped.then(|| {
            let tty = if leading && self.foreground {
                terminal::get().map_or(-1, terminal::Terminal::raw_fd)
            } else {
                -1
            };
            ChildGroup {
                pgid: state.pgid.map_or(0, Pid::as_raw),
                tty,
            }
        });
        let pid = spawn::start(&ExecRequest { group, ..*request }, plan)?;
        if leading {
            state.pgid = Some(pid);
        }
        state.leader.get_or_insert(pid);
        state.live.push(pid);
        drop(state);
        self.ring();
        Ok(Member {
            pid,
            anchor: leading,
        })
    }

    /// Waits until the member ends. If a foreground job stops (Ctrl+Z) it is continued at once,
    /// with a note to the user: there are no stopped foreground jobs yet. A background job that
    /// stops is recorded as stopped and waited for again.
    pub(crate) fn wait(&self, member: &Member, name: &str, io: &Io) -> Termination {
        loop {
            match spawn::wait_member(member.pid, member.anchor) {
                Waited::Ended(ending) => {
                    {
                        let mut state = self.state();
                        state.live.retain(|pid| *pid != member.pid);
                        state.last = Some(ending);
                        if matches!(ending, Termination::Signaled { .. }) {
                            state.hurt = true;
                        }
                    }
                    self.ring();
                    if let Termination::Signaled { signal, .. } = ending
                        && signal == nix::libc::SIGINT
                        && self.foreground
                    {
                        // The shell is not in the foreground group, so it never saw the
                        // Ctrl+C itself. (A background job killed by `kill -INT` is none of
                        // the foreground's business.)
                        signals::note_interrupt();
                    }
                    return ending;
                }
                Waited::Stopped(signal) => {
                    if !self.foreground {
                        self.state().phase = Phase::Stopped(signal);
                        self.ring();
                        continue;
                    }
                    let (first, pgid) = {
                        let mut state = self.state();
                        state.hurt = true;
                        (!std::mem::replace(&mut state.noted, true), state.pgid)
                    };
                    if first {
                        io.say(format!("oh-my-shell: {name}: {STOPPED_NOTE}"));
                    }
                    if let Some(pgid) = pgid {
                        let _ = killpg(pgid, Signal::SIGCONT);
                    }
                }
            }
        }
    }

    /// The job is over: the shell takes the terminal back. Call it once every member has been
    /// waited for.
    pub(crate) fn finish(&self) {
        if !self.foreground {
            return;
        }
        let hurt = self.state().hurt;
        if let Some(terminal) = terminal::get() {
            terminal.take_back(hurt);
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // The leader has ended by now (it was waited for with `WNOWAIT`); this removes the
        // zombie. If it is somehow still alive, `WNOHANG` leaves it alone.
        let state = self.state.get_mut().unwrap_or_else(PoisonError::into_inner);
        if let Some(leader) = state.pgid {
            let _ = waitpid(leader, Some(WaitPidFlag::WNOHANG));
        }
    }
}
