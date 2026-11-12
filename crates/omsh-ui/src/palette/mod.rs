//! The command palette (`Ctrl+P`): a full-screen list of what the shell can do and the names a
//! person can type, with a fuzzy search. It gives back an [`Effect`]; the line editor carries it
//! out. Picking a name puts it in the line and never runs it.

pub mod model;
pub mod render;

use ratatui::Frame;

pub use model::{Effect, Entry, Kind, Model};

use crate::look::Look;
use crate::theme::Theme;
use crate::view::draw::Styles;
use crate::view::run::{self, Step, View};

/// How the palette ended.
#[derive(Debug)]
pub enum Outcome {
    Chosen(Effect),
    /// Escape or Ctrl+C.
    Cancelled,
    /// The shell was told to hang up, or the terminal went away.
    HungUp,
    /// The terminal could not be taken; nothing was shown.
    Unavailable(std::io::Error),
}

struct Palette {
    model: Model,
    styles: Styles,
}

impl View for Palette {
    type Output = Effect;

    fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> Step<Effect> {
        self.model.handle_key(key)
    }

    fn paste(&mut self, text: &str) {
        self.model.paste(text);
    }

    fn draw(&mut self, frame: &mut Frame<'_>) {
        render::draw(frame, &mut self.model, &self.styles);
    }
}

/// Opens the palette and runs it. See [`run::run`] for `should_stop`.
pub fn run(
    entries: Vec<Entry>,
    theme: &Theme,
    look: Look,
    should_stop: &mut dyn FnMut() -> bool,
) -> Outcome {
    let view = Palette {
        model: Model::new(entries),
        styles: Styles::new(theme, look),
    };
    match run::run("command-palette", view, should_stop) {
        run::Outcome::Done(effect) => Outcome::Chosen(effect),
        run::Outcome::Cancelled => Outcome::Cancelled,
        run::Outcome::HungUp => Outcome::HungUp,
        run::Outcome::Unavailable(error) => Outcome::Unavailable(error),
    }
}
