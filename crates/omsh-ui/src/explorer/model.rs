//! The state of the history explorer: what is typed, which scope, which rows match, which is
//! chosen. Nothing here draws or reads the terminal, so every rule has a plain test.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::fuzzy::{Haystack, Query};
use crate::view::input::QueryLine;
use crate::view::select::Selection;

/// One command of the list: the newest run of a distinct command text.
#[derive(Clone, Debug)]
pub struct Item {
    pub command: String,
    /// Where its newest run started.
    pub cwd: String,
    /// How its newest run ended; `None` while it runs, or if the shell ended first.
    pub status: Option<i32>,
    /// When its newest run started, in milliseconds since 1970.
    pub started_ms: i64,
    /// How many runs of it are in the window.
    pub runs: u64,
    haystack: Haystack,
}

impl Item {
    #[must_use]
    pub fn new(
        command: String,
        cwd: String,
        status: Option<i32>,
        started_ms: i64,
        runs: u64,
    ) -> Self {
        let haystack = Haystack::new(&command);
        Self {
            command,
            cwd,
            status,
            started_ms,
            runs,
            haystack,
        }
    }

    pub(crate) fn haystack(&self) -> &Haystack {
        &self.haystack
    }

    /// The newest run ended with a status other than 0.
    #[must_use]
    pub fn failed(&self) -> bool {
        matches!(self.status, Some(status) if status != 0)
    }
}

/// Which commands are in the list before the query is applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Every command.
    All,
    /// Commands whose newest run started in the folder the shell is in.
    Folder,
    /// Commands whose newest run failed.
    Failed,
}

impl Scope {
    /// The one `Ctrl+R` moves to.
    #[must_use]
    pub fn next(self) -> Scope {
        match self {
            Scope::All => Scope::Folder,
            Scope::Folder => Scope::Failed,
            Scope::Failed => Scope::All,
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Scope::All => "all commands",
            Scope::Folder => "this folder",
            Scope::Failed => "failed",
        }
    }
}

/// What a key asks the caller to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Nothing; redraw if anything changed.
    None,
    /// Put this command in the line.
    Pick(String),
    /// Close the view and leave the line as it was.
    Cancel,
}

pub struct Model {
    items: Vec<Item>,
    folder: String,
    /// The text typed, and where the cursor is in it.
    line: QueryLine,
    query: Query,
    scope: Scope,
    /// The items that match the query in this scope, with their scores, in list order.
    matched: Vec<(usize, i32)>,
    /// The query and scope `matched` was made for.
    matched_for: (Query, Scope),
    /// Items in the scope, before the query.
    in_scope: usize,
    /// The chosen row and the row at the bottom of the screen, counted from the best match (0).
    selection: Selection,
}

impl Model {
    /// `typed` is the text of the query to start with (what was in the line).
    #[must_use]
    pub fn new(items: Vec<Item>, folder: String, typed: &str) -> Self {
        let line = QueryLine::new(typed);
        let mut model = Self {
            items,
            folder,
            query: Query::parse(line.text()),
            matched_for: (Query::parse(""), Scope::All),
            line,
            scope: Scope::All,
            matched: Vec::new(),
            in_scope: 0,
            selection: Selection::new(),
        };
        model.search(false);
        model
    }

    #[must_use]
    pub fn typed(&self) -> &str {
        self.line.text()
    }

    /// The cursor, as a number of characters from the start of the text.
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.line.cursor()
    }

    #[must_use]
    pub fn scope(&self) -> Scope {
        self.scope
    }

    #[must_use]
    pub fn query(&self) -> &Query {
        &self.query
    }

    /// How many commands match.
    #[must_use]
    pub fn matches(&self) -> usize {
        self.matched.len()
    }

    /// How many commands are in the scope (before the query).
    #[must_use]
    pub fn in_scope(&self) -> usize {
        self.in_scope
    }

    #[must_use]
    pub fn total(&self) -> usize {
        self.items.len()
    }

    #[must_use]
    pub fn folder(&self) -> &str {
        &self.folder
    }

    /// The chosen row, counted from the best match.
    #[must_use]
    pub fn selected(&self) -> usize {
        self.selection.selected()
    }

    /// The row at the bottom of the screen, counted from the best match.
    #[must_use]
    pub fn offset(&self) -> usize {
        self.selection.offset()
    }

    /// The chosen command.
    #[must_use]
    pub fn current(&self) -> Option<&Item> {
        self.row(self.selected())
    }

    /// The `n`-th best match (0 is the best).
    #[must_use]
    pub fn row(&self, n: usize) -> Option<&Item> {
        self.matched.get(n).map(|(index, _)| &self.items[*index])
    }

    /// Tells the model how many rows the list has room for.
    pub fn set_page(&mut self, rows: usize) {
        self.selection.set_page(rows);
    }

    /// Text pasted into the query (bracketed paste). Line ends and other control characters
    /// are dropped.
    pub fn paste(&mut self, text: &str) {
        if self.line.paste(text) {
            self.typed_changed();
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Action {
        if key.kind == KeyEventKind::Release {
            return Action::None;
        }
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Enter | KeyCode::Tab => self
                .current()
                .map_or(Action::None, |item| Action::Pick(item.command.clone())),
            KeyCode::Esc => Action::Cancel,
            KeyCode::Char('c' | 'g') if control => Action::Cancel,
            KeyCode::Char('r') if control => {
                self.scope = self.scope.next();
                self.search(false);
                Action::None
            }
            KeyCode::Up => self.step_older(1),
            KeyCode::Char('p' | 'k') if control => self.step_older(1),
            KeyCode::Down => self.step_newer(1),
            KeyCode::Char('n' | 'j') if control => self.step_newer(1),
            KeyCode::PageUp => self.step_older(self.selection.page()),
            KeyCode::PageDown => self.step_newer(self.selection.page()),
            KeyCode::Home => self.step_older(usize::MAX),
            KeyCode::End => self.step_newer(usize::MAX),
            _ => {
                if self.line.edit(key) == Some(true) {
                    self.typed_changed();
                }
                Action::None
            }
        }
    }

    /// Toward worse and older matches (up the screen).
    fn step_older(&mut self, by: usize) -> Action {
        self.selection.forward(by, self.matched.len());
        Action::None
    }

    /// Toward the best match (down the screen).
    fn step_newer(&mut self, by: usize) -> Action {
        self.selection.back(by);
        Action::None
    }

    fn typed_changed(&mut self) {
        self.query = Query::parse(self.line.text());
        self.search(true);
    }

    fn in_scope_item(&self, item: &Item) -> bool {
        match self.scope {
            Scope::All => true,
            Scope::Folder => item.cwd == self.folder,
            Scope::Failed => item.failed(),
        }
    }

    /// Finds the matches of the query in the scope. When the query only grew since the last
    /// search (and the scope is the same), only the earlier matches are looked at again.
    fn search(&mut self, may_narrow: bool) {
        let narrow = may_narrow
            && self.matched_for.1 == self.scope
            && self.query.narrows(&self.matched_for.0);
        let mut found: Vec<(usize, i32)> = Vec::new();
        if narrow {
            for &(index, _) in &self.matched {
                if let Some(score) = self.query.score(self.items[index].haystack()) {
                    found.push((index, score));
                }
            }
        } else {
            let mut in_scope = 0;
            for (index, item) in self.items.iter().enumerate() {
                if !self.in_scope_item(item) {
                    continue;
                }
                in_scope += 1;
                if let Some(score) = self.query.score(item.haystack()) {
                    found.push((index, score));
                }
            }
            self.in_scope = in_scope;
        }
        // `found` is in item order, which is newest first; the sort is stable, so equal scores
        // stay newest first.
        found.sort_by_key(|(_, score)| std::cmp::Reverse(*score));
        self.matched = found;
        self.matched_for = (self.query.clone(), self.scope);
        self.selection.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(command: &str, cwd: &str, status: Option<i32>) -> Item {
        Item::new(command.to_owned(), cwd.to_owned(), status, 0, 1)
    }

    fn sample() -> Model {
        // Newest first, as the history gives them.
        Model::new(
            vec![
                item("docker ps -a", "/a", Some(0)),
                item("git push", "/b", Some(1)),
                item("ls", "/a", Some(0)),
                item("docker compose up", "/b", Some(0)),
                item("make test", "/a", Some(2)),
            ],
            "/a".to_owned(),
            "",
        )
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn type_text(model: &mut Model, text: &str) {
        for c in text.chars() {
            model.handle_key(key(KeyCode::Char(c)));
        }
    }

    fn listed(model: &Model) -> Vec<String> {
        (0..model.matches())
            .map(|n| model.row(n).unwrap().command.clone())
            .collect()
    }

    #[test]
    fn an_empty_query_lists_everything_newest_first() {
        let model = sample();
        assert_eq!(model.matches(), 5);
        assert_eq!(model.in_scope(), 5);
        assert_eq!(listed(&model)[0], "docker ps -a");
        assert_eq!(listed(&model)[4], "make test");
        assert_eq!(model.current().unwrap().command, "docker ps -a");
    }

    #[test]
    fn a_better_match_comes_before_a_newer_one() {
        // `scripts` is newer, but `ps` starts a word in `docker ps`.
        let mut model = Model::new(
            vec![
                item("ls scripts", "/a", Some(0)),
                item("docker ps", "/a", Some(0)),
            ],
            "/a".into(),
            "",
        );
        assert_eq!(listed(&model), ["ls scripts", "docker ps"]);
        type_text(&mut model, "ps");
        assert_eq!(listed(&model), ["docker ps", "ls scripts"]);
        // With the same score the newer one stays first.
        let mut same = Model::new(
            vec![item("echo b", "/a", Some(0)), item("echo a", "/a", Some(0))],
            "/a".into(),
            "",
        );
        type_text(&mut same, "echo");
        assert_eq!(listed(&same), ["echo b", "echo a"]);
    }

    #[test]
    fn typing_narrows_the_list_and_puts_the_best_match_first() {
        let mut model = sample();
        type_text(&mut model, "dock");
        assert_eq!(listed(&model), ["docker ps -a", "docker compose up"]);
        type_text(&mut model, " comp");
        assert_eq!(listed(&model), ["docker compose up"]);
        assert_eq!(model.in_scope(), 5);
    }

    #[test]
    fn the_text_that_was_in_the_line_is_the_first_query() {
        let model = Model::new(
            vec![item("ls", "/a", Some(0)), item("git push", "/a", Some(0))],
            "/a".into(),
            "git\n",
        );
        assert_eq!(model.typed(), "git");
        assert_eq!(model.cursor(), 3);
        assert_eq!(listed(&model), ["git push"]);
    }

    #[test]
    fn ctrl_r_cycles_the_scope_and_counts_what_is_in_it() {
        let mut model = sample();
        assert_eq!(model.scope(), Scope::All);
        model.handle_key(ctrl('r'));
        assert_eq!(model.scope(), Scope::Folder);
        assert_eq!(listed(&model), ["docker ps -a", "ls", "make test"]);
        assert_eq!(
            (model.matches(), model.in_scope(), model.total()),
            (3, 3, 5)
        );
        model.handle_key(ctrl('r'));
        assert_eq!(model.scope(), Scope::Failed);
        assert_eq!(listed(&model), ["git push", "make test"]);
        model.handle_key(ctrl('r'));
        assert_eq!(model.scope(), Scope::All);
        assert_eq!(model.matches(), 5);
    }

    #[test]
    fn a_command_that_is_still_running_is_not_failed() {
        let mut model = Model::new(vec![item("sleep 9", "/a", None)], "/a".into(), "");
        model.handle_key(ctrl('r'));
        model.handle_key(ctrl('r'));
        assert_eq!(model.scope(), Scope::Failed);
        assert_eq!(model.matches(), 0);
    }

    #[test]
    fn the_scope_and_the_query_work_together() {
        let mut model = sample();
        model.handle_key(ctrl('r')); // this folder
        type_text(&mut model, "d");
        assert_eq!(listed(&model), ["docker ps -a"]);
    }

    #[test]
    fn enter_and_tab_pick_the_chosen_command_and_escape_picks_nothing() {
        let mut model = sample();
        model.handle_key(key(KeyCode::Up));
        assert_eq!(
            model.handle_key(key(KeyCode::Enter)),
            Action::Pick("git push".into())
        );
        assert_eq!(
            model.handle_key(key(KeyCode::Tab)),
            Action::Pick("git push".into())
        );
        assert_eq!(model.handle_key(key(KeyCode::Esc)), Action::Cancel);
        assert_eq!(model.handle_key(ctrl('c')), Action::Cancel);
        assert_eq!(model.handle_key(ctrl('g')), Action::Cancel);
    }

    #[test]
    fn enter_with_nothing_listed_does_nothing() {
        let mut model = sample();
        type_text(&mut model, "zzzzzz");
        assert_eq!(model.matches(), 0);
        assert_eq!(model.handle_key(key(KeyCode::Enter)), Action::None);
        assert!(model.current().is_none());
    }

    #[test]
    fn the_selection_moves_and_stays_inside_the_list() {
        let mut model = sample();
        model.set_page(2);
        model.handle_key(key(KeyCode::Down));
        assert_eq!(model.selected(), 0);
        for _ in 0..10 {
            model.handle_key(key(KeyCode::Up));
        }
        assert_eq!(model.selected(), 4);
        // The window of two rows followed the selection.
        assert_eq!(model.offset(), 3);
        model.handle_key(ctrl('n'));
        assert_eq!(model.selected(), 3);
        model.handle_key(key(KeyCode::End));
        assert_eq!((model.selected(), model.offset()), (0, 0));
        model.handle_key(key(KeyCode::Home));
        assert_eq!(model.selected(), 4);
        model.handle_key(key(KeyCode::PageDown));
        assert_eq!(model.selected(), 2);
        model.handle_key(key(KeyCode::PageUp));
        assert_eq!(model.selected(), 4);
    }

    #[test]
    fn a_new_query_selects_the_best_match_again() {
        let mut model = sample();
        model.handle_key(key(KeyCode::Up));
        model.handle_key(key(KeyCode::Up));
        type_text(&mut model, "d");
        assert_eq!(model.selected(), 0);
    }

    #[test]
    fn editing_the_query() {
        let mut model = sample();
        type_text(&mut model, "dock er");
        assert_eq!(model.typed(), "dock er");
        model.handle_key(key(KeyCode::Left));
        model.handle_key(key(KeyCode::Left));
        model.handle_key(key(KeyCode::Backspace));
        assert_eq!(model.typed(), "docker");
        model.handle_key(key(KeyCode::Delete));
        assert_eq!(model.typed(), "dockr");
        model.handle_key(ctrl('e'));
        model.handle_key(ctrl('w'));
        assert_eq!(model.typed(), "");
        type_text(&mut model, "ab cd");
        model.handle_key(ctrl('w'));
        assert_eq!(model.typed(), "ab ");
        model.handle_key(ctrl('u'));
        assert_eq!(model.typed(), "");
        model.handle_key(key(KeyCode::Backspace));
        model.handle_key(ctrl('a'));
        assert_eq!(model.cursor(), 0);
    }

    #[test]
    fn letters_that_are_not_ascii_are_edited_by_character() {
        let mut model = sample();
        type_text(&mut model, "äö");
        assert_eq!(model.cursor(), 2);
        model.handle_key(key(KeyCode::Left));
        model.handle_key(key(KeyCode::Backspace));
        assert_eq!(model.typed(), "ö");
    }

    #[test]
    fn a_paste_drops_line_ends_and_control_characters() {
        let mut model = sample();
        model.paste("do\nck\x1b[31mer");
        assert_eq!(model.typed(), "dock[31mer");
        assert!(!model.typed().chars().any(char::is_control));
    }

    #[test]
    fn control_keys_and_releases_do_not_type() {
        let mut model = sample();
        model.handle_key(ctrl('x'));
        model.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT));
        let mut release = key(KeyCode::Char('q'));
        release.kind = KeyEventKind::Release;
        model.handle_key(release);
        assert_eq!(model.typed(), "");
    }

    /// Typing one character at a time (which narrows the earlier matches) must give the same
    /// list as asking the whole text at once.
    #[test]
    fn narrowing_gives_the_same_list_as_a_fresh_search() {
        let commands = [
            "docker ps -a",
            "docker compose up -d",
            "git push origin main",
            "git pull",
            "ls -la",
            "cargo build --release",
            "cargo test -p omsh-ui",
            "Dockerfile lint",
            "echo 'a b'",
            "sudo systemctl restart docker",
        ];
        let items = || {
            commands
                .iter()
                .map(|c| item(c, "/a", Some(0)))
                .collect::<Vec<_>>()
        };
        for query in [
            "do",
            "do ps",
            "'git pu",
            "^c ! test",
            "!docker",
            "g o",
            "D",
            "k$",
            "rest doc",
        ] {
            let mut typed = Model::new(items(), "/a".into(), "");
            type_text(&mut typed, query);
            let fresh = Model::new(items(), "/a".into(), query);
            assert_eq!(listed(&typed), listed(&fresh), "query {query:?}");
        }
    }
}
