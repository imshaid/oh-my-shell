//! Line editing for Oh My Shell: the editor, the prompt and the rules for Enter.
//!
//! [`Editor`] wraps `reedline`. It is used only when a person types at a terminal that can take
//! escape codes (the shell chooses; see `omsh-cli`). It does not run anything: it returns what
//! was typed as a [`Read`], and the shell runs it.
//!
//! This crate does not depend on the executor. What it needs from the shell (the prompt facts,
//! and whether the terminal hung up) is passed in.
//!
//! **Why the editor runs on its own thread.** `reedline` reads the terminal through `crossterm`,
//! and `crossterm` treats end of input on a terminal as "try again", in a loop that never
//! looks at the clock. When a terminal is closed (a window is closed, an ssh session drops) the
//! reading thread would spin at full speed for ever. So [`Editor::read`] runs the editor on a
//! helper thread and waits for it in short steps; if the terminal is gone it gives up on that
//! thread and returns [`Read::HungUp`], so that the shell can end properly.

mod highlight;
mod prompt;
mod segments;
mod validator;

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use omsh_config::{Config, EditMode as ConfigMode};
use omsh_ui::{Role, Theme};
use reedline::{Emacs, FileBackedHistory, Reedline, Signal, Vi};

pub use highlight::{CommandLookup, ShellHighlighter};
pub use prompt::{Look, PromptState, ShellPrompt};
pub use segments::{Head, Segment, git_head, working_directory};
pub use validator::is_incomplete;

/// How many commands the up arrow can reach in one session (S13b replaces this with the
/// database).
const SESSION_HISTORY: usize = 1000;

/// How often a waiting shell looks at the hang-up flag and at the terminal. The same step as the
/// plain reader's.
const IDLE_STEP: Duration = Duration::from_millis(100);

/// Stack of the editor's thread. Checking whether a line is finished parses it, and the parser
/// may go 128 levels deep; the main thread's 8 MiB is what that was measured against.
const EDITOR_STACK: usize = 8 * 1024 * 1024;

/// `EIO`: what reading a terminal that has gone away returns.
const EIO: i32 = 5;

/// The result of asking for a line.
#[derive(Debug)]
pub enum Read {
    /// A command, finished (the validator said so). May hold several lines.
    Line(String),
    /// Ctrl+C: the half-typed line is dropped.
    Interrupted,
    /// Ctrl+D on an empty line.
    EndOfInput,
    /// The terminal hung up.
    HungUp,
    /// The editor could not work with this terminal, or stopped unexpectedly. The shell goes on
    /// with the plain reader.
    Failed(io::Error),
}

/// The line editor.
pub struct Editor {
    /// The engine is lent to the editor's thread while a line is read. It is gone for good after
    /// a hang-up or a failure of that thread.
    engine: Option<Reedline>,
    look: Look,
    /// What the highlighter knows about the system; renewed for each prompt.
    context: Arc<Mutex<highlight::Context>>,
    /// Asked while waiting: has the terminal gone away?
    terminal_gone: Box<dyn Fn() -> bool + Send>,
}

impl Editor {
    /// Builds the editor from the configuration. `unicode` and `color` say what the terminal
    /// takes. `theme` is the look of the command line; without `color` its colors are dropped
    /// and only the attributes (underline, dim, reverse) stay.
    ///
    /// Two questions are asked once in a while while the editor waits. `hang_up`: was the shell
    /// told to hang up (`SIGHUP`)? The editor then ends the line it is reading cleanly, which
    /// puts the terminal back in line mode. `terminal_gone`: is the terminal closed? Nothing can
    /// be restored then, so the editor is abandoned. Either way [`Editor::read`] returns
    /// [`Read::HungUp`].
    #[must_use]
    pub fn new(
        config: &Config,
        theme: &Theme,
        unicode: bool,
        color: bool,
        hang_up: impl Fn() -> bool + Send + 'static,
        terminal_gone: impl Fn() -> bool + Send + 'static,
    ) -> Self {
        let look = Look {
            unicode,
            color,
            left: names(&config.prompt.left),
            right: names(&config.prompt.right),
        };

        let theme = if color {
            theme.clone()
        } else {
            theme.without_color()
        };
        let context = Arc::new(Mutex::new(highlight::Context::new()));
        let flag = Arc::new(AtomicBool::new(false));
        let raise = Arc::clone(&flag);
        let capacity =
            SESSION_HISTORY.min(usize::try_from(config.history.size).unwrap_or(usize::MAX));
        let mut engine = Reedline::create()
            .with_highlighter(Box::new(highlight::ShellHighlighter::new(
                theme.clone(),
                Arc::clone(&context),
            )))
            .with_visual_selection_style(highlight::to_style(theme.get(Role::Selection)))
            .with_validator(Box::new(validator::ShellValidator))
            .with_break_signal(flag)
            .with_idle_callback(Box::new(move || {
                if hang_up() {
                    raise.store(true, Ordering::Relaxed);
                }
            }))
            .with_poll_interval(IDLE_STEP);
        // `new` only fails for a capacity that cannot be addressed; ours is at most 1000.
        if let Ok(history) = FileBackedHistory::new(capacity) {
            engine = engine.with_history(Box::new(history));
        }
        // The default rule `" *"` (a line that starts with a space is not remembered). S13b
        // reads the whole `[history] ignore` list.
        let ignores_space = config.history.ignore.iter().any(|pattern| pattern == " *");
        engine = engine.with_history_exclusion_prefix(ignores_space.then(|| " ".to_owned()));
        engine = match config.editor.mode {
            ConfigMode::Emacs => engine.with_edit_mode(Box::new(Emacs::default())),
            ConfigMode::Vi => engine.with_edit_mode(Box::new(Vi::default())),
        };
        if config.prompt.transient {
            let neutral = ShellPrompt::new(&look, &neutral_state(), segments::now).transient();
            engine = engine.with_transient_prompt(Box::new(neutral));
        }
        Self {
            engine: Some(engine),
            look,
            context,
            terminal_gone: Box::new(terminal_gone),
        }
    }

    /// Shows the prompt and waits for a command. `lookup` answers which command names exist, for
    /// the colors; what it and the file system said earlier is forgotten first.
    pub fn read(&mut self, state: &PromptState, lookup: Box<dyn CommandLookup>) -> Read {
        self.context
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .reset(lookup, state.cwd.clone(), state.home.clone());
        let prompt = ShellPrompt::new(&self.look, state, segments::now);
        self.read_with(prompt)
    }

    /// Waits for the rest of a command that the shell found unfinished after the editor let it
    /// through (an alias that opens an `if`, for example). Shows `> `.
    pub fn read_more(&mut self) -> Read {
        self.read_with(ShellPrompt::continuation())
    }

    fn read_with(&mut self, prompt: ShellPrompt) -> Read {
        let Some(mut engine) = self.engine.take() else {
            return Read::Failed(io::Error::other("the line editor is not available"));
        };
        let (send, receive) = mpsc::channel();
        let spawned = thread::Builder::new()
            .name("line-editor".to_owned())
            .stack_size(EDITOR_STACK)
            .spawn(move || {
                let answer = engine.read_line(&prompt);
                // The receiver is gone if the shell gave up on this thread.
                let _ = send.send((engine, answer));
            });
        if let Err(error) = spawned {
            return Read::Failed(error);
        }
        loop {
            match receive.recv_timeout(IDLE_STEP) {
                Ok((engine, answer)) => {
                    self.engine = Some(engine);
                    return classify(answer);
                }
                Err(RecvTimeoutError::Timeout) => {
                    if (self.terminal_gone)() {
                        // The thread may be spinning on the dead terminal; it ends with the
                        // process. The engine stays with it.
                        let _ = crossterm::terminal::disable_raw_mode();
                        return Read::HungUp;
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    // The thread ended without an answer: it panicked. The terminal may be
                    // in raw mode; the plain reader needs line mode.
                    let _ = crossterm::terminal::disable_raw_mode();
                    return Read::Failed(io::Error::other("the line editor stopped unexpectedly"));
                }
            }
        }
    }
}

/// What the editor's answer means to the shell.
fn classify(answer: io::Result<Signal>) -> Read {
    match answer {
        Ok(Signal::Success(line)) => Read::Line(line),
        Ok(Signal::CtrlD) => Read::EndOfInput,
        Ok(Signal::ExternalBreak(_)) => Read::HungUp,
        // `CtrlC`, and any signal that a later version of the editor adds.
        Ok(_) => Read::Interrupted,
        Err(error) if error.raw_os_error() == Some(EIO) => Read::HungUp,
        Err(error) => Read::Failed(error),
    }
}

fn names(configured: &[String]) -> Vec<Segment> {
    configured
        .iter()
        .filter_map(|name| Segment::from_name(name))
        .collect()
}

fn neutral_state() -> PromptState {
    PromptState {
        cwd: "/".into(),
        home: None,
        virtual_env: None,
        conda_env: None,
        last_status: 0,
        last_duration: None,
        root: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_names_become_segments_and_unknown_ones_are_dropped() {
        let configured = ["cwd".to_owned(), "nope".to_owned(), "git".to_owned()];
        assert_eq!(names(&configured), vec![Segment::Cwd, Segment::Git]);
    }

    #[test]
    fn the_editor_can_be_built_in_both_modes() {
        let mut config = Config::default();
        let _ = Editor::new(&config, &Theme::default(), true, true, || false, || false);
        config.editor.mode = ConfigMode::Vi;
        config.prompt.transient = true;
        let _ = Editor::new(&config, &Theme::default(), false, false, || false, || false);
    }

    #[test]
    fn the_editor_answers_are_mapped_for_the_shell() {
        assert!(matches!(
            classify(Ok(Signal::Success("ls".into()))),
            Read::Line(line) if line == "ls"
        ));
        assert!(matches!(classify(Ok(Signal::CtrlC)), Read::Interrupted));
        assert!(matches!(classify(Ok(Signal::CtrlD)), Read::EndOfInput));
        assert!(matches!(
            classify(Ok(Signal::ExternalBreak("half".into()))),
            Read::HungUp
        ));
        // A terminal that went away: reading it fails with EIO.
        assert!(matches!(
            classify(Err(io::Error::from_raw_os_error(EIO))),
            Read::HungUp
        ));
        assert!(matches!(
            classify(Err(io::Error::other("no"))),
            Read::Failed(_)
        ));
    }
}
