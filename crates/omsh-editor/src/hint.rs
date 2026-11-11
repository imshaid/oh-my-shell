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
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use omsh_history::{NotReady, Store, now_ms};
use omsh_syntax::trigger;
use reedline::{Hinter, History};

/// How long the first question of a prompt may wait for the history writer.
const FIRST_WAIT: Duration = Duration::from_millis(50);

/// How long each later question waits, while the writer has still not caught up. Short: this
/// is on the path of a keystroke, and a writer that is that far behind is held up for a while.
const RETRY_WAIT: Duration = Duration::from_millis(2);

/// Where hints come from. [`Store`] is the real one; tests use fakes.
pub trait HintSource: Send {
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
}

impl HintSource for Store {
    fn settle(&self, limit: Duration) -> bool {
        Store::settle(self, limit)
    }

    fn best(&self, prefix: &str, cwd: &str) -> Result<Option<String>, NotReady> {
        self.hint(prefix, cwd, now_ms())
    }
}

/// What the hinter knows about the prompt being typed at. Renewed by [`Editor::read`].
///
/// [`Editor::read`]: crate::Editor::read
#[derive(Default)]
pub(crate) struct HintState {
    cwd: String,
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
        self.settled = false;
        self.waited = false;
        self.answers.clear();
        self.previous = None;
    }
}

/// The hinter that `reedline` calls.
pub struct ShellHinter {
    source: Box<dyn HintSource>,
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
