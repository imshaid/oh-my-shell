//! A filterable list for `omsh ai setup` (spec section 5, "AI setup"): the provider list and the
//! model list. Type to filter, `↑↓` move, `Enter` chooses, `Esc` cancels.
//!
//! The model list can have two lists behind it: the chat models, shown first, and all of them.
//! `Tab` switches, and the line under the list says which one is on screen and how many there
//! are. An open-right panel: model names come from a server.

use crate::boxes::{Panel, open};
use crate::caps::Caps;
use crate::panels::MAX_WIDTH;
use crate::text::{Line, Style, Tone};
use crate::widgets::list::{List, ListItem};
use crate::widgets::{Key, Step, Widget};

/// Rows of the list shown at once.
const ROWS: usize = 10;

/// The list. Its answer is the label of the row that was chosen.
pub struct Choose {
    title: String,
    intro: Vec<Line>,
    items: Vec<ListItem>,
    /// The longer list behind `Tab`, when there is one.
    all: Option<Vec<ListItem>>,
    showing_all: bool,
    list: List,
    noun: String,
}

impl Choose {
    /// A list of `items` under `title`. `noun` is what they are (`models`), for the line under
    /// the list. The row whose label is `current` is chosen first, if there is one.
    #[must_use]
    pub fn new(title: &str, noun: &str, items: Vec<ListItem>, current: Option<&str>) -> Self {
        let list = Self::make(&items, current);
        Self {
            title: title.to_owned(),
            intro: Vec::new(),
            items,
            all: None,
            showing_all: false,
            list,
            noun: noun.to_owned(),
        }
    }

    /// Lines of text above the list.
    #[must_use]
    pub fn intro(mut self, lines: Vec<Line>) -> Self {
        self.intro = lines;
        self
    }

    /// A longer list that `Tab` switches to (every model, when `items` holds the chat models).
    #[must_use]
    pub fn with_all(mut self, all: Vec<ListItem>) -> Self {
        self.all = Some(all);
        self
    }

    fn make(items: &[ListItem], current: Option<&str>) -> List {
        let mut list = List::new(items.to_vec(), ROWS).filterable();
        if let Some(index) = current.and_then(|name| items.iter().position(|i| i.label == name)) {
            list = list.start_at(index);
        }
        list
    }

    fn rows(&self) -> &[ListItem] {
        match (&self.all, self.showing_all) {
            (Some(all), true) => all,
            _ => &self.items,
        }
    }

    /// True while the longer list is shown.
    #[must_use]
    pub const fn showing_all(&self) -> bool {
        self.showing_all
    }
}

impl Widget for Choose {
    type Output = String;

    fn handle(&mut self, key: Key) -> Step<String> {
        if key == Key::Tab && self.all.is_some() {
            self.showing_all = !self.showing_all;
            self.list = Self::make(self.rows(), None);
            return Step::Stay;
        }
        match self.list.handle(key) {
            Step::Done(index) => self
                .rows()
                .get(index)
                .map_or(Step::Stay, |item| Step::Done(item.label.clone())),
            Step::Stay => Step::Stay,
            Step::Cancel => Step::Cancel,
        }
    }

    fn render(&self, caps: &Caps, width: u16) -> Vec<Line> {
        let width = width.min(MAX_WIDTH);
        let inner = width.saturating_sub(2);
        let vertical = if caps.unicode { "↑↓" } else { "Up/Down" };
        let mut panel = Panel::new(&self.title)
            .tone(Tone::Ai)
            .hint(vertical, "move")
            .hint("Enter", "choose")
            .hint("Esc", "cancel");
        if self.all.is_some() {
            panel = panel.hint(
                "Tab",
                if self.showing_all {
                    "chat models"
                } else {
                    "show all"
                },
            );
        }
        panel = panel.lines(self.intro.iter().cloned());
        panel = panel.lines(self.list.render(caps, inner));
        let rows = self.rows().len();
        let which = match (&self.all, self.showing_all) {
            (Some(all), false) => {
                format!("{rows} {} that chat, of {} in all", self.noun, all.len())
            }
            (Some(_), true) => format!("all {rows} {}", self.noun),
            (None, _) => format!("{rows} {}", self.noun),
        };
        panel = panel.line(Line::new().then(&which, Style::of(Tone::Dim)));
        open(&panel, caps, width)
    }
}

#[cfg(test)]
mod tests;
