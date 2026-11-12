//! The history explorer: a full-screen list of the commands that were run, with a fuzzy search.
//! `model` holds the state and the rules, `render` draws it, and [`run`] is the loop that owns
//! the terminal while the view is open.
//!
//! The view never writes anything but its own screen: no history entry, no variable. It gives a
//! command back, and the caller decides what to do with it (the line editor puts it in the
//! line, it does not run it).

pub mod model;
pub mod render;

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use crossterm::event::{self, Event};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

pub use model::{Action, Item, Model, Scope};
pub use render::{Context, Styles};

use crate::look::Look;
use crate::screen::FullScreen;
use crate::theme::Theme;

/// How long to wait for a key before asking whether the shell was told to hang up.
const STEP: Duration = Duration::from_millis(100);

/// What the view needs to open.
pub struct Settings<'a> {
    pub items: Vec<Item>,
    /// The history could not be read: the empty list says so instead of "no commands".
    pub unavailable: bool,
    /// How many of the newest runs `items` was made from (shown in the note).
    pub window: usize,
    /// The folder the shell is in, for the "this folder" scope.
    pub folder: String,
    pub now_ms: i64,
    /// The text of the line, which is the first query.
    pub typed: &'a str,
    pub theme: &'a Theme,
    pub look: Look,
}

/// How the view ended.
#[derive(Debug)]
pub enum Outcome {
    /// The person chose this command.
    Picked(String),
    /// Escape or Ctrl+C: nothing was chosen.
    Cancelled,
    /// The shell was told to hang up, or the terminal went away.
    HungUp,
    /// The terminal could not be taken (not a terminal, or no raw mode); nothing was shown.
    Unavailable(io::Error),
}

/// How long a view that was told to stop gets to put the terminal back before it is given up
/// on.
const GRACE: Duration = Duration::from_millis(300);

/// Opens the view and runs it until the person picks a command or leaves. `should_stop` is asked
/// about every tenth of a second while no key comes: true ends the view with
/// [`Outcome::HungUp`].
///
/// **Why the keys are read on a thread of their own.** The input library takes the end of input
/// of a closed terminal for "try again", inside the very call that waits for a key, in a loop
/// that never looks at the clock. A view that read keys on the calling thread would spin at full
/// speed for ever when the terminal window is closed. So the keys are read on a helper thread and
/// this one waits for it in short steps, asking `should_stop`; if the terminal is gone the helper
/// is abandoned (it ends with the process), as the line editor's thread is.
pub fn run(settings: Settings<'_>, should_stop: &mut dyn FnMut() -> bool) -> Outcome {
    let guard = match FullScreen::enter() {
        Ok(guard) => guard,
        Err(error) => return Outcome::Unavailable(error),
    };
    let terminal = match Terminal::new(CrosstermBackend::new(io::stderr())) {
        Ok(terminal) => terminal,
        Err(error) => return Outcome::Unavailable(error),
    };
    let context = Context {
        styles: Styles::new(settings.theme, settings.look),
        window: settings.window,
        unavailable: settings.unavailable,
        now_ms: settings.now_ms,
    };
    let model = Model::new(settings.items, settings.folder, settings.typed);
    let stop = Arc::new(AtomicBool::new(false));
    let helper_stop = Arc::clone(&stop);
    let (send, receive) = mpsc::channel();
    let spawned = thread::Builder::new()
        .name("history-explorer".to_owned())
        .spawn(move || {
            let (mut terminal, mut model) = (terminal, model);
            let outcome = event_loop(&mut terminal, &mut model, &context, &helper_stop);
            // The terminal comes back to be put away before the screen is given back.
            let _ = send.send((outcome, terminal));
        });
    if let Err(error) = spawned {
        return Outcome::Unavailable(error);
    }
    loop {
        match receive.recv_timeout(STEP) {
            Ok((outcome, terminal)) => {
                drop(terminal);
                drop(guard);
                return outcome;
            }
            Err(RecvTimeoutError::Timeout) => {
                if should_stop() {
                    stop.store(true, Ordering::Relaxed);
                    // A helper that is not stuck on a dead terminal ends within a step.
                    if let Ok((_, terminal)) = receive.recv_timeout(GRACE) {
                        drop(terminal);
                    }
                    drop(guard);
                    return Outcome::HungUp;
                }
            }
            Err(RecvTimeoutError::Disconnected) => {
                // The helper panicked; the panic hook has given the screen back already.
                drop(guard);
                return Outcome::Unavailable(io::Error::other(
                    "the history view stopped unexpectedly",
                ));
            }
        }
    }
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stderr>>,
    model: &mut Model,
    context: &Context,
    stop: &AtomicBool,
) -> Outcome {
    let mut dirty = true;
    loop {
        if dirty {
            if terminal
                .draw(|frame| render::draw(frame, model, context))
                .is_err()
            {
                return Outcome::HungUp;
            }
            dirty = false;
        }
        if stop.load(Ordering::Relaxed) {
            return Outcome::HungUp;
        }
        match event::poll(STEP) {
            Ok(true) => {}
            Ok(false) => continue,
            Err(_) => return Outcome::HungUp,
        }
        // Everything that is ready is handled, in order, before the next drawing: keys that
        // arrive together (a paste that is not bracketed, a script) are not lost or reordered.
        loop {
            match event::read() {
                Err(_) => return Outcome::HungUp,
                Ok(Event::Key(key)) => match model.handle_key(key) {
                    Action::Pick(command) => return Outcome::Picked(command),
                    Action::Cancel => return Outcome::Cancelled,
                    Action::None => dirty = true,
                },
                Ok(Event::Paste(text)) => {
                    model.paste(&text);
                    dirty = true;
                }
                Ok(Event::Resize(..)) => dirty = true,
                Ok(_) => {}
            }
            match event::poll(Duration::ZERO) {
                Ok(true) => {}
                Ok(false) => break,
                Err(_) => return Outcome::HungUp,
            }
        }
    }
}
