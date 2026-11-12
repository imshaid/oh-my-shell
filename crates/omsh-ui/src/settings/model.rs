//! The state of the settings view: the rows, what is typed to find one, which is chosen, and
//! what the last change did. A change goes through a [`Store`], so tests need no file.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use omsh_config::settings::{self, Row, SETTINGS};
use omsh_config::{Change, Config};

use crate::fuzzy::{Haystack, Query};
use crate::view::input::QueryLine;
use crate::view::run::Step;
use crate::view::select::Selection;

/// Where changes go: the configuration file.
pub trait Store: Send {
    /// Writes one change. The error is a sentence for the person.
    ///
    /// # Errors
    ///
    /// The reason the change was not written.
    fn save(&mut self, key: &str, change: &Change) -> Result<(), String>;

    /// The text of the file now, or `None` if there is none.
    fn text(&self) -> Option<String>;
}

/// What the last key did, shown on the line above the query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Nothing happened yet: the standing note is shown.
    Standing,
    /// A change was saved: `key` is now `value` in the file.
    Saved { key: &'static str, value: String },
    /// Something the person should know (a setting the view does not change).
    Note(String),
    /// A change was not saved.
    Failed(String),
}

pub struct Model {
    running: Config,
    store: Option<Box<dyn Store>>,
    rows: Vec<Row>,
    haystacks: Vec<Haystack>,
    line: QueryLine,
    query: Query,
    /// Indexes of the rows that match, best first.
    matched: Vec<usize>,
    selection: Selection,
    status: Status,
}

impl Model {
    /// `running` is what the shell started with. `store` is `None` when there is no file to
    /// write (`--no-config`): the view then only shows.
    #[must_use]
    pub fn new(running: Config, store: Option<Box<dyn Store>>) -> Self {
        let text = store.as_ref().and_then(|store| store.text());
        let rows = settings::rows(&running, text.as_deref());
        let haystacks = SETTINGS
            .iter()
            .map(|setting| Haystack::new(setting.key))
            .collect();
        let mut model = Self {
            running,
            store,
            rows,
            haystacks,
            line: QueryLine::default(),
            query: Query::parse(""),
            matched: Vec::new(),
            selection: Selection::new(),
            status: Status::Standing,
        };
        model.search();
        model
    }

    #[must_use]
    pub fn read_only(&self) -> bool {
        self.store.is_none()
    }

    #[must_use]
    pub fn status(&self) -> &Status {
        &self.status
    }

    #[must_use]
    pub fn typed(&self) -> &str {
        self.line.text()
    }

    #[must_use]
    pub fn cursor(&self) -> usize {
        self.line.cursor()
    }

    #[must_use]
    pub fn query(&self) -> &Query {
        &self.query
    }

    #[must_use]
    pub fn matches(&self) -> usize {
        self.matched.len()
    }

    #[must_use]
    pub fn total(&self) -> usize {
        self.rows.len()
    }

    #[must_use]
    pub fn selected(&self) -> usize {
        self.selection.selected()
    }

    #[must_use]
    pub fn offset(&self) -> usize {
        self.selection.offset()
    }

    pub fn set_page(&mut self, rows: usize) {
        self.selection.set_page(rows);
    }

    /// The `n`-th row of the list of matches.
    #[must_use]
    pub fn row(&self, n: usize) -> Option<&Row> {
        self.matched.get(n).map(|index| &self.rows[*index])
    }

    pub(crate) fn haystack(&self, n: usize) -> Option<&Haystack> {
        self.matched.get(n).map(|index| &self.haystacks[*index])
    }

    #[must_use]
    pub fn current(&self) -> Option<&Row> {
        self.row(self.selected())
    }

    /// How many settings the file has changed that this shell does not run yet.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.rows.iter().filter(|row| row.pending()).count()
    }

    pub fn paste(&mut self, text: &str) {
        if self.line.paste(text) {
            self.typed_changed();
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Step<()> {
        if key.kind == KeyEventKind::Release {
            return Step::Stay;
        }
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => Step::Cancel,
            KeyCode::Char('c' | 'g') if control => Step::Cancel,
            KeyCode::Enter | KeyCode::Tab => self.change(true),
            KeyCode::BackTab => self.change(false),
            KeyCode::Down => self.move_by(1, true),
            KeyCode::Char('n' | 'j') if control => self.move_by(1, true),
            KeyCode::Up => self.move_by(1, false),
            KeyCode::Char('p' | 'k') if control => self.move_by(1, false),
            KeyCode::PageDown => self.move_by(self.selection.page(), true),
            KeyCode::PageUp => self.move_by(self.selection.page(), false),
            KeyCode::Home => self.move_by(usize::MAX, false),
            KeyCode::End => self.move_by(usize::MAX, true),
            _ => {
                if self.line.edit(key) == Some(true) {
                    self.typed_changed();
                }
                Step::Stay
            }
        }
    }

    fn move_by(&mut self, by: usize, down: bool) -> Step<()> {
        if down {
            self.selection.forward(by, self.matched.len());
        } else {
            self.selection.back(by);
        }
        Step::Stay
    }

    /// Moves the chosen setting to its next (or previous) value and writes it to the file.
    fn change(&mut self, forward: bool) -> Step<()> {
        let Some(row) = self.current() else {
            return Step::Stay;
        };
        let (setting, saved) = (row.setting, row.saved.clone());
        let Some(store) = self.store.as_mut() else {
            self.status = Status::Failed(
                "This shell has no configuration file (--no-config): nothing can be saved."
                    .to_owned(),
            );
            return Step::Stay;
        };
        let Some(change) = settings::cycle(setting.key, &saved, forward) else {
            self.status = Status::Note(setting.elsewhere().to_owned());
            return Step::Stay;
        };
        match store.save(setting.key, &change) {
            Ok(()) => {
                let text = store.text();
                self.rows = settings::rows(&self.running, text.as_deref());
                let value = self
                    .rows
                    .iter()
                    .find(|row| row.setting.key == setting.key)
                    .map(|row| row.saved.clone())
                    .unwrap_or_default();
                self.status = Status::Saved {
                    key: setting.key,
                    value,
                };
            }
            Err(why) => self.status = Status::Failed(why),
        }
        Step::Stay
    }

    fn typed_changed(&mut self) {
        self.query = Query::parse(self.line.text());
        self.search();
    }

    fn search(&mut self) {
        let mut found: Vec<(usize, i32)> = Vec::new();
        for (index, haystack) in self.haystacks.iter().enumerate() {
            if let Some(score) = self.query.score(haystack) {
                found.push((index, score));
            }
        }
        // The order of the file for equal scores (the sort is stable).
        found.sort_by_key(|(_, score)| std::cmp::Reverse(*score));
        self.matched = found.into_iter().map(|(index, _)| index).collect();
        self.selection.reset();
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    /// The text of the fake file, and the reason its next write fails (if any); shared with the
    /// test.
    pub type Text = Arc<Mutex<String>>;
    pub type Failure = Arc<Mutex<Option<String>>>;

    /// A file in memory. The text is shared so that a test can look at it.
    pub struct Memory {
        pub text: Arc<Mutex<String>>,
        pub fail: Arc<Mutex<Option<String>>>,
    }

    impl Store for Memory {
        fn save(&mut self, key: &str, change: &Change) -> Result<(), String> {
            if let Some(why) = self.fail.lock().unwrap().clone() {
                return Err(why);
            }
            let mut text = self.text.lock().unwrap();
            // Enough of an editor for the tests: one line per key.
            let line = |value: &str| format!("{key} = {value}\n");
            let mut kept: String = text
                .lines()
                .filter(|existing| !existing.starts_with(&format!("{key} =")))
                .map(|existing| format!("{existing}\n"))
                .collect();
            match change {
                Change::Bool(on) => kept.push_str(&line(&on.to_string())),
                Change::Text(word) => kept.push_str(&line(&format!("\"{word}\""))),
                Change::Remove => {}
            }
            *text = kept;
            Ok(())
        }

        fn text(&self) -> Option<String> {
            // The in-memory text uses `section.key = value`, which TOML reads as dotted keys.
            Some(self.text.lock().unwrap().clone())
        }
    }

    pub fn memory(text: &str) -> (Memory, Text, Failure) {
        let text = Arc::new(Mutex::new(text.to_owned()));
        let fail = Arc::new(Mutex::new(None));
        (
            Memory {
                text: Arc::clone(&text),
                fail: Arc::clone(&fail),
            },
            text,
            fail,
        )
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn type_text(model: &mut Model, text: &str) {
        for c in text.chars() {
            model.handle_key(key(KeyCode::Char(c)));
        }
    }

    fn model_with(text: &str) -> (Model, Text, Failure) {
        let (store, text, fail) = memory(text);
        (
            Model::new(Config::default(), Some(Box::new(store))),
            text,
            fail,
        )
    }

    fn select(model: &mut Model, name: &str) {
        model.paste(name);
        assert_eq!(model.current().unwrap().setting.key, name);
    }

    #[test]
    fn every_setting_is_listed_in_the_order_of_the_file() {
        let (model, _, _) = model_with("");
        assert_eq!(model.matches(), SETTINGS.len());
        assert_eq!(model.row(0).unwrap().setting.key, "ui.theme");
        assert_eq!(model.pending(), 0);
    }

    #[test]
    fn typing_finds_a_setting_by_its_name() {
        let (mut model, _, _) = model_with("");
        type_text(&mut model, "hints");
        assert_eq!(model.current().unwrap().setting.key, "editor.hints");
        assert!(model.matches() < SETTINGS.len());
    }

    #[test]
    fn enter_changes_a_switch_and_the_file_has_it_but_the_running_value_does_not() {
        let (mut model, text, _) = model_with("");
        select(&mut model, "editor.hints");
        assert_eq!(model.handle_key(key(KeyCode::Enter)), Step::Stay);
        assert_eq!(text.lock().unwrap().as_str(), "editor.hints = false\n");
        let row = model.current().unwrap();
        assert_eq!(
            (row.running.as_str(), row.saved.as_str()),
            ("true", "false")
        );
        assert!(row.pending() && row.in_file);
        assert_eq!(model.pending(), 1);
        assert_eq!(
            model.status(),
            &Status::Saved {
                key: "editor.hints",
                value: "false".into()
            }
        );
        // Again: back to the running value, so nothing is pending.
        model.handle_key(key(KeyCode::Enter));
        assert_eq!(model.pending(), 0);
    }

    #[test]
    fn shift_tab_goes_the_other_way_round_a_choice() {
        let (mut model, text, _) = model_with("");
        select(&mut model, "ui.icons");
        model.handle_key(key(KeyCode::BackTab));
        assert_eq!(text.lock().unwrap().as_str(), "ui.icons = \"ascii\"\n");
        model.handle_key(key(KeyCode::Tab));
        assert_eq!(text.lock().unwrap().as_str(), "ui.icons = \"auto\"\n");
    }

    #[test]
    fn automatic_motion_takes_the_key_out_of_the_file() {
        let (mut model, text, _) = model_with("ui.motion = \"off\"\n");
        select(&mut model, "ui.motion");
        model.handle_key(key(KeyCode::Enter));
        assert_eq!(text.lock().unwrap().as_str(), "");
        assert_eq!(model.current().unwrap().saved, "automatic");
    }

    #[test]
    fn a_setting_the_view_does_not_change_says_where_to_change_it() {
        let (mut model, text, _) = model_with("");
        select(&mut model, "safety.confirm");
        model.handle_key(key(KeyCode::Enter));
        assert_eq!(
            model.status(),
            &Status::Note("Edit config.toml to change it.".into())
        );
        assert_eq!(text.lock().unwrap().as_str(), "");
        model.paste("");
        let (mut model, _, _) = model_with("");
        select(&mut model, "ai.model");
        model.handle_key(key(KeyCode::Enter));
        assert_eq!(
            model.status(),
            &Status::Note("Change it with `omsh ai setup`.".into())
        );
        let (mut model, _, _) = model_with("");
        select(&mut model, "history.size");
        model.handle_key(key(KeyCode::Enter));
        assert!(matches!(model.status(), Status::Note(n) if n.contains("starts")));
    }

    #[test]
    fn a_write_that_fails_changes_nothing_and_says_why() {
        let (mut model, text, fail) = model_with("");
        select(&mut model, "editor.hints");
        *fail.lock().unwrap() = Some("the file has a syntax error".into());
        model.handle_key(key(KeyCode::Enter));
        assert_eq!(
            model.status(),
            &Status::Failed("the file has a syntax error".into())
        );
        assert_eq!(text.lock().unwrap().as_str(), "");
        assert_eq!(model.current().unwrap().saved, "true");
        assert_eq!(model.pending(), 0);
    }

    #[test]
    fn without_a_file_the_view_shows_and_refuses_to_change() {
        let mut model = Model::new(Config::default(), None);
        assert!(model.read_only());
        select(&mut model, "editor.hints");
        model.handle_key(key(KeyCode::Enter));
        assert!(matches!(model.status(), Status::Failed(why) if why.contains("--no-config")));
        assert_eq!(model.current().unwrap().saved, "true");
    }

    #[test]
    fn a_change_made_before_the_view_opened_is_pending_from_the_start() {
        let (store, _, _) = memory("editor.mode = \"vi\"\n");
        let model = Model::new(Config::default(), Some(Box::new(store)));
        assert_eq!(model.pending(), 1);
    }

    #[test]
    fn escape_ctrl_c_and_ctrl_g_close_the_view() {
        let (mut model, _, _) = model_with("");
        assert_eq!(model.handle_key(key(KeyCode::Esc)), Step::Cancel);
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        assert_eq!(model.handle_key(ctrl('c')), Step::Cancel);
        assert_eq!(model.handle_key(ctrl('g')), Step::Cancel);
    }

    #[test]
    fn the_selection_moves_and_stays_in_the_list() {
        let (mut model, _, _) = model_with("");
        model.set_page(5);
        model.handle_key(key(KeyCode::Down));
        assert_eq!(model.selected(), 1);
        model.handle_key(key(KeyCode::End));
        assert_eq!(model.selected(), SETTINGS.len() - 1);
        model.handle_key(key(KeyCode::Home));
        assert_eq!((model.selected(), model.offset()), (0, 0));
        model.handle_key(key(KeyCode::Up));
        assert_eq!(model.selected(), 0);
    }

    #[test]
    fn a_change_keeps_the_chosen_row() {
        let (mut model, _, _) = model_with("");
        select(&mut model, "ai.context.git");
        let at = model.selected();
        model.handle_key(key(KeyCode::Enter));
        assert_eq!(model.selected(), at);
        assert_eq!(model.current().unwrap().setting.key, "ai.context.git");
    }
}
