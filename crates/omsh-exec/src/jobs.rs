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

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use nix::sys::signal::{Signal, killpg};
use nix::sys::wait::{WaitPidFlag, waitpid};
use nix::unistd::Pid;

use crate::io::{FdPlan, Io};
use crate::signals;
use crate::spawn::{self, ChildGroup, ExecRequest, SpawnFailure, Waited};
use crate::status::Termination;
use crate::terminal;

/// What the stop of a job tells the user, until S09 can keep stopped jobs.
const STOPPED_NOTE: &str = "stopped; job control comes in a later version, so it was continued";

/// A process group made of the programs of one pipeline.
#[derive(Debug)]
pub(crate) struct Job {
    /// True if the job owns the terminal while it runs. All jobs do until S09 adds `&`.
    foreground: bool,
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    /// The group, once the first program has started. It is the pid of the leader.
    pgid: Option<Pid>,
    /// A member was killed by a signal or stopped: it may have left the terminal in a
    /// strange state.
    hurt: bool,
    /// The user has been told that the job stopped.
    noted: bool,
}

/// A program of a job that has started.
#[derive(Debug)]
pub(crate) struct Member {
    pid: Pid,
    /// The leader. Its process stays a zombie until the job is dropped.
    anchor: bool,
}

impl Job {
    /// A job that will own the terminal while it runs.
    pub(crate) fn foreground() -> Arc<Self> {
        Arc::new(Self {
            foreground: true,
            state: Mutex::new(State::default()),
        })
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
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
        let leading = state.pgid.is_none();
        let tty = if leading && self.foreground {
            terminal::get().map_or(-1, terminal::Terminal::raw_fd)
        } else {
            -1
        };
        let group = ChildGroup {
            pgid: state.pgid.map_or(0, Pid::as_raw),
            tty,
        };
        let pid = spawn::start(
            &ExecRequest {
                group: Some(group),
                ..*request
            },
            plan,
        )?;
        if leading {
            state.pgid = Some(pid);
        }
        Ok(Member {
            pid,
            anchor: leading,
        })
    }

    /// Waits until the member ends. If it stops (Ctrl+Z) the job is continued at once, with
    /// a note to the user: there are no stopped jobs yet.
    pub(crate) fn wait(&self, member: &Member, name: &str, io: &Io) -> Termination {
        loop {
            match spawn::wait_member(member.pid, member.anchor) {
                Waited::Ended(ending) => {
                    if let Termination::Signaled { signal, .. } = ending {
                        self.state().hurt = true;
                        if signal == nix::libc::SIGINT {
                            // The shell is not in the foreground group, so it never saw the
                            // Ctrl+C itself.
                            signals::note_interrupt();
                        }
                    }
                    return ending;
                }
                Waited::Stopped => {
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
