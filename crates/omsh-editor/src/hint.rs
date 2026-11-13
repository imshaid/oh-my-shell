//! Fish-style hints: the gray rest of a command, taken from the history, while you type.
//!
//! [`ShellHinter`] is called by `reedline` on every keystroke, so it must be cheap (blueprint
//! §16: highlight and hint together under 5 ms). The expensive part is the question to the
//! history database (`Store::hint`, which looks at the newest rows only). Three things keep it
//! rare:
//!
//! - **a memory per prompt**: the answer for each line already asked about, so a backspace
//!   costs nothing;
//! - **a reuse rule**: when the new line extends the previous one and the previous answer
//!   still starts with it, that answer is still the best (the ranking belongs to the whole
//!   command and does not depend on how much of it was typed), and when the previous line had
//!   no hint, a longer line has none either. No question is asked in both cases;
//! - **a wait for the writer** at the first question of a prompt (a short limit), so the
//!   command that just ran can be suggested.
//!
//! **The first key of a prompt** used to cost 3 to 6 ms: the first question has the shortest
//! text, so the database groups its whole window for a prefix that matches thousands of rows
//! (measured with a timer around each step: the highlighter 25 us, the wait for the writer
//! 0.1 to 0.2 ms, the question 2.3 to 3.6 ms). The ranking of a command does not depend on what
//! is typed, so as soon as a prompt is drawn a helper thread asks the database once for the
//! best commands in order ([`Store::hint_ranking`]) and the first keys are answered from that
//! list. A key typed before the list is ready is answered by the question as before.
//!
//! **What may be remembered.** "Nothing to suggest" is an answer only when the history is up to
//! date: the writer has stored what was typed before this prompt, and the database could be
//! read. If the wait for the writer timed out (a slow disk, another shell holding the database),
//! or the database was not open or not readable, the answer is *unknown*, not "nothing": it is
//! shown if there is one, but it is not remembered, and it is not used by the reuse rule, so the
//! next keystroke asks again (with a very short wait, so that typing is never held up). Without
//! this a hint that missed the first question was missing for the rest of the prompt. This was
//! the failure of two PTY tests on the CI runner.
//!
//! A hint is accepted with Right or End (all of it) and Alt+F (one word); reedline's default
//! key bindings already do that.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;

use omsh_history::{NotReady, Ranking, Store, now_ms};
use omsh_syntax::trigger;
use reedline::{Hinter, History};

/// How long the first question of a prompt may wait for the history writer.
const FIRST_WAIT: Duration = Duration::from_millis(50);

/// How long each later question waits, while the writer has still not caught up. Short: this
/// is on the path of a keystroke, and a writer that is that far behind is held up for a while.
const RETRY_WAIT: Duration = Duration::from_millis(2);

/// How many commands the ranking of a prompt holds at most. A text that matches none of them is
/// asked of the database (see [`Ranking::best`]), so the number only trades memory (about
/// 200 KB at the limit) against how often that happens.
const RANKING_LIMIT: usize = 5_000;

/// Where hints come from. [`Store`] is the real one; tests use fakes.
pub trait HintSource: Send + Sync {
    /// Waits for at most `limit` until the commands that just ran are readable. True if they
    /// are.
    fn settle(&self, limit: Duration) -> bool;
    /// The best whole command that starts with `prefix` and is longer than it: `Ok(None)` when
    /// the history has none, `Err` when it cannot be asked now.
    ///
    /// # Errors
    ///
    /// [`NotReady`] when the history cannot be read at this moment.
    fn best(&self, prefix: &str, cwd: &str) -> Result<Option<String>, NotReady>;
    /// The best commands for `cwd` in order, if the source can give them and the history is up
    /// to date (it may wait for the writer). Asked once for each prompt, on a helper thread. The
    /// default gives none, and every question goes to [`HintSource::best`].
    fn ranking(&self, _cwd: &str) -> Option<Ranking> {
        None
    }
}

impl HintSource for Store {
    fn settle(&self, limit: Duration) -> bool {
        Store::settle(self, limit)
    }

    fn best(&self, prefix: &str, cwd: &str) -> Result<Option<String>, NotReady> {
        self.hint(prefix, cwd, now_ms())
    }

    fn ranking(&self, cwd: &str) -> Option<Ranking> {
        // A list made before the writer caught up could miss the command that just ran.
        if !Store::settle(self, FIRST_WAIT) {
            return None;
        }
        self.hint_ranking(cwd, now_ms(), RANKING_LIMIT).ok()
    }
}

/// What the hinter knows about the prompt being typed at. Renewed by [`Editor::read`].
///
/// [`Editor::read`]: crate::Editor::read
#[derive(Default)]
pub(crate) struct HintState {
    cwd: String,
    /// Where the helper thread of [`start_ranking`] asks. Set when the hinter is made.
    source: Option<Arc<dyn HintSource>>,
    /// The number of this prompt. A ranking from an older prompt is thrown away.
    generation: u64,
    /// The commands in order of rank, once the helper thread has made them for this prompt. A
    /// ranking is only made after the writer caught up, so its answers may be remembered.
    ranking: Option<Arc<Ranking>>,
    /// Where [`start_ranking`] sends the work for the one helper thread, once it is started. The
    /// thread ends when this is dropped with the state.
    worker: Option<Sender<Job>>,
    /// True once the writer has been seen to catch up at this prompt. Until then no answer is
    /// remembered.
    settled: bool,
    /// True once the writer has been waited for at this prompt (the first wait is the long one).
    waited: bool,
    /// The best command for each line asked about at this prompt (none: no hint).
    answers: HashMap<String, Option<String>>,
    /// The line asked about last, and its answer.
    previous: Option<(String, Option<String>)>,
}

impl HintState {
    pub(crate) fn reset(&mut self, cwd: String) {
        self.cwd = cwd;
        self.generation += 1;
        self.ranking = None;
        self.settled = false;
        self.waited = false;
        self.answers.clear();
        self.previous = None;
    }
}

/// The hinter that `reedline` calls.
pub struct ShellHinter {
    source: Arc<dyn HintSource>,
    state: Arc<Mutex<HintState>>,
    style: nu_ansi_term::Style,
    /// What `handle` returned last, unstyled: what Right and Alt+F bring into the line.
    current: String,
}

impl ShellHinter {
    pub(crate) fn new(
        source: Box<dyn HintSource>,
        state: Arc<Mutex<HintState>>,
        style: nu_ansi_term::Style,
    ) -> Self {
        let source: Arc<dyn HintSource> = Arc::from(source);
        state.lock().unwrap_or_else(PoisonError::into_inner).source = Some(Arc::clone(&source));
        Self {
            source,
            state,
            style,
            current: String::new(),
        }
    }

    /// A hinter that is not tied to an [`Editor`](crate::Editor), for the benchmark.
    #[must_use]
    pub fn standalone(store: Store, cwd: &str, style: nu_ansi_term::Style) -> (Self, HintReset) {
        let state = Arc::new(Mutex::new(HintState::default()));
        state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .reset(cwd.to_owned());
        let reset = HintReset {
            state: Arc::clone(&state),
            cwd: cwd.to_owned(),
        };
        (Self::new(Box::new(store), state, style), reset)
    }

    /// The command to suggest for `line`, or none. Asks the source only when the memory and
    /// the reuse rule cannot answer. Remembers an answer only if the history was up to date
    /// when it was given (see the module comment).
    fn best(&self, line: &str) -> Option<String> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(known) = state.answers.get(line) {
            return known.clone();
        }
        let reused = match &state.previous {
            Some((before, answer)) if line.starts_with(before.as_str()) => match answer {
                None => Some(None),
                Some(best) if best.len() > line.len() && best.starts_with(line) => {
                    Some(Some(best.clone()))
                }
                Some(_) => None,
            },
            _ => None,
        };
        let answer = match reused {
            Some(answer) => answer,
            None => {
                // The list for this prompt, if the helper has made it: no question needed.
                if let Some(told) = state.ranking.as_ref().and_then(|r| r.best(line)) {
                    let answer = told.map(str::to_owned);
                    state.answers.insert(line.to_owned(), answer.clone());
                    state.previous = Some((line.to_owned(), answer.clone()));
                    return answer;
                }
                if !state.settled {
                    let limit = if state.waited { RETRY_WAIT } else { FIRST_WAIT };
                    state.waited = true;
                    state.settled = self.source.settle(limit);
                }
                // The history cannot be read now: nothing to show, nothing to remember.
                let Ok(answer) = self.source.best(line, &state.cwd) else {
                    return None;
                };
                if !state.settled {
                    // The writer is behind: what was found (or not found) may be out of date.
                    return answer;
                }
                answer
            }
        };
        state.answers.insert(line.to_owned(), answer.clone());
        state.previous = Some((line.to_owned(), answer.clone()));
        answer
    }
}

/// One prompt's work for the helper thread: whose ranking to make (`generation` is the prompt)
/// and for which folder.
struct Job {
    cwd: String,
    generation: u64,
}

/// Has the ranking made for the prompt that has just been reset. Nothing happens if the hinter
/// is off (no source) or the helper thread cannot be made: the questions are then asked one by
/// one.
///
/// **One thread for the life of the editor**, not one for each prompt. A thread that is new at
/// each prompt gets a memory area (a malloc arena) of its own from the allocator, and whether
/// the next prompt's thread reuses the area of the one before or opens another depends on
/// whether that one has ended yet: the shell's idle memory moved by about 1 MB from run to run
/// (S28b). One thread always uses the same area.
pub(crate) fn start_ranking(state: &Arc<Mutex<HintState>>) {
    let mut locked = state.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(source) = locked.source.clone() else {
        return;
    };
    let job = Job {
        cwd: locked.cwd.clone(),
        generation: locked.generation,
    };
    // The thread is there: give it the work. A send fails only if the thread is gone (it
    // panicked); then a new one is started below.
    let job = match &locked.worker {
        Some(worker) => match worker.send(job) {
            Ok(()) => return,
            Err(refused) => refused.0,
        },
        None => job,
    };
    let (sender, jobs) = mpsc::channel();
    let weak = Arc::downgrade(state);
    let spawned = std::thread::Builder::new()
        .name("hint-ranking".into())
        .spawn(move || rank_for_prompts(&source, &weak, &jobs));
    if spawned.is_ok() && sender.send(job).is_ok() {
        locked.worker = Some(sender);
    }
}

/// The helper thread: makes the ranking of each prompt it is told about, and hands it to the
/// state if that prompt is still the current one. Ends when the state is gone.
fn rank_for_prompts(
    source: &Arc<dyn HintSource>,
    state: &Weak<Mutex<HintState>>,
    jobs: &Receiver<Job>,
) {
    while let Ok(mut job) = jobs.recv() {
        // Prompts that came while the last ranking was being made are over; only the newest
        // one needs a list.
        while let Ok(newer) = jobs.try_recv() {
            job = newer;
        }
        let Some(ranking) = source.ranking(&job.cwd) else {
            continue;
        };
        let Some(state) = state.upgrade() else {
            return;
        };
        let mut locked = state.lock().unwrap_or_else(PoisonError::into_inner);
        // The prompt may be over: its answers belong to it alone.
        if locked.generation == job.generation {
            locked.ranking = Some(Arc::new(ranking));
            // The writer had caught up when the list was made.
            locked.settled = true;
        }
    }
}

/// Starts a new prompt for a [`ShellHinter::standalone`] (the benchmark's worst case).
pub struct HintReset {
    state: Arc<Mutex<HintState>>,
    cwd: String,
}

impl HintReset {
    /// Forgets every answer, as a new prompt does.
    pub fn new_prompt(&self) {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .reset(self.cwd.clone());
    }
}

impl Hinter for ShellHinter {
    fn handle(
        &mut self,
        line: &str,
        _pos: usize,
        _history: &dyn History,
        use_ansi_coloring: bool,
        _cwd: &str,
    ) -> String {
        // Nothing for an empty line, a line of several lines (a hint is one line), or a request
        // to the AI (natural language is not in the history as a command).
        self.current = if line.is_empty() || line.contains('\n') || trigger(line).is_some() {
            String::new()
        } else {
            self.best(line)
                .and_then(|best| best.get(line.len()..).map(str::to_owned))
                .unwrap_or_default()
        };
        if use_ansi_coloring && !self.current.is_empty() {
            self.style.paint(&self.current).to_string()
        } else {
            self.current.clone()
        }
    }

    fn complete_hint(&self) -> String {
        self.current.clone()
    }

    /// The first word of the hint, with the blanks in front of it.
    fn next_hint_token(&self) -> String {
        let text = self.current.as_str();
        let blanks = text.len() - text.trim_start().len();
        let word = text[blanks..]
            .find(char::is_whitespace)
            .map_or(text.len(), |end| blanks + end);
        text[..word].to_owned()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use nu_ansi_term::{Color, Style};

    use super::*;

    /// Answers from a list and counts the questions.
    struct Fake {
        commands: Vec<&'static str>,
        asked: Arc<AtomicUsize>,
        settled: Arc<AtomicUsize>,
        /// The list the helper thread gets, once `gate` is open (none: the source has no list).
        ranking: Option<Ranking>,
        /// The helper thread waits here until the test opens it.
        gate: Option<Arc<(Mutex<bool>, std::sync::Condvar)>>,
    }

    impl HintSource for Fake {
        fn settle(&self, _limit: Duration) -> bool {
            self.settled.fetch_add(1, Ordering::Relaxed);
            true
        }

        fn best(&self, prefix: &str, _cwd: &str) -> Result<Option<String>, NotReady> {
            self.asked.fetch_add(1, Ordering::Relaxed);
            Ok(self
                .commands
                .iter()
                .find(|command| command.starts_with(prefix) && command.len() > prefix.len())
                .map(|command| (*command).to_owned()))
        }

        fn ranking(&self, _cwd: &str) -> Option<Ranking> {
            if let Some(gate) = &self.gate {
                let (open, bell) = &**gate;
                let mut open = open.lock().unwrap();
                while !*open {
                    open = bell.wait(open).unwrap();
                }
            }
            self.ranking.clone()
        }
    }

    struct Rig {
        hinter: ShellHinter,
        state: Arc<Mutex<HintState>>,
        asked: Arc<AtomicUsize>,
        settled: Arc<AtomicUsize>,
    }

    fn rig(commands: &[&'static str], style: Style) -> Rig {
        let asked = Arc::new(AtomicUsize::new(0));
        let settled = Arc::new(AtomicUsize::new(0));
        let state = Arc::new(Mutex::new(HintState::default()));
        let source = Fake {
            commands: commands.to_vec(),
            asked: Arc::clone(&asked),
            settled: Arc::clone(&settled),
            ranking: None,
            gate: None,
        };
        Rig {
            hinter: ShellHinter::new(Box::new(source), Arc::clone(&state), style),
            state,
            asked,
            settled,
        }
    }

    impl Rig {
        fn hint(&mut self, line: &str) -> String {
            let history = reedline::FileBackedHistory::default();
            self.hinter.handle(line, line.len(), &history, false, "/")
        }

        fn asked(&self) -> usize {
            self.asked.load(Ordering::Relaxed)
        }
    }

    #[test]
    fn the_hint_is_the_rest_of_the_command() {
        let mut rig = rig(&["git status --short"], Style::new());
        assert_eq!(rig.hint("git st"), "atus --short");
        assert_eq!(rig.hinter.complete_hint(), "atus --short");
        assert_eq!(rig.hint("zzz"), "");
        assert_eq!(rig.hinter.complete_hint(), "");
    }

    #[test]
    fn there_is_no_hint_for_an_empty_line_several_lines_or_a_request_to_the_ai() {
        let mut rig = rig(
            &["git status", "? list files", "echo a\necho b"],
            Style::new(),
        );
        assert_eq!(rig.hint(""), "");
        assert_eq!(rig.hint("echo a\necho"), "");
        assert_eq!(rig.hint("? l"), "");
        assert_eq!(rig.hint("?? g"), "");
        assert_eq!(rig.asked(), 0, "none of them asks the history");
    }

    #[test]
    fn typing_on_along_the_hint_asks_once() {
        let mut rig = rig(&["cargo build --release"], Style::new());
        for end in 1..="cargo build".len() {
            let line = &"cargo build"[..end];
            assert_eq!(rig.hint(line), &"cargo build --release"[end..]);
        }
        assert_eq!(rig.asked(), 1);
        assert_eq!(rig.settled.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn a_backspace_is_answered_from_memory() {
        let mut rig = rig(&["make test", "mkdir x"], Style::new());
        assert_eq!(rig.hint("ma"), "ke test");
        assert_eq!(rig.hint("m"), "ake test");
        let asked = rig.asked();
        assert_eq!(rig.hint("ma"), "ke test");
        assert_eq!(rig.hint("m"), "ake test");
        assert_eq!(rig.asked(), asked);
    }

    #[test]
    fn a_line_that_goes_another_way_than_the_answer_asks_again() {
        let mut rig = rig(&["make test", "mkdir x"], Style::new());
        assert_eq!(rig.hint("m"), "ake test");
        // "mk" no longer starts like the answer: the history is asked.
        assert_eq!(rig.hint("mk"), "dir x");
        assert_eq!(rig.asked(), 2);
    }

    #[test]
    fn after_a_line_with_no_hint_a_longer_one_has_none_and_nothing_is_asked() {
        let mut rig = rig(&["ls -la"], Style::new());
        assert_eq!(rig.hint("cd"), "");
        assert_eq!(rig.hint("cd /tmp"), "");
        assert_eq!(rig.asked(), 1);
    }

    #[test]
    fn when_the_whole_command_is_typed_there_is_nothing_to_add() {
        let mut rig = rig(&["ls -la", "ls -la /tmp"], Style::new());
        assert_eq!(rig.hint("ls -l"), "a");
        // The answer is used up; the history is asked for something longer.
        assert_eq!(rig.hint("ls -la"), " /tmp");
        assert_eq!(rig.asked(), 2);
    }

    #[test]
    fn a_new_prompt_forgets_everything() {
        let mut rig = rig(&["git status"], Style::new());
        assert_eq!(rig.hint("git s"), "tatus");
        rig.state.lock().unwrap().reset("/other".to_owned());
        assert_eq!(rig.hint("git s"), "tatus");
        assert_eq!(rig.asked(), 2);
        assert_eq!(rig.settled.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn the_hint_is_drawn_in_its_style_with_color_and_plain_without() {
        let style = Style::new().fg(Color::DarkGray);
        let mut rig = rig(&["git status"], style);
        let history = reedline::FileBackedHistory::default();
        let drawn = rig.hinter.handle("git s", 5, &history, true, "/");
        assert_eq!(drawn, style.paint("tatus").to_string());
        assert_ne!(drawn, "tatus");
        // The text that Right brings in has no escape codes.
        assert_eq!(rig.hinter.complete_hint(), "tatus");
        let drawn = rig.hinter.handle("git s", 5, &history, false, "/");
        assert_eq!(drawn, "tatus");
    }

    #[test]
    fn alt_f_brings_one_word() {
        let mut rig = rig(&["git commit -m fix"], Style::new());
        rig.hint("git ");
        assert_eq!(rig.hinter.next_hint_token(), "commit");
        rig.hint("git commit");
        assert_eq!(rig.hinter.next_hint_token(), " -m");
        rig.hint("git commit -m ");
        assert_eq!(rig.hinter.next_hint_token(), "fix");
        rig.hint("zzz");
        assert_eq!(rig.hinter.next_hint_token(), "");
    }

    /// A history that can be slow or unreadable, controlled by the test, like a writer that is
    /// held up by a slow disk or by another shell. It records how long each wait was allowed.
    #[derive(Clone, Default)]
    struct Slow {
        /// The writer has caught up: `settle` says true.
        caught_up: Arc<AtomicBool>,
        /// The last command has reached the database. Before that, a question finds nothing.
        stored: Arc<AtomicBool>,
        /// The database can be read. Before that, a question is "not ready".
        readable: Arc<AtomicBool>,
        limits: Arc<Mutex<Vec<Duration>>>,
        asked: Arc<AtomicUsize>,
    }

    const STORED: &str = "echo hint-$((1+1))";

    impl Slow {
        fn set(flag: &AtomicBool, value: bool) {
            flag.store(value, Ordering::Relaxed);
        }

        /// A hinter over this history, and a handle to start a new prompt.
        fn hinter(&self) -> (ShellHinter, Arc<Mutex<HintState>>) {
            let state = Arc::new(Mutex::new(HintState::default()));
            let hinter = ShellHinter::new(Box::new(self.clone()), Arc::clone(&state), Style::new());
            (hinter, state)
        }

        fn limits(&self) -> Vec<Duration> {
            self.limits.lock().unwrap().clone()
        }
    }

    impl HintSource for Slow {
        fn settle(&self, limit: Duration) -> bool {
            self.limits.lock().unwrap().push(limit);
            self.caught_up.load(Ordering::Relaxed)
        }

        fn best(&self, prefix: &str, _cwd: &str) -> Result<Option<String>, NotReady> {
            self.asked.fetch_add(1, Ordering::Relaxed);
            if !self.readable.load(Ordering::Relaxed) {
                return Err(NotReady);
            }
            let found = self.stored.load(Ordering::Relaxed)
                && STORED.starts_with(prefix)
                && STORED.len() > prefix.len();
            Ok(found.then(|| STORED.to_owned()))
        }
    }

    fn typed(hinter: &mut ShellHinter, line: &str) -> String {
        let history = reedline::FileBackedHistory::default();
        hinter.handle(line, line.len(), &history, false, "/")
    }

    /// The CI failure of two S14b PTY tests: the first question of a prompt came while the writer
    /// had not stored the last command, the answer "nothing" was remembered, and the hint never
    /// showed at that prompt, however much more was typed.
    #[test]
    fn nothing_found_while_the_writer_was_behind_is_not_kept_for_the_prompt() {
        let slow = Slow::default();
        Slow::set(&slow.readable, true);
        let (mut hinter, _) = slow.hinter();
        assert_eq!(typed(&mut hinter, "echo h"), "", "the writer is behind");
        // The writer catches up and the command is stored.
        Slow::set(&slow.stored, true);
        Slow::set(&slow.caught_up, true);
        assert_eq!(typed(&mut hinter, "echo hi"), "nt-$((1+1))");
        // The same line again, and a longer one, are now answered and remembered.
        let asked = slow.asked.load(Ordering::Relaxed);
        assert_eq!(typed(&mut hinter, "echo hi"), "nt-$((1+1))");
        assert_eq!(typed(&mut hinter, "echo hin"), "t-$((1+1))");
        assert_eq!(slow.asked.load(Ordering::Relaxed), asked);
    }

    #[test]
    fn a_history_that_cannot_be_read_gives_no_hint_and_is_asked_again() {
        let slow = Slow::default();
        Slow::set(&slow.caught_up, true);
        Slow::set(&slow.stored, true);
        let (mut hinter, _) = slow.hinter();
        assert_eq!(typed(&mut hinter, "echo h"), "", "not readable yet");
        assert_eq!(typed(&mut hinter, "echo h"), "", "still not readable");
        assert_eq!(slow.asked.load(Ordering::Relaxed), 2, "asked each time");
        Slow::set(&slow.readable, true);
        assert_eq!(typed(&mut hinter, "echo h"), "int-$((1+1))");
    }

    #[test]
    fn an_answer_given_while_the_writer_is_behind_is_shown_but_not_kept_or_reused() {
        let slow = Slow::default();
        Slow::set(&slow.readable, true);
        Slow::set(&slow.stored, true);
        let (mut hinter, _) = slow.hinter();
        // The command is already readable, but the writer has not said it is up to date.
        assert_eq!(typed(&mut hinter, "echo h"), "int-$((1+1))");
        assert_eq!(slow.asked.load(Ordering::Relaxed), 1);
        // Neither the memory nor the reuse rule answer the next questions.
        assert_eq!(typed(&mut hinter, "echo h"), "int-$((1+1))");
        assert_eq!(typed(&mut hinter, "echo hi"), "nt-$((1+1))");
        assert_eq!(slow.asked.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn the_first_wait_is_long_the_later_ones_are_short_and_they_stop_once_the_writer_caught_up() {
        let slow = Slow::default();
        Slow::set(&slow.readable, true);
        let (mut hinter, state) = slow.hinter();
        typed(&mut hinter, "e");
        typed(&mut hinter, "ec");
        typed(&mut hinter, "ech");
        assert_eq!(slow.limits(), [FIRST_WAIT, RETRY_WAIT, RETRY_WAIT]);
        Slow::set(&slow.caught_up, true);
        typed(&mut hinter, "echo");
        // Lines that the reuse rule cannot answer: each is a new question, and none waits.
        typed(&mut hinter, "ls");
        typed(&mut hinter, "pwd");
        assert_eq!(slow.asked.load(Ordering::Relaxed), 6);
        assert_eq!(
            slow.limits(),
            [FIRST_WAIT, RETRY_WAIT, RETRY_WAIT, RETRY_WAIT],
            "no wait after the writer caught up"
        );
        // A new prompt starts again with the long wait.
        state.lock().unwrap().reset("/".to_owned());
        Slow::set(&slow.caught_up, false);
        typed(&mut hinter, "e");
        assert_eq!(slow.limits().last(), Some(&FIRST_WAIT));
    }

    #[test]
    fn the_real_store_gives_the_hint() {
        let store = Store::in_memory();
        let token = store.start("git status", "/w");
        store.finish(token, 0, Duration::ZERO);
        assert!(store.settle(Duration::from_secs(10)));
        let (mut hinter, reset) = ShellHinter::standalone(store, "/w", Style::new());
        reset.new_prompt();
        let history = reedline::FileBackedHistory::default();
        assert_eq!(hinter.handle("git s", 5, &history, false, "/"), "tatus");
    }
}

#[cfg(test)]
mod ranking_tests {
    use std::sync::Condvar;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    use nu_ansi_term::Style;

    use super::*;

    type Gate = Arc<(Mutex<bool>, Condvar)>;

    /// A source that counts its questions and hands out a list after a gate is opened.
    struct Listed {
        asked: Arc<AtomicUsize>,
        ranking: Option<Ranking>,
        gate: Gate,
        /// The thread of each call of `ranking`, in order, noted when the call starts.
        threads: Arc<Mutex<Vec<std::thread::ThreadId>>>,
        /// Held by the source only: its count tells when the source has been dropped.
        alive: Arc<()>,
    }

    impl HintSource for Listed {
        fn settle(&self, _limit: Duration) -> bool {
            true
        }

        fn best(&self, prefix: &str, _cwd: &str) -> Result<Option<String>, NotReady> {
            self.asked.fetch_add(1, Ordering::Relaxed);
            // The database would say this; the list says the same for "git" and "ls".
            Ok(["git status", "ls -la"]
                .iter()
                .find(|c| c.starts_with(prefix) && c.len() > prefix.len())
                .map(|c| (*c).to_owned()))
        }

        fn ranking(&self, _cwd: &str) -> Option<Ranking> {
            let _ = &self.alive;
            self.threads
                .lock()
                .unwrap()
                .push(std::thread::current().id());
            let (open, bell) = &*self.gate;
            let mut open = open.lock().unwrap();
            while !*open {
                open = bell.wait(open).unwrap();
            }
            self.ranking.clone()
        }
    }

    struct Rig {
        hinter: ShellHinter,
        state: Arc<Mutex<HintState>>,
        asked: Arc<AtomicUsize>,
        gate: Gate,
        threads: Arc<Mutex<Vec<std::thread::ThreadId>>>,
        alive: Arc<()>,
    }

    fn rig(ranking: Option<Ranking>) -> Rig {
        let asked = Arc::new(AtomicUsize::new(0));
        let gate: Gate = Arc::new((Mutex::new(false), Condvar::new()));
        let state = Arc::new(Mutex::new(HintState::default()));
        let threads = Arc::new(Mutex::new(Vec::new()));
        let alive = Arc::new(());
        let source = Listed {
            asked: Arc::clone(&asked),
            ranking,
            gate: Arc::clone(&gate),
            threads: Arc::clone(&threads),
            alive: Arc::clone(&alive),
        };
        let hinter = ShellHinter::new(Box::new(source), Arc::clone(&state), Style::new());
        Rig {
            hinter,
            state,
            asked,
            gate,
            threads,
            alive,
        }
    }

    fn list(commands: &[&str], complete: bool) -> Option<Ranking> {
        Some(Ranking::from_commands(
            commands.iter().map(|c| (*c).to_owned()).collect(),
            complete,
        ))
    }

    impl Rig {
        fn new_prompt(&self) {
            self.state.lock().unwrap().reset("/".to_owned());
            start_ranking(&self.state);
        }

        fn open_gate(&self) {
            let (open, bell) = &*self.gate;
            *open.lock().unwrap() = true;
            bell.notify_all();
        }

        fn wait_for_list(&self) {
            let end = Instant::now() + Duration::from_secs(5);
            while self.state.lock().unwrap().ranking.is_none() {
                assert!(Instant::now() < end, "the list never came");
                std::thread::sleep(Duration::from_millis(2));
            }
        }

        fn hint(&mut self, line: &str) -> String {
            let history = reedline::FileBackedHistory::default();
            self.hinter.handle(line, line.len(), &history, false, "/")
        }
    }

    #[test]
    fn once_the_list_is_there_the_first_keys_ask_nobody() {
        let mut rig = rig(list(&["git status", "ls -la"], true));
        rig.new_prompt();
        rig.open_gate();
        rig.wait_for_list();
        assert_eq!(rig.hint("g"), "it status");
        assert_eq!(rig.hint("l"), "s -la");
        assert_eq!(rig.hint("x"), "", "a complete list can say nothing");
        assert_eq!(rig.asked.load(Ordering::Relaxed), 0);
        // It also counts as an up-to-date answer: remembered for a backspace.
        assert!(rig.state.lock().unwrap().settled);
        assert_eq!(rig.hint("g"), "it status");
        assert_eq!(rig.asked.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn a_key_typed_before_the_list_is_ready_is_answered_by_the_question() {
        let mut rig = rig(list(&["git status", "ls -la"], true));
        rig.new_prompt();
        assert_eq!(rig.hint("g"), "it status");
        assert_eq!(rig.asked.load(Ordering::Relaxed), 1);
        // Later the list arrives and takes over for texts not asked yet.
        rig.open_gate();
        rig.wait_for_list();
        assert_eq!(rig.hint("l"), "s -la");
        assert_eq!(rig.asked.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn a_list_made_for_an_earlier_prompt_is_not_used_at_the_next() {
        let mut rig = rig(list(&["git status", "ls -la"], true));
        rig.new_prompt(); // prompt 1: the helper waits at the gate
        rig.state.lock().unwrap().reset("/".to_owned()); // prompt 2 starts, no helper for it
        rig.open_gate();
        // Give the old helper time to finish and (wrongly) publish.
        std::thread::sleep(Duration::from_millis(100));
        assert!(rig.state.lock().unwrap().ranking.is_none());
        assert_eq!(rig.hint("g"), "it status");
        assert_eq!(rig.asked.load(Ordering::Relaxed), 1, "asked, not listed");
    }

    #[test]
    fn a_source_with_no_list_is_asked_as_before() {
        let mut rig = rig(None);
        rig.new_prompt();
        rig.open_gate();
        std::thread::sleep(Duration::from_millis(50));
        assert!(rig.state.lock().unwrap().ranking.is_none());
        assert_eq!(rig.hint("g"), "it status");
        assert_eq!(rig.asked.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn a_cut_list_that_cannot_tell_hands_the_question_to_the_source() {
        let mut rig = rig(list(&["ls -la"], false));
        rig.new_prompt();
        rig.open_gate();
        rig.wait_for_list();
        // The list holds a match for "l": no question.
        assert_eq!(rig.hint("l"), "s -la");
        assert_eq!(rig.asked.load(Ordering::Relaxed), 0);
        // It holds none for "g" and was cut, so it cannot say "nothing": the source is asked.
        assert_eq!(rig.hint("g"), "it status");
        assert_eq!(rig.asked.load(Ordering::Relaxed), 1);
    }

    impl Rig {
        fn calls(&self) -> usize {
            self.threads.lock().unwrap().len()
        }

        fn wait_until(&self, what: &str, done: impl Fn(&Rig) -> bool) {
            let end = Instant::now() + Duration::from_secs(5);
            while !done(self) {
                assert!(Instant::now() < end, "{what}");
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }

    /// The point of the change: a thread that is new at each prompt gets an allocator area of its
    /// own, and the shell's idle memory moved by about 1 MB with it (S28b).
    #[test]
    fn one_thread_makes_the_list_of_every_prompt() {
        let rig = rig(list(&["git status"], true));
        rig.open_gate();
        for prompt in 1..=4 {
            rig.new_prompt();
            rig.wait_for_list();
            assert_eq!(rig.calls(), prompt, "one call for each prompt");
        }
        let threads = rig.threads.lock().unwrap().clone();
        assert!(
            threads.iter().all(|id| *id == threads[0]),
            "the lists were made by {} different threads",
            threads
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
        );
        assert_ne!(threads[0], std::thread::current().id());
    }

    #[test]
    fn prompts_that_came_while_a_list_was_being_made_are_skipped_but_the_newest_is_made() {
        let rig = rig(list(&["git status"], true));
        rig.new_prompt(); // prompt 1: the thread takes it and waits at the gate
        rig.wait_until("the thread never took the first prompt", |r| r.calls() == 1);
        rig.new_prompt(); // prompts 2 and 3 wait in the queue
        rig.new_prompt();
        rig.open_gate();
        rig.wait_for_list();
        // The list is the one of the newest prompt; the middle one was never made.
        assert_eq!(rig.calls(), 2);
        assert_eq!(rig.hint_state_generation(), 3);
        let mut rig = rig;
        assert_eq!(rig.hint("g"), "it status");
        assert_eq!(rig.asked.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn the_thread_ends_when_the_editor_is_gone() {
        let Rig {
            hinter,
            state,
            gate,
            alive,
            ..
        } = rig(list(&["git status"], true));
        {
            let (open, bell) = &*gate;
            *open.lock().unwrap() = true;
            bell.notify_all();
        }
        state.lock().unwrap().reset("/".to_owned());
        start_ranking(&state);
        let end = Instant::now() + Duration::from_secs(5);
        while state.lock().unwrap().ranking.is_none() {
            assert!(Instant::now() < end, "the list never came");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(Arc::strong_count(&alive) > 1, "the source is in use");
        // The thread holds the source and the state only weakly: dropping the editor's side
        // lets it go, and with it the source.
        drop(hinter);
        drop(state);
        let end = Instant::now() + Duration::from_secs(5);
        while Arc::strong_count(&alive) > 1 {
            assert!(Instant::now() < end, "the thread was still running");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    impl Rig {
        fn hint_state_generation(&self) -> u64 {
            self.state.lock().unwrap().generation
        }
    }
}
