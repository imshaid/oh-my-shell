//! Background jobs: a command that ends with `&`.
//!
//! The shell does not wait for it. Its commands run on a thread of the shell, on a copy of the
//! shell, as the stages of a pipeline do: §4.3 forbids running shell code in a forked child, so
//! `{ a; b; } &` and `a && b &` cannot be a forked subshell. The programs it starts are the
//! job's members, in a process group of their own when the shell has job control.
//!
//! The thread outlives the call that started it. It ends when its commands end, or earlier if
//! someone stops it with `kill %n`. If the shell itself ends first, the thread goes with it,
//! while the programs it already started carry on.

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use omsh_syntax::Span;
use omsh_syntax::ast::ListItem;

use super::pipeline::STAGE_STACK;
use super::{Flow, Unwind};
use crate::io::Io;
use crate::jobs::{Job, Phase};
use crate::shell::Shell;
use crate::status::ExitStatus;

/// How long the shell waits, after starting a background job, for its first program to start.
/// It needs the process id of that program for `$!` and for the `[1] 1234` line. A job that
/// starts no program (`{ x=1; } &`) is not worth waiting for.
const START_WAIT: Duration = Duration::from_millis(100);

impl Shell {
    /// Starts `item` as a background job and returns at once with status 0.
    pub(super) fn run_background(&mut self, item: &ListItem, io: &Io) -> Flow {
        let job = Job::background(self.options.monitor, self.jobs.bell());
        let mut copy = self.subshell();
        copy.job = Some(Arc::clone(&job));
        copy.task = 0;
        let mut table = io.clone();
        if !self.options.monitor {
            // Without job control a background command must not read what the user types
            // (POSIX). Redirections of the command itself are applied after this.
            table.null_stdin();
        }

        let number = self.jobs.add(Arc::clone(&job), self.job_text(item.span));
        let and_or = item.and_or.clone();
        // Dropped without a status (a panic), the task counts as 134, as for a pipeline stage.
        let task = job.task(0);
        let thread_job = Arc::clone(&job);
        let spawned = thread::Builder::new()
            .stack_size(STAGE_STACK)
            .spawn(move || {
                let flow = copy.run_and_or(&and_or, &table);
                // `kill %n` told the shell code to stop: that is how it ended.
                let flow = match (flow, thread_job.cancelled_by()) {
                    (Err(Unwind::Interrupted), Some(signal)) => Ok(ExitStatus::from_signal(signal)),
                    (flow, _) => flow,
                };
                let status = copy.finish_copy(flow, &table);
                // Let go of the pipes before the job counts as over.
                drop(table);
                task.done(status);
            });
        if let Err(error) = spawned {
            self.jobs.remove(number);
            io.say(format!(
                "oh-my-shell: cannot start a background job: {error}"
            ));
            return Ok(ExitStatus(1));
        }

        self.wait_for_start(&job);
        self.last_background = job
            .leader()
            .and_then(|pid| u32::try_from(pid.as_raw()).ok());
        if self.interactive {
            match self.last_background {
                Some(pid) => io.say(format!("[{number}] {pid}")),
                None => io.say(format!("[{number}]")),
            }
        }
        Ok(ExitStatus::SUCCESS)
    }

    /// Waits a short time for the job's first program to start, or for the job to end.
    fn wait_for_start(&self, job: &Job) {
        let bell = self.jobs.bell();
        let deadline = Instant::now() + START_WAIT;
        loop {
            // Read before looking, so that a change in between is not missed.
            let seen = bell.count();
            if job.leader().is_some() || matches!(job.phase(), Phase::Finished(_)) {
                return;
            }
            let now = Instant::now();
            if now >= deadline {
                return;
            }
            bell.wait_after(seen, deadline - now);
        }
    }

    /// The text `jobs` shows for a command: what the user typed, without the `&`.
    fn job_text(&self, span: Span) -> String {
        self.source_text(span).map_or_else(
            || "(background job)".to_owned(),
            |text| {
                text.trim_end_matches(|c: char| c == '&' || c.is_whitespace())
                    .to_owned()
            },
        )
    }

    /// True if this shell runs the commands of a background job.
    pub(super) fn in_background_job(&self) -> bool {
        self.job.as_ref().is_some_and(|job| job.is_background())
    }

    /// The signals a program must ignore because of the job it belongs to: Ctrl+C and Ctrl+\
    /// for a background job of a shell without job control (POSIX), and the stop signals for a
    /// top-level `$(...)`. With job control a background job is in a group that the terminal
    /// does not signal.
    pub(super) fn job_ignored_signals(&self) -> u64 {
        match &self.job {
            Some(job) if job.is_background() && !job.is_grouped() => {
                (1_u64 << nix::libc::SIGINT) | (1_u64 << nix::libc::SIGQUIT)
            }
            // A top-level `$(...)` is part of the command being expanded, which is in the
            // foreground: Ctrl+Z must not stop one of its programs and leave the shell waiting
            // for the words (bash ignores the stop signals there too).
            None if self.no_new_jobs && self.options.monitor => {
                (1_u64 << nix::libc::SIGTSTP)
                    | (1_u64 << nix::libc::SIGTTIN)
                    | (1_u64 << nix::libc::SIGTTOU)
            }
            _ => 0,
        }
    }
}
