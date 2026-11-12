//! The history explorer: a full-screen list of the commands that were run, with a fuzzy search.
//! `model` holds the state and the rules, `render` draws it, and [`run`] is the loop that owns
//! the terminal while the view is open.
//!
//! The view never writes anything but its own screen: no history entry, no variable. It gives a
//! command back, and the caller decides what to do with it (the line editor puts it in the
//! line, it does not run it).

pub mod model;
pub mod render;

use ratatui::Frame;

pub use model::{Action, Item, Model, Scope};
pub use render::{Context, Styles};

use crate::look::Look;
use crate::theme::Theme;
use crate::view::run::{self, Step, View};

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
    Unavailable(std::io::Error),
}

struct Explorer {
    model: Model,
    context: Context,
}

impl View for Explorer {
    type Output = String;

    fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> Step<String> {
        match self.model.handle_key(key) {
            Action::Pick(command) => Step::Done(command),
            Action::Cancel => Step::Cancel,
            Action::None => Step::Stay,
        }
    }

    fn paste(&mut self, text: &str) {
        self.model.paste(text);
    }

    fn draw(&mut self, frame: &mut Frame<'_>) {
        render::draw(frame, &mut self.model, &self.context);
    }
}

/// Opens the view and runs it until the person picks a command or leaves. `should_stop` is asked
/// about every tenth of a second while no key comes: true ends the view with
/// [`Outcome::HungUp`]. See [`run::run`] for why the keys are read on a thread of their own.
pub fn run(settings: Settings<'_>, should_stop: &mut dyn FnMut() -> bool) -> Outcome {
    let context = Context {
        styles: Styles::new(settings.theme, settings.look),
        window: settings.window,
        unavailable: settings.unavailable,
        now_ms: settings.now_ms,
    };
    let model = Model::new(settings.items, settings.folder, settings.typed);
    match run::run("history-explorer", Explorer { model, context }, should_stop) {
        run::Outcome::Done(command) => Outcome::Picked(command),
        run::Outcome::Cancelled => Outcome::Cancelled,
        run::Outcome::HungUp => Outcome::HungUp,
        run::Outcome::Unavailable(error) => Outcome::Unavailable(error),
    }
}
