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

mod complete;
mod git_status;
mod highlight;
mod hint;
mod history;
mod paste;
mod prompt;
mod segments;
mod suggest;
mod validator;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use nu_ansi_term::Style;
use omsh_config::{Config, EditMode as ConfigMode};
use omsh_history::{Filter, Store};
use omsh_ui::{Role, Theme};
use reedline::{
    ColumnarMenu, EditCommand, Emacs, KeyCode, KeyModifiers, Keybindings, MenuBuilder, Reedline,
    ReedlineEvent, ReedlineMenu, Signal, Vi, default_emacs_keybindings,
    default_vi_insert_keybindings, default_vi_normal_keybindings, default_vi_visual_keybindings,
};

pub use complete::{Facts, Scene, ShellCompleter, fish_completion_directories};
pub use git_status::passed_variables;
pub use highlight::{CommandLookup, ShellHighlighter};
pub use hint::{HintReset, HintSource, ShellHinter};
pub use prompt::{Look, PromptState, ShellPrompt};
pub use segments::{Head, Segment, git_head, working_directory};
pub use suggest::suggest_command;
pub use validator::is_incomplete;

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
    /// `Alt+?`: the person asks why the last command failed. What was typed so far stays in
    /// the editor's buffer.
    ExplainLast,
    /// The terminal hung up.
    HungUp,
    /// The editor could not work with this terminal, or stopped unexpectedly. The shell goes on
    /// with the plain reader.
    Failed(io::Error),
}

/// What the editor can bring back: the history, and the rule for the lines that are not
/// stored in it (those can be recalled once, right after they ran).
#[derive(Clone, Debug)]
pub struct Recall {
    pub store: Store,
    pub filter: Filter,
}

/// The line editor.
pub struct Editor {
    /// The engine is lent to the editor's thread while a line is read. It is gone for good after
    /// a hang-up or a failure of that thread.
    engine: Option<Reedline>,
    look: Look,
    /// What the highlighter knows about the system; renewed for each prompt.
    context: Arc<Mutex<highlight::Context>>,
    /// What the hinter knows about the prompt; renewed for each prompt.
    hints: Arc<Mutex<hint::HintState>>,
    /// What the completer knows about the shell; renewed for each prompt.
    scene: Arc<Mutex<complete::Scene>>,
    /// Finds the branch and the dirty marker for each prompt; only when the prompt has a git
    /// segment.
    git: Option<git_status::GitWatch>,
    /// Asked while waiting: has the terminal gone away?
    terminal_gone: Box<dyn Fn() -> bool + Send>,
}

impl Editor {
    /// Builds the editor from the configuration. `unicode` and `color` say what the terminal
    /// takes. `theme` is the look of the command line; without `color` its colors are dropped
    /// and only the attributes (underline, dim, reverse) stay. `recall` is what the Up arrow
    /// and the history search show.
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
        recall: Recall,
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
        let mut facts = highlight::Context::new();
        facts.set_abbreviations(
            config
                .editor
                .abbreviations
                .keys()
                .cloned()
                .collect::<HashSet<_>>(),
        );
        let context = Arc::new(Mutex::new(facts));
        let hints = Arc::new(Mutex::new(hint::HintState::default()));
        let scene = Arc::new(Mutex::new(complete::Scene::default()));
        let flag = Arc::new(AtomicBool::new(false));
        let raise = Arc::clone(&flag);
        let hint_source = recall.store.clone();
        let history = history::DbHistory::new(recall.store, recall.filter);
        // With a session, Up shows this shell's commands and what was stored before it
        // started, not what another open shell runs meanwhile.
        let session = history.session_id();
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
            .with_poll_interval(IDLE_STEP)
            .with_history(Box::new(history))
            .with_history_session_id(Some(session))
            // A paste is inserted into the line, not run line by line (see `paste.rs`).
            .use_bracketed_paste(true)
            .with_abbreviations(abbreviations(&config.editor.abbreviations))
            // Tab: the common start of the choices goes into the line, and the choices are
            // listed (see `complete`). A single choice is taken at once.
            .with_completer(Box::new(ShellCompleter::new(Arc::clone(&scene))))
            .with_menu(ReedlineMenu::EngineCompleter(Box::new(completion_menu(
                &theme,
            ))))
            .with_quick_completions(true)
            .with_partial_completions(true);
        if config.editor.hints {
            let style = highlight::to_style(theme.get(Role::Autosuggestion));
            engine = engine.with_hinter(Box::new(ShellHinter::new(
                Box::new(hint_source),
                Arc::clone(&hints),
                style,
            )));
        }
        engine = match config.editor.mode {
            ConfigMode::Emacs => {
                let mut keys = default_emacs_keybindings();
                bind_explain(&mut keys);
                bind_completion(&mut keys);
                engine.with_edit_mode(Box::new(paste::PasteMode::new(Box::new(Emacs::new(keys)))))
            }
            ConfigMode::Vi => {
                let mut insert = default_vi_insert_keybindings();
                let mut normal = default_vi_normal_keybindings();
                bind_explain(&mut insert);
                bind_completion(&mut insert);
                let mut visual = default_vi_visual_keybindings();
                bind_explain(&mut normal);
                bind_explain(&mut visual);
                engine.with_edit_mode(Box::new(paste::PasteMode::new(Box::new(Vi::new(
                    insert, normal, visual,
                )))))
            }
        };
        if config.prompt.transient {
            let neutral = ShellPrompt::new(&look, &neutral_state(), segments::now).transient();
            engine = engine.with_transient_prompt(Box::new(neutral));
        }
        // The helper thread asks the editor for a repaint when it learns something after the
        // prompt was drawn.
        let git = look
            .left
            .iter()
            .chain(&look.right)
            .any(|segment| *segment == Segment::Git)
            .then(|| {
                let signal = engine.repaint_signal();
                git_status::GitWatch::new(Arc::new(move || signal.request_repaint()))
            });
        Self {
            engine: Some(engine),
            look,
            context,
            hints,
            scene,
            git,
            terminal_gone: Box::new(terminal_gone),
        }
    }

    /// Shows the prompt and waits for a command. `lookup` answers which command names exist, for
    /// the colors; what it and the file system said earlier is forgotten first. `facts` is what
    /// Tab completion offers (function and variable names, `PATH`).
    pub fn read(
        &mut self,
        state: &PromptState,
        lookup: Box<dyn CommandLookup>,
        facts: Facts,
    ) -> Read {
        self.scene
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .reset(facts, state.cwd.clone(), state.home.clone());
        self.context
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .reset(lookup, state.cwd.clone(), state.home.clone());
        // The folder as history stores it (text), so that "run here" matches.
        self.hints
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .reset(state.cwd.to_string_lossy().into_owned());
        let mut prompt = ShellPrompt::new(&self.look, state, segments::now);
        if let Some(watch) = &self.git {
            // Started now, read at every paint; the wait is short and bounded, so that a slow
            // `git` never delays the prompt.
            let handle = watch.start(&state.cwd, &state.git_env);
            watch.wait(&handle, git_status::FIRST_PAINT_WAIT);
            prompt = prompt.with_git(handle);
        }
        self.read_with(prompt)
    }

    /// Waits for the rest of a command that the shell found unfinished after the editor let it
    /// through (an alias that opens an `if`, for example). Shows `> `.
    pub fn read_more(&mut self) -> Read {
        self.read_with(ShellPrompt::continuation())
    }

    /// Asks for a new version of `text` (the command of a plan step): the line editor opens
    /// with `text` already in the buffer, under an `edit> ` prompt. No history is added.
    pub fn edit(&mut self, text: &str) -> Read {
        self.read_prefilled(ShellPrompt::labeled("edit"), Some(text.to_owned()))
    }

    fn read_with(&mut self, prompt: ShellPrompt) -> Read {
        self.read_prefilled(prompt, None)
    }

    fn read_prefilled(&mut self, prompt: ShellPrompt, initial: Option<String>) -> Read {
        let Some(mut engine) = self.engine.take() else {
            return Read::Failed(io::Error::other("the line editor is not available"));
        };
        let (send, receive) = mpsc::channel();
        let spawned = thread::Builder::new()
            .name("line-editor".to_owned())
            .stack_size(EDITOR_STACK)
            .spawn(move || {
                if let Some(text) = initial {
                    engine.run_edit_commands(&[EditCommand::InsertString(text)]);
                }
                let answer = engine.read_line(&prompt);
                // A line that is not stored is reported to the editor as a failed save (see
                // `history.rs`); nobody needs to hear of it.
                let _ = engine.take_history_save_error();
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

/// The name of the menu that Tab opens.
const COMPLETION_MENU: &str = "completion_menu";

/// The menu of choices: names in columns, with a description beside a name that has one. The
/// chosen one is shown with the `selection` role of the theme (reverse video), the description is
/// dim; no other color is set.
fn completion_menu(theme: &Theme) -> ColumnarMenu {
    let selected = highlight::to_style(theme.get(Role::Selection));
    ColumnarMenu::default()
        .with_name(COMPLETION_MENU)
        .with_text_style(Style::default())
        .with_selected_text_style(selected)
        .with_description_text_style(Style::new().dimmed())
        .with_match_text_style(Style::default())
        .with_selected_match_text_style(selected)
}

/// Binds Tab to the completion menu (Tab again moves to the next choice) and Shift+Tab to the
/// previous choice.
fn bind_completion(keys: &mut Keybindings) {
    keys.add_binding(
        KeyModifiers::NONE,
        KeyCode::Tab,
        ReedlineEvent::UntilFound(vec![
            ReedlineEvent::Menu(COMPLETION_MENU.to_owned()),
            ReedlineEvent::MenuNext,
        ]),
    );
    keys.add_binding(
        KeyModifiers::SHIFT,
        KeyCode::BackTab,
        ReedlineEvent::MenuPrevious,
    );
}

/// The name the editor gives to `Alt+?` when it hands control back to the shell.
const EXPLAIN_LAST: &str = "omsh:explain-last";

/// Binds `Alt+?` (with and without Shift: terminals differ) to "explain the last failure".
fn bind_explain(keys: &mut Keybindings) {
    for modifiers in [KeyModifiers::ALT, KeyModifiers::ALT | KeyModifiers::SHIFT] {
        keys.add_binding(
            modifiers,
            KeyCode::Char('?'),
            ReedlineEvent::ExecuteHostCommand(EXPLAIN_LAST.to_owned()),
        );
    }
}

/// What the editor's answer means to the shell.
fn classify(answer: io::Result<Signal>) -> Read {
    match answer {
        Ok(Signal::HostCommand(name)) if name == EXPLAIN_LAST => Read::ExplainLast,
        Ok(Signal::Success(line)) => Read::Line(line),
        Ok(Signal::CtrlD) => Read::EndOfInput,
        Ok(Signal::ExternalBreak(_)) => Read::HungUp,
        // `CtrlC`, and any signal that a later version of the editor adds.
        Ok(_) => Read::Interrupted,
        Err(error) if error.raw_os_error() == Some(EIO) => Read::HungUp,
        Err(error) => Read::Failed(error),
    }
}

fn abbreviations(configured: &BTreeMap<String, String>) -> HashMap<String, String> {
    configured
        .iter()
        .map(|(name, text)| (name.clone(), text.clone()))
        .collect()
}

fn names(configured: &[String]) -> Vec<Segment> {
    configured
        .iter()
        .filter_map(|name| Segment::from_name(name))
        .collect()
}

/// The state the collapsed (transient) prompt is built from: it shows only the symbol, which is
/// `#` for root, as in the live prompt.
fn neutral_state() -> PromptState {
    PromptState {
        cwd: "/".into(),
        home: None,
        virtual_env: None,
        conda_env: None,
        last_status: 0,
        last_duration: None,
        root: nix::unistd::geteuid().is_root(),
        git_env: Vec::new(),
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
        let _ = Editor::new(
            &config,
            &Theme::default(),
            true,
            true,
            Recall {
                store: Store::in_memory(),
                filter: Filter::default(),
            },
            || false,
            || false,
        );
        config.editor.mode = ConfigMode::Vi;
        config.prompt.transient = true;
        let _ = Editor::new(
            &config,
            &Theme::default(),
            false,
            false,
            Recall {
                store: Store::in_memory(),
                filter: Filter::default(),
            },
            || false,
            || false,
        );
    }

    #[test]
    fn the_editor_answers_are_mapped_for_the_shell() {
        assert!(matches!(
            classify(Ok(Signal::Success("ls".into()))),
            Read::Line(line) if line == "ls"
        ));
        assert!(matches!(classify(Ok(Signal::CtrlC)), Read::Interrupted));
        assert!(matches!(
            classify(Ok(Signal::HostCommand(EXPLAIN_LAST.into()))),
            Read::ExplainLast
        ));
        // A host command we did not ask for is not Alt+?.
        assert!(matches!(
            classify(Ok(Signal::HostCommand("other".into()))),
            Read::Interrupted
        ));
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
