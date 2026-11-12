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
//! # State, stops and who waits
//!
//! The job keeps its own state: *running*, *stopped* or *finished*. It is stopped when it has
//! programs, every one of them is stopped (the bash rule: `a | b` with only `a` stopped is
//! still running) and every task has *settled*. A task settles when it starts a program, runs a
//! command that is not a program (a builtin or a function: shell code pauses by itself while
//! the job is stopped) or finishes. Before that it may be about to start a program, so
//! `sh -c 'kill -STOP $$' | cat` is not stopped in the moment before `cat` is forked.
//!
//! The record of a stop can go stale: a program continued from outside (`kill -CONT` from
//! another shell) is not reported by the waits, which look only for an end or a stop. So before
//! the job counts as stopped, each recorded stop is checked with the kernel
//! ([`spawn::still_stopped`], a look that takes nothing). Otherwise a pipeline whose first
//! program was continued, and whose other stages have already ended, would be given up as
//! stopped while its first program has not yet been seen to exit.
//!
//! It is finished when every *task* is over. A task is one thread that runs shell code for the
//! job: the stages of a pipeline, or the thread of a background job. Each holds a [`Task`] and
//! reports its status with [`Task::done`]; a task that is dropped without it (a panic) counts
//! as 134, so a job cannot stay running forever.
//!
//! Nobody calls `waitpid(-1)`, which would steal the children of other threads. The thread
//! that started a program waits for exactly that pid ([`Job::wait_or_stop`]) and writes what it
//! saw into the job. A stop is not an ending: the waiter records it and the job's state changes.
//! The main shell, which has the prompt to go back to, is the only one that gives up on a
//! stopped foreground job (`exec/stopped.rs`); it hands the waiting to a thread, so a later
//! `fg` or `bg` finds the job's threads still waiting.
//!
//! A job is *foreground* while it owns the terminal. `Ctrl+Z` makes it a background job (in the
//! job table, stopped); `fg` makes it foreground again. A job also remembers the terminal
//! settings (termios) it had when it was stopped, and gets them back with `fg`.

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use nix::errno::Errno;
use nix::sys::signal::{Signal, kill, killpg};
use nix::sys::termios::Termios;
use nix::sys::wait::{WaitPidFlag, waitpid};
use nix::unistd::Pid;

use crate::io::FdPlan;
use crate::jobtable::Bell;
use crate::signals;
use crate::spawn::{self, ChildGroup, ExecRequest, SpawnFailure, Waited};
use crate::status::{ExitStatus, Termination};
use crate::terminal;

/// How often shell code that waits for a stopped job looks at whether it was told to stop.
const PAUSE_STEP: Duration = Duration::from_millis(100);

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
    /// True while the job owns the terminal. Ctrl+Z clears it, `fg` sets it again.
    foreground: AtomicBool,
    /// True if its programs get a process group of their own. A job of a shell with job control
    /// always has one.
    grouped: bool,
    /// `set -o pipefail` was on when the job started: the status is the last failure.
    pipefail: bool,
    /// The signal that told the job to stop (`kill %n`), or 0. The shell code of a background
    /// job reads this where a foreground job reads the Ctrl+C flag.
    cancelled: AtomicI32,
    /// Rung when the state changes: what `wait`, `fg` and paused shell code sleep on.
    bell: Arc<Bell>,
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
    /// The programs of `live` that are stopped.
    stopped: Vec<Pid>,
    /// The signal of the latest stop.
    stop_signal: i32,
    /// A member was killed by a signal: it may have left the terminal in a strange state.
    hurt: bool,
    phase: Phase,
    /// How the last program that ended did so.
    last: Option<Termination>,
    /// The status of each task, once it is over.
    results: Vec<Option<ExitStatus>>,
    /// Which tasks have *settled*: started a program, run a command that is not a program, or
    /// finished. A task that has not is still on its way to its first program, and a job with
    /// such a task does not count as stopped (see `refresh`).
    settled: Vec<bool>,
    /// The terminal settings the job had when it was stopped.
    modes: Option<Termios>,
    /// Pids made up by tests (no process behind them), whose stops the kernel cannot confirm.
    #[cfg(test)]
    pretend: Vec<Pid>,
}

/// A program of a job that has started.
#[derive(Debug)]
pub(crate) struct Member {
    pid: Pid,
    /// The leader of a group. Its process stays a zombie until the job is dropped.
    anchor: bool,
}

/// One thread's share of a job: the shell code it runs. Report its status with [`Task::done`].
#[derive(Debug)]
pub(crate) struct Task {
    job: Arc<Job>,
    index: usize,
    done: bool,
}

impl Task {
    /// The task is over with this status.
    pub(crate) fn done(mut self, status: ExitStatus) {
        self.done = true;
        self.job.set_result(self.index, status);
    }
}

impl Drop for Task {
    fn drop(&mut self) {
        if !self.done {
            // The thread ended without saying how, which only a panic does.
            self.job.set_result(self.index, ExitStatus(134));
        }
    }
}

impl Job {
    fn new(
        foreground: bool,
        grouped: bool,
        bell: Arc<Bell>,
        tasks: usize,
        pipefail: bool,
    ) -> Arc<Self> {
        Arc::new(Self {
            foreground: AtomicBool::new(foreground),
            grouped,
            pipefail,
            cancelled: AtomicI32::new(0),
            bell,
            state: Mutex::new(State {
                pgid: None,
                leader: None,
                live: Vec::new(),
                stopped: Vec::new(),
                stop_signal: 0,
                hurt: false,
                phase: Phase::Running,
                last: None,
                results: vec![None; tasks],
                settled: vec![false; tasks],
                modes: None,
                #[cfg(test)]
                pretend: Vec::new(),
            }),
        })
    }

    /// A job that will own the terminal while it runs, made of `tasks` threads of shell code.
    /// `bell` is rung whenever the job changes.
    pub(crate) fn foreground(bell: Arc<Bell>, tasks: usize, pipefail: bool) -> Arc<Self> {
        Self::new(true, true, bell, tasks, pipefail)
    }

    /// A job that runs without the terminal, made of one thread of shell code. With `grouped`
    /// its programs get a process group of their own.
    pub(crate) fn background(grouped: bool, bell: Arc<Bell>) -> Arc<Self> {
        Self::new(false, grouped, bell, 1, false)
    }

    /// The share of the job that thread number `index` has.
    pub(crate) fn task(self: &Arc<Self>, index: usize) -> Task {
        Task {
            job: Arc::clone(self),
            index,
            done: false,
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn ring(&self) {
        self.bell.ring();
    }

    /// The job is stopped when it has programs, all of them are stopped, and every task has
    /// settled. A task that has not may be about to start a program (`sh -c 'kill -STOP $$' |
    /// cat` stops its first program before the second stage has reached `cat`), and bash keeps
    /// `a | b` running while `b` runs. It is no longer stopped when one runs again. A finished
    /// job stays finished.
    fn refresh(state: &mut State) {
        if matches!(state.phase, Phase::Finished(_)) {
            return;
        }
        let mut all_stopped = !state.live.is_empty()
            && state.live.iter().all(|pid| state.stopped.contains(pid))
            && state.settled.iter().all(|settled| *settled);
        if all_stopped {
            // The record of a stop can be stale: a program continued from outside is not
            // reported by the waits, and its waiter may not have seen it end yet. Ask the
            // kernel before the job counts as stopped, or a pipeline that was continued and
            // ran on would be given up by the shell as stopped.
            let gone: Vec<Pid> = state
                .stopped
                .iter()
                .copied()
                .filter(|pid| !Self::kernel_says_stopped(state, *pid))
                .collect();
            if !gone.is_empty() {
                state.stopped.retain(|pid| !gone.contains(pid));
                all_stopped = false;
            }
        }
        state.phase = match (all_stopped, state.phase) {
            (true, Phase::Stopped(signal)) => Phase::Stopped(signal),
            (true, _) => Phase::Stopped(state.stop_signal),
            (false, _) => Phase::Running,
        };
    }

    /// True if `pid` is really stopped, as far as the kernel says. In tests, pids made up with
    /// `pretend_started_by` have no process behind them, so their record is trusted.
    fn kernel_says_stopped(state: &State, pid: Pid) -> bool {
        #[cfg(test)]
        if state.pretend.contains(&pid) {
            return true;
        }
        let _ = state;
        spawn::still_stopped(pid)
    }

    fn set_result(&self, index: usize, status: ExitStatus) {
        let mut state = self.state();
        if let Some(slot) = state.results.get_mut(index) {
            *slot = Some(status);
        }
        if let Some(slot) = state.settled.get_mut(index) {
            *slot = true;
        }
        if state.results.iter().all(Option::is_some) {
            let results: Vec<ExitStatus> = state.results.iter().flatten().copied().collect();
            let status = if self.pipefail {
                // The last command that failed, if any.
                results
                    .iter()
                    .rev()
                    .copied()
                    .find(|s| *s != ExitStatus::SUCCESS)
                    .unwrap_or(ExitStatus::SUCCESS)
            } else {
                results.last().copied().unwrap_or(ExitStatus::SUCCESS)
            };
            state.phase = Phase::Finished(status);
        }
        // This task may have been the only one that had not settled (it ended without ever
        // starting a program, e.g. `nosuch`), which can make the job stopped.
        Self::refresh(&mut state);
        drop(state);
        self.ring();
    }

    /// Task `index` is no longer on its way to a program: it runs a command that starts none
    /// (a builtin, a function, an assignment). Shell code like that pauses by itself while the
    /// job is stopped (`pause_while_stopped`), so it must not keep the job from counting as
    /// stopped. Without this, `cat | while read l; do :; done` could never be stopped.
    pub(crate) fn settle(&self, index: usize) {
        let mut state = self.state();
        let before = state.phase;
        if let Some(slot) = state.settled.get_mut(index) {
            *slot = true;
        }
        Self::refresh(&mut state);
        let changed = state.phase != before;
        drop(state);
        if changed {
            self.ring();
        }
    }

    /// The bell this job rings.
    pub(crate) fn bell(&self) -> &Arc<Bell> {
        &self.bell
    }

    pub(crate) fn is_foreground(&self) -> bool {
        self.foreground.load(Ordering::SeqCst)
    }

    pub(crate) fn is_background(&self) -> bool {
        !self.is_foreground()
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
        state.stopped.clear();
        if matches!(state.phase, Phase::Stopped(_)) {
            state.phase = Phase::Running;
        }
        drop(state);
        self.ring();
    }

    /// Looks at the kernel again for a job that is recorded as stopped: a program continued from
    /// outside (`kill -CONT` from another shell) is not reported by the waits, so the record
    /// can be out of date. Rings the bell if the phase changed. It only looks (see
    /// [`spawn::still_stopped`]), so it is cheap and safe to call where jobs are shown.
    pub(crate) fn recheck(&self) {
        let mut state = self.state();
        // Nothing to ask the kernel about: a finished job, or one with no program.
        if matches!(state.phase, Phase::Finished(_)) || state.live.is_empty() {
            return;
        }
        let before = state.phase;
        Self::refresh(&mut state);
        let changed = state.phase != before;
        drop(state);
        if changed {
            self.ring();
        }
    }

    /// The programs of the job that are running.
    #[cfg(test)]
    pub(crate) fn live_pids(&self) -> Vec<Pid> {
        self.state().live.clone()
    }

    /// Records programs as started, without starting anything.
    #[cfg(test)]
    pub(crate) fn pretend_started(&self, pids: &[Pid]) {
        self.pretend_started_by(0, pids);
    }

    /// Records programs as started by task `task`, without starting anything.
    #[cfg(test)]
    pub(crate) fn pretend_started_by(&self, task: usize, pids: &[Pid]) {
        let mut state = self.state();
        state.live.extend_from_slice(pids);
        state.pretend.extend_from_slice(pids);
        if let Some(slot) = state.settled.get_mut(task) {
            *slot = true;
        }
        Self::refresh(&mut state);
    }

    /// Records a stop of `pid` as `wait_or_stop` does.
    #[cfg(test)]
    pub(crate) fn pretend_stopped(&self, pid: Pid, signal: i32) {
        let mut state = self.state();
        state.stopped.push(pid);
        state.stop_signal = signal;
        Self::refresh(&mut state);
    }

    /// Sets the phase without any program behind it.
    #[cfg(test)]
    pub(crate) fn state_for_test(&self, phase: Phase) {
        self.state().phase = phase;
        self.ring();
    }

    /// Shell code of this job calls this where it looks for Ctrl+C: while the job is stopped
    /// it sleeps, so a loop in a stopped pipeline does not spin. It returns when the job runs
    /// again or is told to stop.
    pub(crate) fn pause_while_stopped(&self) {
        loop {
            let seen = self.bell.count();
            if !matches!(self.phase(), Phase::Stopped(_)) || self.is_cancelled() {
                return;
            }
            self.bell.wait_after(seen, PAUSE_STEP);
        }
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
        if signal == Signal::SIGCONT {
            // Write "continued" into the record *before* the signal goes out. The program may
            // run on and stop again before this thread gets another turn, and the waiter
            // records that new stop; marking afterwards would erase it and leave a job that
            // reads `Running` with every program stopped (the flake of
            // `a_job_can_be_stopped_and_continued_again`).
            self.mark_continued();
            let sent = send(signal);
            #[cfg(test)]
            tests::pause_after_send();
            if sent.is_err() {
                self.recheck();
            }
            return sent;
        }
        send(signal)?;
        if stopped && matches!(signal, Signal::SIGTERM | Signal::SIGHUP) {
            // Same order: the record first, then the continue that lets the signal act.
            self.mark_continued();
            let sent = send(Signal::SIGCONT);
            #[cfg(test)]
            tests::pause_after_send();
            if sent.is_err() {
                self.recheck();
            }
            sent?;
        } else {
            self.ring();
        }
        Ok(())
    }

    /// Starts a program as a member of the job, run by task number `task` (which settles).
    ///
    /// # Errors
    ///
    /// Returns [`SpawnFailure`] if the program could not be started. The job is then as it was
    /// before: the next program becomes the leader if there was none.
    pub(crate) fn start(
        &self,
        request: &ExecRequest<'_>,
        plan: &mut FdPlan,
        task: usize,
    ) -> Result<Member, SpawnFailure> {
        // Held until the program runs, so that members start one at a time.
        let mut state = self.state();
        let leading = self.grouped && state.pgid.is_none();
        let group = self.grouped.then(|| {
            let tty = if leading && self.is_foreground() {
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
        if let Some(slot) = state.settled.get_mut(task) {
            *slot = true;
        }
        Self::refresh(&mut state);
        drop(state);
        self.ring();
        Ok(Member {
            pid,
            anchor: leading,
        })
    }

    /// Waits until the member ends or stops, and records which in the job. A stop is not the
    /// end: the caller decides whether to wait again.
    pub(crate) fn wait_or_stop(&self, member: &Member) -> Waited {
        let waited = spawn::wait_member(member.pid, member.anchor);
        match waited {
            Waited::Ended(ending) => {
                {
                    let mut state = self.state();
                    state.live.retain(|pid| *pid != member.pid);
                    state.stopped.retain(|pid| *pid != member.pid);
                    state.last = Some(ending);
                    if matches!(ending, Termination::Signaled { .. }) {
                        state.hurt = true;
                    }
                    Self::refresh(&mut state);
                }
                self.ring();
                if let Termination::Signaled { signal, .. } = ending
                    && signal == nix::libc::SIGINT
                    && self.is_foreground()
                {
                    // The shell is not in the foreground group, so it never saw the
                    // Ctrl+C itself. (A background job killed by `kill -INT` is none of
                    // the foreground's business.)
                    signals::note_interrupt();
                }
            }
            Waited::Stopped(signal) => {
                {
                    let mut state = self.state();
                    if !state.stopped.contains(&member.pid) {
                        state.stopped.push(member.pid);
                    }
                    state.stop_signal = signal;
                    Self::refresh(&mut state);
                }
                self.ring();
            }
        }
        waited
    }

    /// Waits until the member ends. Stops are recorded and waited out: the job's state shows
    /// them, and a `fg` or `bg` continues the program.
    pub(crate) fn wait_until_end(&self, member: &Member) -> Termination {
        loop {
            if let Waited::Ended(ending) = self.wait_or_stop(member) {
                return ending;
            }
        }
    }

    /// The job stops owning the terminal: the shell takes it back and puts its own terminal
    /// settings back, keeping the job's for `fg`.
    pub(crate) fn suspend(&self) {
        self.foreground.store(false, Ordering::SeqCst);
        let modes = terminal::get().and_then(terminal::Terminal::suspend_job);
        self.state().modes = modes;
        self.ring();
    }

    /// `fg`: the job owns the terminal again, with the settings it had, and runs.
    ///
    /// # Errors
    ///
    /// Returns the error of sending `SIGCONT`.
    pub(crate) fn resume_foreground(&self) -> Result<(), Errno> {
        let (pgid, modes) = {
            let mut state = self.state();
            state.hurt = false;
            (state.pgid, state.modes.clone())
        };
        self.foreground.store(true, Ordering::SeqCst);
        if let (Some(terminal), Some(group)) = (terminal::get(), pgid) {
            terminal.give_to(group, modes.as_ref());
        }
        self.signal(Signal::SIGCONT)
    }

    /// The job is over: the shell takes the terminal back. Call it once the job has finished.
    pub(crate) fn finish(&self) {
        if !self.is_foreground() {
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

#[cfg(test)]
pub(crate) mod tests {
    use std::cell::Cell;
    use std::time::Instant;

    use super::*;
    use crate::jobtable::Jobs;
    use crate::status::ExitStatus;

    thread_local! {
        /// How long `Job::signal` waits after it has sent a signal, on this thread only. Tests
        /// use it to make the program run on (and stop again) before the job's record changes.
        static PAUSE_AFTER_SEND: Cell<Duration> = const { Cell::new(Duration::ZERO) };
    }

    /// Makes the signals sent from this thread wait `pause` after the send.
    pub(crate) fn set_pause_after_send(pause: Duration) {
        PAUSE_AFTER_SEND.with(|cell| cell.set(pause));
    }

    pub(super) fn pause_after_send() {
        let pause = PAUSE_AFTER_SEND.with(Cell::get);
        if !pause.is_zero() {
            std::thread::sleep(pause);
        }
    }

    #[test]
    fn shell_code_of_a_stopped_job_sleeps_until_it_runs_again() {
        let jobs = Jobs::new();
        let job = Job::foreground(jobs.bell(), 1, false);
        job.pretend_started(&[Pid::from_raw(900_001)]);
        job.pretend_stopped(Pid::from_raw(900_001), nix::libc::SIGTSTP);
        let (sender, receiver) = std::sync::mpsc::channel();
        let sleeper = {
            let job = Arc::clone(&job);
            std::thread::spawn(move || {
                job.pause_while_stopped();
                let _ = sender.send(Instant::now());
            })
        };
        // Still asleep after a while.
        assert!(receiver.recv_timeout(Duration::from_millis(300)).is_err());
        let woken = Instant::now();
        job.mark_continued();
        let at = receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("the shell code did not wake up");
        assert!(at >= woken);
        sleeper.join().unwrap();
    }

    #[test]
    fn shell_code_of_a_stopped_job_wakes_when_the_job_is_told_to_end() {
        let jobs = Jobs::new();
        let job = Job::background(false, jobs.bell());
        job.pretend_started(&[Pid::from_raw(900_002)]);
        job.pretend_stopped(Pid::from_raw(900_002), nix::libc::SIGTTIN);
        let (sender, receiver) = std::sync::mpsc::channel();
        let sleeper = {
            let job = Arc::clone(&job);
            std::thread::spawn(move || {
                job.pause_while_stopped();
                let _ = sender.send(());
            })
        };
        assert!(receiver.recv_timeout(Duration::from_millis(300)).is_err());
        // `cancelled` is what `kill %n` sets; the pids here are made up, so signal nothing.
        job.cancelled
            .store(Signal::SIGTERM as i32, Ordering::SeqCst);
        receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("the shell code did not wake up");
        sleeper.join().unwrap();
    }

    #[test]
    fn a_job_with_a_task_that_has_not_settled_is_not_stopped() {
        let stopped_first = || {
            let jobs = Jobs::new();
            let job = Job::foreground(jobs.bell(), 2, false);
            let first = Pid::from_raw(900_003);
            job.pretend_started_by(0, &[first]);
            job.pretend_stopped(first, nix::libc::SIGTSTP);
            job
        };
        // Task 1 may still start a program: the one program that exists is stopped, and the
        // job runs on.
        let job = stopped_first();
        assert_eq!(job.phase(), Phase::Running);
        // It starts nothing (a builtin): now the job is stopped.
        job.settle(1);
        assert_eq!(job.phase(), Phase::Stopped(nix::libc::SIGTSTP));
        // Or it ends without a program.
        let job = stopped_first();
        job.task(1).done(ExitStatus(0));
        assert_eq!(job.phase(), Phase::Stopped(nix::libc::SIGTSTP));
        // Or its program starts and runs: the job still runs.
        let job = stopped_first();
        job.pretend_started_by(1, &[Pid::from_raw(900_004)]);
        assert_eq!(job.phase(), Phase::Running);
    }

    #[test]
    fn a_job_that_has_not_stopped_does_not_make_shell_code_wait() {
        let jobs = Jobs::new();
        let job = Job::foreground(jobs.bell(), 1, false);
        let start = Instant::now();
        job.pause_while_stopped();
        assert!(start.elapsed() < Duration::from_millis(50));
    }
}
