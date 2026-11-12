//! The state of the command palette: the entries, what is typed, which entries match, which is
//! chosen. Nothing here draws or reads the terminal.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::fuzzy::{Haystack, Query};
use crate::view::input::QueryLine;
use crate::view::run::Step;
use crate::view::select::Selection;

/// What an entry is. The order is the order of the groups in the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// Something the shell can do: open a view, explain a failure.
    Action,
    Alias,
    Abbreviation,
    Function,
    Builtin,
}

impl Kind {
    /// The tag shown in front of an entry, ASCII, at most 7 characters.
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::Action => "action",
            Self::Alias => "alias",
            Self::Abbreviation => "abbr",
            Self::Function => "func",
            Self::Builtin => "builtin",
        }
    }
}

/// What choosing an entry does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Put this text in the line at the cursor. Nothing runs.
    Insert(String),
    OpenSettings,
    OpenHistory,
    /// The same as `Alt+?`.
    ExplainLast,
}

/// One line of the palette.
#[derive(Clone, Debug)]
pub struct Entry {
    pub kind: Kind,
    pub name: String,
    /// What it is or does, in one line.
    pub detail: String,
    /// The key that does the same without the palette (`Ctrl+R`), if any.
    pub key: Option<String>,
    pub effect: Effect,
    haystack: Haystack,
    name_len: usize,
}

impl Entry {
    #[must_use]
    pub fn new(
        kind: Kind,
        name: impl Into<String>,
        detail: impl Into<String>,
        key: Option<&str>,
        effect: Effect,
    ) -> Self {
        let (name, detail) = (name.into(), detail.into());
        let haystack = Haystack::new(&format!("{name} {detail}"));
        Self {
            kind,
            name_len: name.chars().count(),
            name,
            detail,
            key: key.map(str::to_owned),
            effect,
            haystack,
        }
    }

    pub(crate) fn haystack(&self) -> &Haystack {
        &self.haystack
    }

    /// How many characters the name has (the first ones of the text that is searched).
    pub(crate) fn name_len(&self) -> usize {
        self.name_len
    }
}

pub struct Model {
    entries: Vec<Entry>,
    line: QueryLine,
    query: Query,
    /// The entries that match, with their scores, in list order.
    matched: Vec<(usize, i32)>,
    matched_for: Query,
    selection: Selection,
}

impl Model {
    /// The entries are grouped (actions, aliases, abbreviations, functions, builtins); inside a
    /// group actions keep the order given and the others are sorted by name. With nothing typed
    /// this is the order of the list.
    #[must_use]
    pub fn new(mut entries: Vec<Entry>) -> Self {
        entries.sort_by(|a, b| {
            a.kind.cmp(&b.kind).then_with(|| {
                if a.kind == Kind::Action {
                    std::cmp::Ordering::Equal
                } else {
                    a.name.cmp(&b.name)
                }
            })
        });
        let mut model = Self {
            entries,
            line: QueryLine::default(),
            query: Query::parse(""),
            matched: Vec::new(),
            matched_for: Query::parse(""),
            selection: Selection::new(),
        };
        model.search(false);
        model
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
        self.entries.len()
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

    #[must_use]
    pub fn current(&self) -> Option<&Entry> {
        self.row(self.selected())
    }

    /// The `n`-th entry of the list of matches (0 is the first).
    #[must_use]
    pub fn row(&self, n: usize) -> Option<&Entry> {
        self.matched.get(n).map(|(index, _)| &self.entries[*index])
    }

    pub fn paste(&mut self, text: &str) {
        if self.line.paste(text) {
            self.typed_changed();
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Step<Effect> {
        if key.kind == KeyEventKind::Release {
            return Step::Stay;
        }
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Enter | KeyCode::Tab => self
                .current()
                .map_or(Step::Stay, |entry| Step::Done(entry.effect.clone())),
            KeyCode::Esc => Step::Cancel,
            KeyCode::Char('c' | 'g') if control => Step::Cancel,
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

    fn move_by(&mut self, by: usize, down: bool) -> Step<Effect> {
        if down {
            self.selection.forward(by, self.matched.len());
        } else {
            self.selection.back(by);
        }
        Step::Stay
    }

    fn typed_changed(&mut self) {
        self.query = Query::parse(self.line.text());
        self.search(true);
    }

    /// When the query only grew since the last search, only the earlier matches are looked at
    /// again.
    fn search(&mut self, may_narrow: bool) {
        let mut found: Vec<(usize, i32)> = Vec::new();
        if may_narrow && self.query.narrows(&self.matched_for) {
            for &(index, _) in &self.matched {
                if let Some(score) = self.query.score(self.entries[index].haystack()) {
                    found.push((index, score));
                }
            }
        } else {
            for (index, entry) in self.entries.iter().enumerate() {
                if let Some(score) = self.query.score(entry.haystack()) {
                    found.push((index, score));
                }
            }
        }
        // Entries are in group order; the sort is stable, so equal scores stay in that order.
        found.sort_by_key(|(_, score)| std::cmp::Reverse(*score));
        self.matched = found;
        self.matched_for = self.query.clone();
        self.selection.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries() -> Vec<Entry> {
        vec![
            Entry::new(
                Kind::Builtin,
                "history",
                "List commands.",
                None,
                Effect::Insert("history".into()),
            ),
            Entry::new(
                Kind::Alias,
                "ll",
                "ls -l",
                None,
                Effect::Insert("ll".into()),
            ),
            Entry::new(
                Kind::Action,
                "Settings",
                "Open the settings view.",
                None,
                Effect::OpenSettings,
            ),
            Entry::new(
                Kind::Builtin,
                "cd",
                "Change folder.",
                None,
                Effect::Insert("cd".into()),
            ),
            Entry::new(
                Kind::Action,
                "History explorer",
                "Search every command.",
                Some("Ctrl+R"),
                Effect::OpenHistory,
            ),
            Entry::new(
                Kind::Function,
                "mkcd",
                "A function.",
                None,
                Effect::Insert("mkcd".into()),
            ),
            Entry::new(
                Kind::Abbreviation,
                "gco",
                "git checkout",
                None,
                Effect::Insert("git checkout".into()),
            ),
            Entry::new(
                Kind::Alias,
                "gs",
                "git status",
                None,
                Effect::Insert("gs".into()),
            ),
        ]
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

    fn names(model: &Model) -> Vec<String> {
        (0..model.matches())
            .map(|n| model.row(n).unwrap().name.clone())
            .collect()
    }

    #[test]
    fn with_nothing_typed_the_groups_come_in_order_and_actions_keep_their_order() {
        let model = Model::new(entries());
        assert_eq!(
            names(&model),
            [
                "Settings",
                "History explorer",
                "gs",
                "ll",
                "gco",
                "mkcd",
                "cd",
                "history"
            ]
            .map(String::from)
        );
        assert_eq!(model.total(), 8);
    }

    #[test]
    fn typing_filters_and_the_best_match_is_first() {
        let mut model = Model::new(entries());
        type_text(&mut model, "hist");
        // Both match. The name `history` has the better score (the whole name, same case).
        assert_eq!(
            names(&model)[..2],
            ["history", "History explorer"].map(String::from)
        );
        assert_eq!(model.selected(), 0);
        type_text(&mut model, "x");
        assert_eq!(names(&model), ["History explorer".to_owned()]);
    }

    #[test]
    fn equal_scores_keep_the_order_of_the_groups() {
        let mut model = Model::new(vec![
            Entry::new(
                Kind::Builtin,
                "same",
                "text",
                None,
                Effect::Insert("b".into()),
            ),
            Entry::new(
                Kind::Function,
                "same",
                "text",
                None,
                Effect::Insert("f".into()),
            ),
            Entry::new(
                Kind::Alias,
                "same",
                "text",
                None,
                Effect::Insert("a".into()),
            ),
        ]);
        type_text(&mut model, "same");
        let order: Vec<_> = (0..3).map(|n| model.row(n).unwrap().kind).collect();
        assert_eq!(order, [Kind::Alias, Kind::Function, Kind::Builtin]);
    }

    #[test]
    fn the_detail_is_searched_too() {
        let mut model = Model::new(entries());
        type_text(&mut model, "checkout");
        assert_eq!(names(&model), ["gco".to_owned()]);
    }

    #[test]
    fn backspace_widens_the_search_again() {
        let mut model = Model::new(entries());
        type_text(&mut model, "histz");
        assert_eq!(model.matches(), 0);
        model.handle_key(key(KeyCode::Backspace));
        assert_eq!(model.matches(), 2);
    }

    #[test]
    fn narrowing_gives_what_a_fresh_search_gives() {
        let mut typed = Model::new(entries());
        type_text(&mut typed, "git s");
        let mut pasted = Model::new(entries());
        pasted.paste("git s");
        assert_eq!(names(&typed), names(&pasted));
        assert_eq!(names(&typed), ["gs".to_owned()]);
    }

    #[test]
    fn enter_chooses_the_effect_of_the_selected_entry() {
        let mut model = Model::new(entries());
        assert_eq!(
            model.handle_key(key(KeyCode::Enter)),
            Step::Done(Effect::OpenSettings)
        );
        model.handle_key(key(KeyCode::Down));
        assert_eq!(
            model.handle_key(key(KeyCode::Tab)),
            Step::Done(Effect::OpenHistory)
        );
        type_text(&mut model, "gco");
        assert_eq!(
            model.handle_key(key(KeyCode::Enter)),
            Step::Done(Effect::Insert("git checkout".into()))
        );
    }

    #[test]
    fn enter_with_no_match_does_nothing() {
        let mut model = Model::new(entries());
        type_text(&mut model, "zzzz");
        assert_eq!(model.handle_key(key(KeyCode::Enter)), Step::Stay);
    }

    #[test]
    fn escape_ctrl_c_and_ctrl_g_leave() {
        let mut model = Model::new(entries());
        assert_eq!(model.handle_key(key(KeyCode::Esc)), Step::Cancel);
        assert_eq!(model.handle_key(ctrl('c')), Step::Cancel);
        assert_eq!(model.handle_key(ctrl('g')), Step::Cancel);
    }

    #[test]
    fn the_selection_moves_down_the_list_and_stays_inside_it() {
        let mut model = Model::new(entries());
        model.set_page(3);
        model.handle_key(key(KeyCode::Down));
        model.handle_key(ctrl('n'));
        assert_eq!(model.selected(), 2);
        model.handle_key(key(KeyCode::End));
        assert_eq!(model.selected(), 7);
        assert_eq!(model.offset(), 5);
        model.handle_key(key(KeyCode::Down));
        assert_eq!(model.selected(), 7);
        model.handle_key(ctrl('p'));
        model.handle_key(key(KeyCode::Up));
        assert_eq!(model.selected(), 5);
        model.handle_key(key(KeyCode::Home));
        assert_eq!((model.selected(), model.offset()), (0, 0));
        model.handle_key(key(KeyCode::PageDown));
        assert_eq!(model.selected(), 3);
    }

    #[test]
    fn a_new_query_starts_at_the_first_match() {
        let mut model = Model::new(entries());
        model.handle_key(key(KeyCode::End));
        type_text(&mut model, "g");
        assert_eq!(model.selected(), 0);
    }

    #[test]
    fn control_characters_never_get_into_the_query() {
        let mut model = Model::new(entries());
        model.paste("hi\nst\x1b[31m");
        assert!(!model.typed().chars().any(char::is_control));
    }

    #[test]
    fn a_key_release_does_nothing() {
        let mut model = Model::new(entries());
        let mut release = key(KeyCode::Enter);
        release.kind = KeyEventKind::Release;
        assert_eq!(model.handle_key(release), Step::Stay);
    }
}
