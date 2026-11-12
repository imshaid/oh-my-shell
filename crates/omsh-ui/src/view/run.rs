//! The loop that owns the terminal while a full-screen view is open.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use crossterm::event::{self, Event, KeyEvent};
use ratatui::backend::CrosstermBackend;
use ratatui::{Frame, Terminal};

use crate::screen::FullScreen;

/// How long to wait for a key before asking whether the shell was told to hang up.
const STEP: Duration = Duration::from_millis(100);

/// How long a view that was told to stop gets to put the terminal back before it is given up
/// on.
const GRACE: Duration = Duration::from_millis(300);

/// What a key did to a view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step<T> {
    /// The view goes on; draw it again.
    Stay,
    /// The view is finished, with this answer.
    Done(T),
    /// The person left without choosing.
    Cancel,
}

/// A full-screen view: state, keys and drawing. It runs on a thread of its own (see [`run`]).
pub trait View: Send + 'static {
    /// What the person chose.
    type Output: Send + 'static;

    fn handle_key(&mut self, key: KeyEvent) -> Step<Self::Output>;

    /// Bracketed paste.
    fn paste(&mut self, text: &str);

    fn draw(&mut self, frame: &mut Frame<'_>);
}

/// How a view ended.
#[derive(Debug)]
pub enum Outcome<T> {
    Done(T),
    /// Escape or Ctrl+C: nothing was chosen.
    Cancelled,
    /// The shell was told to hang up, or the terminal went away.
    HungUp,
    /// The terminal could not be taken (not a terminal, or no raw mode); nothing was shown.
    Unavailable(io::Error),
}

/// Opens `view` and runs it until the person finishes or leaves. `should_stop` is asked about
/// every tenth of a second while no key comes: true ends the view with [`Outcome::HungUp`].
/// `name` is the name of the helper thread.
///
/// **Why the keys are read on a thread of their own.** The input library takes the end of input
/// of a closed terminal for "try again", inside the very call that waits for a key, in a loop
/// that never looks at the clock. A view that read keys on the calling thread would spin at full
/// speed for ever when the terminal window is closed. So the keys are read on a helper thread and
/// this one waits for it in short steps, asking `should_stop`; if the terminal is gone the helper
/// is abandoned (it ends with the process), as the line editor's thread is.
pub fn run<V: View>(
    name: &'static str,
    view: V,
    should_stop: &mut dyn FnMut() -> bool,
) -> Outcome<V::Output> {
    let guard = match FullScreen::enter() {
        Ok(guard) => guard,
        Err(error) => return Outcome::Unavailable(error),
    };
    let terminal = match Terminal::new(CrosstermBackend::new(io::stderr())) {
        Ok(terminal) => terminal,
        Err(error) => return Outcome::Unavailable(error),
    };
    let stop = Arc::new(AtomicBool::new(false));
    let helper_stop = Arc::clone(&stop);
    let (send, receive) = mpsc::channel();
    let spawned = thread::Builder::new().name(name.to_owned()).spawn(move || {
        let (mut terminal, mut view) = (terminal, view);
        let outcome = event_loop(&mut terminal, &mut view, &helper_stop);
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
                return Outcome::Unavailable(io::Error::other("the view stopped unexpectedly"));
            }
        }
    }
}

fn event_loop<V: View>(
    terminal: &mut Terminal<CrosstermBackend<io::Stderr>>,
    view: &mut V,
    stop: &AtomicBool,
) -> Outcome<V::Output> {
    let mut dirty = true;
    loop {
        if dirty {
            if terminal.draw(|frame| view.draw(frame)).is_err() {
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
                Ok(Event::Key(key)) => match view.handle_key(key) {
                    Step::Done(answer) => return Outcome::Done(answer),
                    Step::Cancel => return Outcome::Cancelled,
                    Step::Stay => dirty = true,
                },
                Ok(Event::Paste(text)) => {
                    view.paste(&text);
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
