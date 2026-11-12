//! The table of background jobs: `[1]`, `[2]`, ... as `jobs` lists them.
//!
//! A job is added when a command ends with `&`. It is taken out when the user has been told
//! that it finished (a notice before a prompt, or `jobs`, or `wait`). Only the main shell has a
//! table with anything in it: a subshell starts with an empty one, as in bash.
//!
//! Everything that waits for a job (`wait`) sleeps on the table's [`Bell`]. The code that
//! changes a job's state rings it, so a waiter wakes up at once and not after a polling delay.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use nix::sys::signal::Signal;
use nix::unistd::Pid;

use crate::jobs::{Job, Phase};
use crate::status::Termination;

/// Wakes up whoever waits for a job to change. It counts rings, so a waiter that looked at the
/// state and then went to sleep cannot miss a change that happened in between.
#[derive(Debug, Default)]
pub(crate) struct Bell {
    rings: Mutex<u64>,
    changed: Condvar,
}

impl Bell {
    fn lock(&self) -> MutexGuard<'_, u64> {
        self.rings.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Says that something changed.
    pub(crate) fn ring(&self) {
        *self.lock() += 1;
        self.changed.notify_all();
    }

    /// The number of rings so far. Read it *before* looking at the state you wait for.
    pub(crate) fn count(&self) -> u64 {
        *self.lock()
    }

    /// Sleeps until the bell has rung since `seen` (a value of [`Bell::count`]), or `timeout`
    /// has passed.
    pub(crate) fn wait_after(&self, seen: u64, timeout: Duration) {
        let guard = self.lock();
        if *guard != seen {
            return;
        }
        let _ = self
            .changed
            .wait_timeout(guard, timeout)
            .unwrap_or_else(PoisonError::into_inner);
    }
}

/// What a job spec such as `%1` or `%sleep` stands for.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Spec {
    /// `%%`, `%+` or `%`: the current job.
    Current,
    /// `%-`: the one before it.
    Previous,
    /// `%n`.
    Number(u32),
    /// `%string`: the command starts with it.
    Prefix(String),
    /// `%?string`: the command contains it.
    Contains(String),
}

/// Why a job spec matched nothing useful.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SpecError {
    NoSuchJob,
    Ambiguous,
}

impl SpecError {
    pub(crate) const fn message(self) -> &'static str {
        match self {
            Self::NoSuchJob => "no such job",
            Self::Ambiguous => "ambiguous job spec",
        }
    }
}

/// Reads a job spec. Returns nothing if the text does not start with `%`, which means it is
/// not a job spec at all (a process id, for `kill` and `wait`).
pub(crate) fn parse_spec(text: &[u8]) -> Option<Spec> {
    let rest = text.strip_prefix(b"%")?;
    Some(match rest {
        b"" | b"%" | b"+" => Spec::Current,
        b"-" => Spec::Previous,
        digits if digits.iter().all(u8::is_ascii_digit) => {
            // A number too big for any job names none.
            Spec::Number(
                std::str::from_utf8(digits)
                    .ok()
                    .and_then(|d| d.parse().ok())
                    .unwrap_or(u32::MAX),
            )
        }
        [b'?', needle @ ..] if !needle.is_empty() => {
            Spec::Contains(String::from_utf8_lossy(needle).into_owned())
        }
        prefix => Spec::Prefix(String::from_utf8_lossy(prefix).into_owned()),
    })
}

/// One line of the table, as it is at the moment.
#[derive(Debug, Clone)]
pub(crate) struct View {
    pub(crate) number: u32,
    /// `+` for the current job, `-` for the one before it, and a space for the rest.
    pub(crate) mark: char,
    pub(crate) job: Arc<Job>,
    pub(crate) phase: Phase,
    pub(crate) text: String,
    /// `disown -h`: the job is left alone when the shell hangs up.
    pub(crate) no_hup: bool,
}

impl View {
    /// The line `jobs` prints. A running job ends with ` &`, as the user typed it. With
    /// `long`, the process id of the first program comes after the mark.
    pub(crate) fn line(&self, long: bool) -> String {
        let state = describe(self.phase, self.job.last_ending());
        let amp = if self.phase == Phase::Running {
            " &"
        } else {
            ""
        };
        if long {
            let pid = self
                .job
                .leader()
                .map_or_else(|| "-".to_owned(), |pid| pid.to_string());
            format!(
                "[{}]{} {pid} {state:<22}{}{amp}",
                self.number, self.mark, self.text
            )
        } else {
            format!(
                "[{}]{}  {state:<27}{}{amp}",
                self.number, self.mark, self.text
            )
        }
    }
}

#[derive(Debug)]
struct Entry {
    number: u32,
    job: Arc<Job>,
    text: String,
    /// The stopped state the user has been told about.
    shown: Option<Phase>,
    no_hup: bool,
}

#[derive(Debug, Default)]
struct Table {
    /// In order of number.
    entries: Vec<Entry>,
    current: Option<u32>,
    previous: Option<u32>,
}

impl Table {
    /// The numbers of the current and the previous job. The most recent job is current until
    /// something else is said, then the one added before it.
    fn marks(&self) -> (Option<u32>, Option<u32>) {
        let exists = |n: u32| self.entries.iter().any(|e| e.number == n);
        let current = self
            .current
            .filter(|n| exists(*n))
            .or_else(|| self.entries.last().map(|e| e.number));
        let previous = self
            .previous
            .filter(|n| exists(*n) && Some(*n) != current)
            .or_else(|| {
                self.entries
                    .iter()
                    .rev()
                    .map(|e| e.number)
                    .find(|n| Some(*n) != current)
            });
        (current, previous)
    }

    fn view(&self, entry: &Entry) -> View {
        // A stopped job may have been continued from outside; ask the kernel before showing it.
        entry.job.recheck();
        let (current, previous) = self.marks();
        let mark = if Some(entry.number) == current {
            '+'
        } else if Some(entry.number) == previous {
            '-'
        } else {
            ' '
        };
        View {
            number: entry.number,
            mark,
            job: Arc::clone(&entry.job),
            phase: entry.job.phase(),
            text: entry.text.clone(),
            no_hup: entry.no_hup,
        }
    }
}

/// The background jobs of one shell.
#[derive(Debug)]
pub(crate) struct Jobs {
    bell: Arc<Bell>,
    table: Mutex<Table>,
}

impl Jobs {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            bell: Arc::new(Bell::default()),
            table: Mutex::new(Table::default()),
        })
    }

    fn table(&self) -> MutexGuard<'_, Table> {
        self.table.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// What a job rings when it changes, and what `wait` sleeps on.
    pub(crate) fn bell(&self) -> Arc<Bell> {
        Arc::clone(&self.bell)
    }

    /// Adds a job and returns its number: one more than the highest in use, or 1 for an empty
    /// table.
    pub(crate) fn add(&self, job: Arc<Job>, text: String) -> u32 {
        self.insert(job, text, false)
    }

    /// Adds a job that has just been stopped in the foreground. The user is told about it
    /// right away, so no notice follows.
    pub(crate) fn add_stopped(&self, job: Arc<Job>, text: String) -> u32 {
        self.insert(job, text, true)
    }

    fn insert(&self, job: Arc<Job>, text: String, shown: bool) -> u32 {
        let mut table = self.table();
        let shown = shown.then(|| job.phase());
        let number = table.entries.last().map_or(1, |e| e.number + 1);
        let (current, _) = table.marks();
        table.previous = current;
        table.current = Some(number);
        table.entries.push(Entry {
            number,
            job,
            text,
            shown,
            no_hup: false,
        });
        drop(table);
        self.bell.ring();
        number
    }

    /// Every job, in order of number.
    pub(crate) fn list(&self) -> Vec<View> {
        let table = self.table();
        table.entries.iter().map(|e| table.view(e)).collect()
    }

    /// Looks a job up by spec.
    pub(crate) fn find(&self, spec: &Spec) -> Result<View, SpecError> {
        let table = self.table();
        let (current, previous) = table.marks();
        let matches: Vec<&Entry> = match spec {
            Spec::Current => table
                .entries
                .iter()
                .filter(|e| Some(e.number) == current)
                .collect(),
            Spec::Previous => table
                .entries
                .iter()
                .filter(|e| Some(e.number) == previous)
                .collect(),
            Spec::Number(n) => table.entries.iter().filter(|e| e.number == *n).collect(),
            Spec::Prefix(p) => table
                .entries
                .iter()
                .filter(|e| e.text.starts_with(p.as_str()))
                .collect(),
            Spec::Contains(p) => table
                .entries
                .iter()
                .filter(|e| e.text.contains(p.as_str()))
                .collect(),
        };
        match matches.as_slice() {
            [] => Err(SpecError::NoSuchJob),
            [one] => Ok(table.view(one)),
            _ => Err(SpecError::Ambiguous),
        }
    }

    /// The job that has the program `pid`: its first program, or one that still runs.
    pub(crate) fn find_pid(&self, pid: Pid) -> Option<View> {
        let table = self.table();
        table
            .entries
            .iter()
            .find(|e| e.job.leader() == Some(pid) || e.job.has_live(pid))
            .map(|e| table.view(e))
    }

    /// Records that the user has been told a job is stopped, and returns the line to tell them.
    pub(crate) fn announce_stopped(&self, number: u32) -> Option<String> {
        let mut table = self.table();
        let view = table
            .entries
            .iter()
            .find(|e| e.number == number)
            .map(|e| table.view(e))?;
        if let Some(entry) = table.entries.iter_mut().find(|e| e.number == number) {
            entry.shown = Some(view.phase);
        }
        Some(view.line(false))
    }

    /// Makes a job the current one (`fg` and `bg` do), and the old current one the previous.
    pub(crate) fn make_current(&self, number: u32) {
        let mut table = self.table();
        let (current, _) = table.marks();
        if current != Some(number) {
            table.previous = current;
            table.current = Some(number);
        }
    }

    /// `disown -h`: the job stays in the table but does not get `SIGHUP` when the shell ends.
    pub(crate) fn keep_on_hangup(&self, number: u32) {
        if let Some(entry) = self.table().entries.iter_mut().find(|e| e.number == number) {
            entry.no_hup = true;
        }
    }

    /// True if some job is stopped.
    pub(crate) fn has_stopped(&self) -> bool {
        self.table().entries.iter().any(|e| {
            e.job.recheck();
            matches!(e.job.phase(), Phase::Stopped(_))
        })
    }

    /// Takes a job out of the table.
    pub(crate) fn remove(&self, number: u32) {
        self.table().entries.retain(|e| e.number != number);
    }

    /// True if there is no job at all.
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.table().entries.is_empty()
    }

    /// The lines to show before the next prompt: a job that finished (it leaves the table), and
    /// a job that stopped since the user was last told. Each is shown once.
    pub(crate) fn notices(&self) -> Vec<String> {
        let mut table = self.table();
        let mut lines = Vec::new();
        let mut done = Vec::new();
        let views: Vec<View> = table.entries.iter().map(|e| table.view(e)).collect();
        for (entry, view) in table.entries.iter_mut().zip(views) {
            match view.phase {
                Phase::Finished(_) => {
                    lines.push(view.line(false));
                    done.push(entry.number);
                }
                Phase::Stopped(_) if entry.shown != Some(view.phase) => {
                    entry.shown = Some(view.phase);
                    lines.push(view.line(false));
                }
                Phase::Stopped(_) => {}
                // Running again: if it stops once more, the user is told once more.
                Phase::Running => entry.shown = None,
            }
        }
        table.entries.retain(|e| !done.contains(&e.number));
        lines
    }
}

/// The word for a state, as bash says it: `Running`, `Done`, `Exit 3`, `Terminated`, ...
fn describe(phase: Phase, last: Option<Termination>) -> String {
    match phase {
        Phase::Running => "Running".to_owned(),
        Phase::Stopped(signal) => match Signal::try_from(signal) {
            Ok(Signal::SIGTTIN) => "Stopped (tty input)".to_owned(),
            Ok(Signal::SIGTTOU) => "Stopped (tty output)".to_owned(),
            _ => "Stopped".to_owned(),
        },
        Phase::Finished(status) => {
            if let Some(Termination::Signaled { signal, .. }) = last
                && crate::status::ExitStatus::from_signal(signal) == status
            {
                return signal_text(signal);
            }
            if status.code() == 0 {
                "Done".to_owned()
            } else {
                format!("Exit {}", status.code())
            }
        }
    }
}

/// What a signal that killed a job is called in a notice.
fn signal_text(signal: i32) -> String {
    let text = match Signal::try_from(signal) {
        Ok(Signal::SIGHUP) => "Hangup",
        Ok(Signal::SIGINT) => "Interrupt",
        Ok(Signal::SIGQUIT) => "Quit",
        Ok(Signal::SIGILL) => "Illegal instruction",
        Ok(Signal::SIGABRT) => "Aborted",
        Ok(Signal::SIGBUS) => "Bus error",
        Ok(Signal::SIGFPE) => "Floating point exception",
        Ok(Signal::SIGKILL) => "Killed",
        Ok(Signal::SIGUSR1) => "User defined signal 1",
        Ok(Signal::SIGSEGV) => "Segmentation fault",
        Ok(Signal::SIGUSR2) => "User defined signal 2",
        Ok(Signal::SIGPIPE) => "Broken pipe",
        Ok(Signal::SIGALRM) => "Alarm clock",
        Ok(Signal::SIGTERM) => "Terminated",
        _ => return format!("Signal {signal}"),
    };
    text.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::ExitStatus;

    fn job(jobs: &Jobs) -> Arc<Job> {
        Job::background(false, jobs.bell())
    }

    #[test]
    fn specs_are_read_by_their_first_character() {
        assert_eq!(parse_spec(b"%"), Some(Spec::Current));
        assert_eq!(parse_spec(b"%%"), Some(Spec::Current));
        assert_eq!(parse_spec(b"%+"), Some(Spec::Current));
        assert_eq!(parse_spec(b"%-"), Some(Spec::Previous));
        assert_eq!(parse_spec(b"%12"), Some(Spec::Number(12)));
        assert_eq!(parse_spec(b"%sleep"), Some(Spec::Prefix("sleep".into())));
        assert_eq!(parse_spec(b"%?eep"), Some(Spec::Contains("eep".into())));
        assert_eq!(parse_spec(b"1234"), None);
        assert_eq!(parse_spec(b""), None);
        // Too big for any job.
        assert_eq!(
            parse_spec(b"%99999999999999999999"),
            Some(Spec::Number(u32::MAX))
        );
    }

    #[test]
    fn numbers_count_up_and_start_again_when_the_table_is_empty() {
        let jobs = Jobs::new();
        assert_eq!(jobs.add(job(&jobs), "a".into()), 1);
        assert_eq!(jobs.add(job(&jobs), "b".into()), 2);
        jobs.remove(2);
        assert_eq!(jobs.add(job(&jobs), "c".into()), 2);
        jobs.remove(1);
        jobs.remove(2);
        assert!(jobs.is_empty());
        assert_eq!(jobs.add(job(&jobs), "d".into()), 1);
    }

    #[test]
    fn the_newest_job_is_current_and_the_one_before_it_is_previous() {
        let jobs = Jobs::new();
        jobs.add(job(&jobs), "one".into());
        jobs.add(job(&jobs), "two".into());
        jobs.add(job(&jobs), "three".into());
        assert_eq!(jobs.find(&Spec::Current).unwrap().text, "three");
        assert_eq!(jobs.find(&Spec::Previous).unwrap().text, "two");
        let marks: String = jobs.list().iter().map(|v| v.mark).collect();
        assert_eq!(marks, " -+");
        // Removing the current job moves the marks down.
        jobs.remove(3);
        assert_eq!(jobs.find(&Spec::Current).unwrap().text, "two");
        assert_eq!(jobs.find(&Spec::Previous).unwrap().text, "one");
    }

    #[test]
    fn a_spec_finds_one_job_or_says_why_not() {
        let jobs = Jobs::new();
        jobs.add(job(&jobs), "sleep 5".into());
        jobs.add(job(&jobs), "sleep 9".into());
        jobs.add(job(&jobs), "cat".into());
        assert_eq!(jobs.find(&Spec::Number(2)).unwrap().text, "sleep 9");
        assert_eq!(
            jobs.find(&Spec::Number(7)).unwrap_err(),
            SpecError::NoSuchJob
        );
        assert_eq!(jobs.find(&Spec::Prefix("ca".into())).unwrap().number, 3);
        assert_eq!(jobs.find(&Spec::Contains("at".into())).unwrap().number, 3);
        assert_eq!(
            jobs.find(&Spec::Prefix("sleep".into())).unwrap_err(),
            SpecError::Ambiguous
        );
        assert_eq!(
            jobs.find(&Spec::Prefix("nope".into())).unwrap_err(),
            SpecError::NoSuchJob
        );
        assert_eq!(
            Jobs::new().find(&Spec::Current).unwrap_err(),
            SpecError::NoSuchJob
        );
    }

    #[test]
    fn a_finished_job_is_reported_once_and_leaves_the_table() {
        let jobs = Jobs::new();
        let running = job(&jobs);
        let done = job(&jobs);
        jobs.add(Arc::clone(&running), "slow".into());
        jobs.add(Arc::clone(&done), "quick".into());
        assert!(jobs.notices().is_empty());
        done.task(0).done(ExitStatus(0));
        assert_eq!(jobs.notices(), ["[2]+  Done                       quick"]);
        assert!(jobs.notices().is_empty());
        assert_eq!(jobs.list().len(), 1);
        running.task(0).done(ExitStatus(3));
        assert_eq!(jobs.notices(), ["[1]+  Exit 3                     slow"]);
        assert!(jobs.is_empty());
    }

    #[test]
    fn a_stopped_job_is_reported_once_and_stays() {
        let jobs = Jobs::new();
        let stopped = job(&jobs);
        jobs.add(Arc::clone(&stopped), "cat".into());
        // Pretend the member reported a stop: set the phase through the public path.
        stopped.state_for_test(Phase::Stopped(nix::libc::SIGTTIN));
        assert_eq!(jobs.notices(), ["[1]+  Stopped (tty input)        cat"]);
        assert!(jobs.notices().is_empty());
        assert_eq!(jobs.list().len(), 1);
        assert!(jobs.has_stopped());
    }

    #[test]
    fn a_job_stopped_in_the_foreground_is_not_announced_a_second_time() {
        let jobs = Jobs::new();
        let stopped = job(&jobs);
        stopped.state_for_test(Phase::Stopped(nix::libc::SIGTSTP));
        jobs.add_stopped(Arc::clone(&stopped), "vim".into());
        assert!(jobs.notices().is_empty());
        // It runs again and stops again: that is news.
        stopped.state_for_test(Phase::Running);
        assert!(jobs.notices().is_empty());
        stopped.state_for_test(Phase::Stopped(nix::libc::SIGTSTP));
        assert_eq!(jobs.notices(), ["[1]+  Stopped                    vim"]);
    }

    #[test]
    fn fg_and_bg_make_a_job_the_current_one() {
        let jobs = Jobs::new();
        jobs.add(job(&jobs), "one".into());
        jobs.add(job(&jobs), "two".into());
        jobs.make_current(1);
        assert_eq!(jobs.find(&Spec::Current).unwrap().text, "one");
        assert_eq!(jobs.find(&Spec::Previous).unwrap().text, "two");
        // Already current: nothing moves.
        jobs.make_current(1);
        assert_eq!(jobs.find(&Spec::Previous).unwrap().text, "two");
    }

    #[test]
    fn a_job_is_stopped_only_when_every_program_of_it_is() {
        use nix::unistd::Pid;
        let jobs = Jobs::new();
        let job = job(&jobs);
        // Two programs, one stopped: the job still runs. (Pids are made up; no process is
        // touched.)
        job.pretend_started(&[Pid::from_raw(900_001), Pid::from_raw(900_002)]);
        job.pretend_stopped(Pid::from_raw(900_001), nix::libc::SIGTSTP);
        assert_eq!(job.phase(), Phase::Running);
        job.pretend_stopped(Pid::from_raw(900_002), nix::libc::SIGTSTP);
        assert_eq!(job.phase(), Phase::Stopped(nix::libc::SIGTSTP));
        job.mark_continued();
        assert_eq!(job.phase(), Phase::Running);
    }

    #[test]
    fn a_job_is_finished_when_every_task_is_over() {
        let jobs = Jobs::new();
        let job = Job::foreground(jobs.bell(), 3, false);
        job.task(0).done(ExitStatus(5));
        job.task(2).done(ExitStatus(0));
        assert_eq!(job.phase(), Phase::Running);
        job.task(1).done(ExitStatus(7));
        // The status is the last stage's.
        assert_eq!(job.phase(), Phase::Finished(ExitStatus(0)));
        // With pipefail it is the last failure.
        let job = Job::foreground(jobs.bell(), 3, true);
        job.task(0).done(ExitStatus(5));
        job.task(1).done(ExitStatus(7));
        job.task(2).done(ExitStatus(0));
        assert_eq!(job.phase(), Phase::Finished(ExitStatus(7)));
    }

    #[test]
    fn a_task_that_is_dropped_without_a_status_counts_as_a_crash() {
        let jobs = Jobs::new();
        let job = Job::foreground(jobs.bell(), 1, false);
        drop(job.task(0));
        assert_eq!(job.phase(), Phase::Finished(ExitStatus(134)));
    }

    #[test]
    fn a_running_job_is_listed_with_an_ampersand() {
        let jobs = Jobs::new();
        jobs.add(job(&jobs), "sleep 5".into());
        let view = jobs.list().remove(0);
        assert_eq!(
            view.line(false),
            "[1]+  Running                    sleep 5 &"
        );
        assert_eq!(view.line(true), "[1]+ - Running               sleep 5 &");
    }

    #[test]
    fn states_are_worded_like_bash() {
        let killed = |signal| Termination::Signaled {
            signal,
            core_dumped: false,
        };
        let done = |code: u8, last| describe(Phase::Finished(ExitStatus(code)), last);
        assert_eq!(done(0, Some(Termination::Exited(0))), "Done");
        assert_eq!(done(7, Some(Termination::Exited(7))), "Exit 7");
        assert_eq!(done(143, Some(killed(15))), "Terminated");
        assert_eq!(done(137, Some(killed(9))), "Killed");
        // A program that exits with 143 by itself is not "Terminated".
        assert_eq!(done(143, Some(Termination::Exited(143))), "Exit 143");
        assert_eq!(done(143, None), "Exit 143");
    }

    #[test]
    fn the_bell_does_not_lose_a_ring_that_comes_before_the_wait() {
        let bell = Bell::default();
        let seen = bell.count();
        bell.ring();
        // Returns at once instead of sleeping for the timeout.
        let start = std::time::Instant::now();
        bell.wait_after(seen, Duration::from_secs(5));
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
