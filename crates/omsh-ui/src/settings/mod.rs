//! The settings view: every key of `config.toml` with its value, where the value comes from and
//! whether the running shell already has it. Switches and fixed choices can be changed here;
//! the change is written to the file and applies when a new shell starts.

pub mod model;
pub mod render;

use std::path::PathBuf;

use omsh_config::{Change, Config};
use ratatui::Frame;

pub use model::{Model, Status, Store};

use crate::look::Look;
use crate::theme::Theme;
use crate::view::draw::Styles;
use crate::view::run::{self, Step, View};

/// How the view ended.
#[derive(Debug)]
pub enum Outcome {
    /// Closed.
    Closed,
    /// The shell was told to hang up, or the terminal went away.
    HungUp,
    /// The terminal could not be taken; nothing was shown.
    Unavailable(std::io::Error),
}

/// The real store: `config.toml`.
struct File {
    path: PathBuf,
}

impl Store for File {
    fn save(&mut self, key: &str, change: &Change) -> Result<(), String> {
        omsh_config::set_setting(&self.path, key, change).map_err(|error| error.to_string())
    }

    fn text(&self) -> Option<String> {
        omsh_config::read_text_file(&self.path).ok().flatten()
    }
}

struct Settings {
    model: Model,
    styles: Styles,
}

impl View for Settings {
    type Output = ();

    fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> Step<()> {
        self.model.handle_key(key)
    }

    fn paste(&mut self, text: &str) {
        self.model.paste(text);
    }

    fn draw(&mut self, frame: &mut Frame<'_>) {
        render::draw(frame, &mut self.model, &self.styles);
    }
}

/// Opens the view and runs it. `running` is the configuration this shell started with; `file`
/// is `config.toml`, or `None` when the shell reads none (the view then only shows). See
/// [`run::run`] for `should_stop`.
pub fn run(
    running: Config,
    file: Option<PathBuf>,
    theme: &Theme,
    look: Look,
    should_stop: &mut dyn FnMut() -> bool,
) -> Outcome {
    let store = file.map(|path| Box::new(File { path }) as Box<dyn Store>);
    let view = Settings {
        model: Model::new(running, store),
        styles: Styles::new(theme, look),
    };
    match run::run("settings-view", view, should_stop) {
        run::Outcome::Done(()) | run::Outcome::Cancelled => Outcome::Closed,
        run::Outcome::HungUp => Outcome::HungUp,
        run::Outcome::Unavailable(error) => Outcome::Unavailable(error),
    }
}
