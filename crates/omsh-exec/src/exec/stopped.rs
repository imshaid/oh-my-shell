//! Stopped jobs: what the main shell does when Ctrl+Z stops the job it is waiting for.
//!
//! The shell has a prompt to go back to, so it is the only code that gives up on a stopped
//! foreground job. Everything else (pipeline stages, the thread of a background job, a copy of
//! the shell inside a job) records the stop in the [`Job`] and goes on waiting; see `jobs.rs`.
//!
//! The steps for a stop:
//!
//! 1. Whoever waits for a program (a stage thread, or the main thread for a single command)
//!    learns of the stop and writes it into the job. When every program of the job is stopped
//!    and every task has settled (started a program, run a command that starts none, or
//!    finished: a stage still on its way to its program keeps the job running), the job's
//!    phase becomes `Stopped` and its bell rings.
//! 2. The main shell notices (it was waiting on the bell, or it was the one that waited for the
//!    single program), takes the terminal back with the shell's own terminal settings (the
//!    job's are kept for `fg`), puts the job in the table and says so.
//! 3. If the main thread was the one blocked in `waitpid` (a lone `sleep 30`), a small thread
//!    takes over the waiting: the program's `Member` and the job's task go to it. Later, `fg`
//!    or `bg` continue the program, and that thread sees it end.

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use omsh_syntax::Span;

use super::{Flow, Program, Unwind};
use crate::io::Io;
use crate::jobs::{Job, Member, Phase, Task};
use crate::jobtable::View;
use crate::shell::Shell;
use crate::status::ExitStatus;

/// How often the shell looks at a foreground job that has not changed. A change wakes it
/// sooner.
const WAIT_STEP: Duration = Duration::from_millis(100);

/// The stack of the thread that waits for a stopped program: it only calls `waitpid`.
const WAITER_STACK: usize = 256 * 1024;

impl Shell {
    /// The text of the command at `span`, as it was typed.
    pub(super) fn source_text(&self, span: Span) -> Option<String> {
        self.source
            .as_deref()
            .and_then(|source| source.get(span.start..span.end))
            .map(|text| text.trim().replace('\n', " "))
    }

    /// What a job is called in the table: the command as typed, or the words of the program
    /// where the source is not at hand (inside a function).
    fn foreground_text(&self, program: &Program) -> String {
        self.current_span
            .and_then(|span| self.source_text(span))
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| {
                program
                    .argv
                    .iter()
                    .map(|word| word.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
    }

    /// The text for a job made of the command at `span`.
    pub(super) fn span_text(&self, span: Span) -> String {
        self.source_text(span)
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| "(job)".to_owned())
    }

    /// The program of a job of its own has stopped while the main thread waited for it. A
    /// thread takes over the waiting, and the job goes to the table.
    pub(super) fn hand_over_and_stop(
        &self,
        job: &Arc<Job>,
        member: Member,
        task: Task,
        program: &Program,
        io: &Io,
    ) -> Flow {
        let waiter = Arc::clone(job);
        let spawned = thread::Builder::new()
            .stack_size(WAITER_STACK)
            .spawn(move || {
                let ending = waiter.wait_until_end(&member);
                task.done(ending.status());
            });
        if let Err(error) = spawned {
            // The program is stopped and nobody will collect it; say so rather than hide it.
            io.say(format!(
                "oh-my-shell: cannot wait for a stopped job: {error}; ending it"
            ));
            let _ = job.signal(nix::sys::signal::Signal::SIGKILL);
        }
        self.stop_foreground(job, None, &self.foreground_text(program), io)
    }

    /// Waits until the job is over or stopped. The job's own threads do the waiting for its
    /// programs; this only sleeps on the bell.
    pub(super) fn wait_foreground(&self, job: &Job) -> Phase {
        let bell = job.bell();
        loop {
            // Read before looking, so that a change in between is not missed.
            let seen = bell.count();
            match job.phase() {
                Phase::Running => bell.wait_after(seen, WAIT_STEP),
                other => return other,
            }
        }
    }

    /// A foreground job whose threads are running (a pipeline or a subshell): waits for it,
    /// and either returns its status or puts it in the table as stopped.
    pub(super) fn finish_or_stop(&self, job: &Arc<Job>, text: &str, io: &Io) -> Flow {
        match self.wait_foreground(job) {
            Phase::Finished(status) => {
                job.finish();
                Ok(status)
            }
            Phase::Stopped(_) | Phase::Running => self.stop_foreground(job, None, text, io),
        }
    }

    /// The foreground `job` has stopped. The shell takes the terminal back, the job goes to
    /// the table (or stays there, with `number`) and the user is told. The status is 128 plus
    /// the signal. Inside a loop the loops are left (see [`Unwind::Stopped`]).
    pub(crate) fn stop_foreground(
        &self,
        job: &Arc<Job>,
        number: Option<u32>,
        text: &str,
        io: &Io,
    ) -> Flow {
        let signal = match job.phase() {
            Phase::Stopped(signal) => signal,
            _ => nix::libc::SIGTSTP,
        };
        job.suspend();
        let number =
            number.unwrap_or_else(|| self.jobs.add_stopped(Arc::clone(job), text.to_owned()));
        self.jobs.make_current(number);
        if self.interactive {
            // The terminal has echoed `^Z` without a line ending.
            let _ = io.write_all(2, b"\n");
        }
        if let Some(line) = self.jobs.announce_stopped(number) {
            io.say(line);
        }
        let status = ExitStatus::from_signal(signal);
        if self.loop_depth > 0 {
            Err(Unwind::Stopped(status))
        } else {
            Ok(status)
        }
    }

    /// `fg`: continues the job in the foreground and waits for it. It may stop again.
    pub(crate) fn resume_in_foreground(&self, view: &View, io: &Io) -> Flow {
        let job = &view.job;
        let _ = io.write_all(1, format!("{}\n", view.text).as_bytes());
        self.jobs.make_current(view.number);
        if let Err(errno) = job.resume_foreground() {
            io.say(format!("oh-my-shell: fg: {}: {}", view.text, errno.desc()));
            return Ok(ExitStatus(1));
        }
        match self.wait_foreground(job) {
            Phase::Finished(status) => {
                self.jobs.remove(view.number);
                job.finish();
                if let Some(line) = job.last_ending().and_then(|e| e.describe(&view.text)) {
                    io.say(line);
                }
                Ok(status)
            }
            Phase::Stopped(_) | Phase::Running => {
                self.stop_foreground(job, Some(view.number), &view.text, io)
            }
        }
    }
}
