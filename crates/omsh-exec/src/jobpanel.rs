//! The jobs panel's place in the shell (U6).
//!
//! `jobs -i` shows the job table as a panel with buttons. Like `undo`, the panel is not drawn
//! here: the executor may not depend on the crate that draws (the workspace's dependency
//! direction), so the program that owns the shell hands it something that implements
//! [`JobsPanel`]. This module holds what that panel is told (a [`JobWatch`] that gives
//! [`JobRow`]s) and what it may answer ([`Picked`]); `builtins/jobs.rs` does the acting.
//!
//! The rows are plain facts: a number, the mark, a state with its real words, the command text
//! and the time since the job was made. Nothing here is invented: the state words are the
//! ones `jobs` prints (`describe` is built from the same [`JobState`]).

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use crate::builtins::Terminal;
use crate::jobtable::Jobs;
use crate::shell::Shell;

/// Where a job is, in the words `jobs` uses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobState {
    Running,
    /// Stopped. `Some("tty input")` or `Some("tty output")` when the terminal stopped it.
    Stopped(Option<&'static str>),
    /// Over with status 0.
    Done,
    /// Over with this nonzero status.
    Exit(u8),
    /// Ended by a signal; the text is the notice word (`Terminated`, `Killed`, ...).
    Signaled(String),
}

impl JobState {
    /// The words of the `jobs` listing: `Running`, `Stopped (tty input)`, `Done`, `Exit 3`,
    /// `Terminated`.
    #[must_use]
    pub fn word(&self) -> String {
        match self {
            Self::Running => "Running".to_owned(),
            Self::Stopped(None) => "Stopped".to_owned(),
            Self::Stopped(Some(why)) => format!("Stopped ({why})"),
            Self::Done => "Done".to_owned(),
            Self::Exit(code) => format!("Exit {code}"),
            Self::Signaled(word) => word.clone(),
        }
    }

    /// True once the job is over, whatever way.
    #[must_use]
    pub const fn is_finished(&self) -> bool {
        matches!(self, Self::Done | Self::Exit(_) | Self::Signaled(_))
    }
}

/// One line of the job table, as it is at the moment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobRow {
    pub number: u32,
    /// `+` for the current job, `-` for the one before it, and a space for the rest.
    pub mark: char,
    pub state: JobState,
    pub text: String,
    /// Since the job was made, or until it ended.
    pub elapsed: Duration,
}

/// What the panel can ask for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobAction {
    /// `fg`: continue the job and wait for it.
    Foreground,
    /// `bg`: continue a stopped job without the terminal.
    Background,
    /// `SIGSTOP`: stop a running job.
    Stop,
    /// `SIGTERM`, as `kill %n` (a stopped job is continued so that it can act on it). The
    /// panel has asked the question already.
    Kill,
}

/// What a panel answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Picked {
    /// The panel was closed (Esc, or the terminal went away).
    Closed,
    /// The panel could not be shown (no terminal to draw on). Asked first, `jobs -i` prints
    /// the plain list instead.
    Unavailable,
    Act(u32, JobAction),
    /// Ctrl+C or a hang-up: the line ends as it does for other builtins.
    Interrupted,
}

/// A look at the job table that the panel can repeat, to keep its rows current.
#[derive(Clone)]
pub struct JobWatch(Arc<Jobs>);

impl JobWatch {
    /// Every job, in order of number. Nothing is removed from the table.
    #[must_use]
    pub fn rows(&self) -> Vec<JobRow> {
        self.0.rows()
    }
}

impl fmt::Debug for JobWatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("JobWatch")
    }
}

/// What `jobs -i` shows. Implemented by the program that owns the shell.
pub trait JobsPanel: Send + Sync {
    /// Shows the table and returns what the person chose. It may be called again after an
    /// action, with the same `watch`.
    fn pick(&self, watch: &JobWatch, terminal: &mut dyn Terminal) -> Picked;
}

/// The shell's handle on its jobs panel. Shared by every copy of the shell.
#[derive(Clone, Default)]
pub(crate) struct JobsPanelHook(pub(crate) Option<Arc<dyn JobsPanel>>);

impl fmt::Debug for JobsPanelHook {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if self.0.is_some() {
            "JobsPanelHook(set)"
        } else {
            "JobsPanelHook(none)"
        })
    }
}

impl Shell {
    /// Gives the shell its jobs panel. Without one, `jobs -i` prints the plain list.
    pub fn set_jobs_panel(&mut self, panel: Arc<dyn JobsPanel>) {
        self.jobs_panel = JobsPanelHook(Some(panel));
    }

    /// A look at this shell's job table.
    #[must_use]
    pub fn job_watch(&self) -> JobWatch {
        JobWatch(Arc::clone(&self.jobs))
    }

    /// The job table now, without removing anything.
    #[must_use]
    pub fn job_rows(&self) -> Vec<JobRow> {
        self.jobs.rows()
    }

    /// The exit status of a job that is over (`128 + n` when a signal ended it), or `None`
    /// while it runs or is stopped, or when no such job is in the table. Nothing is removed.
    #[must_use]
    pub fn job_status(&self, number: u32) -> Option<u8> {
        self.jobs
            .list()
            .into_iter()
            .find(|view| view.number == number)
            .and_then(|view| match view.phase {
                crate::jobs::Phase::Finished(status) => Some(status.code()),
                _ => None,
            })
    }
}
