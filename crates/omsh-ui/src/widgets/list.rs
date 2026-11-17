//! A list of rows with `↑↓`, `Enter`, `Esc`, and (when asked) type-to-filter with the same fuzzy
//! matcher as the explorer. Rows that are too long are cut with an ellipsis; a list longer than
//! its window says `↑ n more` and `↓ n more`.

use crate::caps::Caps;
use crate::fuzzy::{Haystack, Query};
use crate::text::{Line, Style, Tone};
use crate::widgets::numbered::Numbered;
use crate::widgets::{Key, Step, Widget};

/// One row: a name, and a second part that is shown dim after it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListItem {
    /// The row's name.
    pub label: String,
    /// More about it (a path, a time). May be empty.
    pub detail: String,
}

impl ListItem {
    /// A row with a name and a detail.
    #[must_use]
    pub fn new(label: &str, detail: &str) -> Self {
        Self {
            label: label.to_owned(),
            detail: detail.to_owned(),
        }
    }
}

/// A list with a chosen row and a window.
#[derive(Clone, Debug)]
pub struct List {
    items: Vec<ListItem>,
    haystacks: Vec<Haystack>,
    filterable: bool,
    typed: String,
    /// Indices into `items`, best match first.
    shown: Vec<usize>,
    /// The chosen row, as an index into `shown`.
    cursor: usize,
    /// The first row of the window, as an index into `shown`.
    top: usize,
    rows: usize,
}

impl List {
    /// A list that shows at most `rows` rows at a time (at least one).
    #[must_use]
    pub fn new(items: Vec<ListItem>, rows: usize) -> Self {
        let haystacks = items
            .iter()
            .map(|item| Haystack::new(&format!("{} {}", item.label, item.detail)))
            .collect();
        let shown = (0..items.len()).collect();
        Self {
            items,
            haystacks,
            filterable: false,
            typed: String::new(),
            shown,
            cursor: 0,
            top: 0,
            rows: rows.max(1),
        }
    }

    /// This list with type-to-filter on.
    #[must_use]
    pub fn filterable(mut self) -> Self {
        self.filterable = true;
        self
    }

    /// What has been typed to filter.
    #[must_use]
    pub fn typed(&self) -> &str {
        &self.typed
    }

    /// The number of rows that match the filter.
    #[must_use]
    pub fn shown(&self) -> usize {
        self.shown.len()
    }

    /// The index (in the items given to [`List::new`]) of the chosen row, if there is one.
    #[must_use]
    pub fn chosen(&self) -> Option<usize> {
        self.shown.get(self.cursor).copied()
    }

    /// The same rows as a numbered question, with no default (there is no safe row).
    #[must_use]
    pub fn to_numbered(&self, prompt: &str) -> Numbered {
        let labels: Vec<String> = self
            .items
            .iter()
            .map(|item| {
                if item.detail.is_empty() {
                    item.label.clone()
                } else {
                    format!("{}  {}", item.label, item.detail)
                }
            })
            .collect();
        let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
        Numbered::from_labels(prompt, &labels)
    }

    fn refilter(&mut self) {
        let query = Query::parse(&self.typed);
        let mut scored: Vec<(i32, usize)> = self
            .haystacks
            .iter()
            .enumerate()
            .filter_map(|(index, haystack)| query.score(haystack).map(|score| (score, index)))
            .collect();
        // Best first; equal scores keep the order the rows were given in.
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        self.shown = scored.into_iter().map(|(_, index)| index).collect();
        self.cursor = 0;
        self.top = 0;
    }

    fn keep_chosen_in_the_window(&mut self) {
        if self.cursor < self.top {
            self.top = self.cursor;
        } else if self.cursor >= self.top + self.rows {
            self.top = self.cursor + 1 - self.rows;
        }
    }
}

impl Widget for List {
    type Output = usize;

    fn handle(&mut self, key: Key) -> Step<usize> {
        match key {
            Key::Up => {
                self.cursor = self.cursor.saturating_sub(1);
                self.keep_chosen_in_the_window();
            }
            Key::Down => {
                if self.cursor + 1 < self.shown.len() {
                    self.cursor += 1;
                }
                self.keep_chosen_in_the_window();
            }
            Key::Enter => {
                return match self.chosen() {
                    Some(index) => Step::Done(index),
                    None => Step::Stay,
                };
            }
            Key::Esc => return Step::Cancel,
            Key::Char(c) if self.filterable && !c.is_control() => {
                self.typed.push(c);
                self.refilter();
            }
            Key::Backspace if self.filterable => {
                self.typed.pop();
                self.refilter();
            }
            _ => {}
        }
        Step::Stay
    }

    fn render(&self, caps: &Caps, width: u16) -> Vec<Line> {
        let room = usize::from(width);
        let mut out = Vec::new();
        if self.filterable {
            let caret = if caps.unicode { "▏" } else { "_" };
            out.push(
                Line::new()
                    .tone(Tone::Dim, "filter ")
                    .tone(Tone::Plain, &self.typed)
                    .tone(Tone::Dim, caret)
                    .fit(room, caps.unicode),
            );
        }
        if self.shown.is_empty() {
            out.push(Line::new().tone(Tone::Dim, "no match"));
            return out;
        }
        let end = (self.top + self.rows).min(self.shown.len());
        if self.top > 0 {
            let arrow = if caps.unicode { "↑" } else { "^" };
            out.push(Line::new().tone(Tone::Dim, &format!("{arrow} {} more", self.top)));
        }
        let marker = if caps.unicode { "❯ " } else { "> " };
        for position in self.top..end {
            let item = &self.items[self.shown[position]];
            let chosen = position == self.cursor;
            let mut line = if chosen {
                Line::new().then(marker, Style::of(Tone::Key))
            } else {
                Line::plain("  ")
            };
            if chosen {
                // No dim text on the chosen row: dim on reverse video is hard to read (S28c).
                line.push(&item.label, Style::PLAIN.selected());
                if !item.detail.is_empty() {
                    line.push("  ", Style::PLAIN.selected());
                    line.push(&item.detail, Style::PLAIN.selected());
                }
            } else {
                line.push(&item.label, Style::PLAIN);
                if !item.detail.is_empty() {
                    line.push("  ", Style::PLAIN);
                    line.push(&item.detail, Style::of(Tone::Dim));
                }
            }
            out.push(line.fit(room, caps.unicode));
        }
        if end < self.shown.len() {
            let arrow = if caps.unicode { "↓" } else { "v" };
            out.push(Line::new().tone(
                Tone::Dim,
                &format!("{arrow} {} more", self.shown.len() - end),
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caps::{CapsFacts, Out};
    use omsh_config::Icons;

    fn caps(icons: Icons) -> Caps {
        let facts = CapsFacts {
            stdin_tty: true,
            stdout_tty: true,
            stderr_tty: true,
            utf8: true,
            ..CapsFacts::default()
        };
        Caps::decide(facts, Out::Stderr, icons, None)
    }

    fn items(n: usize) -> Vec<ListItem> {
        (0..n)
            .map(|i| ListItem::new(&format!("item {i}"), &format!("/tmp/{i}")))
            .collect()
    }

    fn shown(list: &List, icons: Icons, width: u16) -> Vec<String> {
        list.render(&caps(icons), width)
            .iter()
            .map(Line::plain_text)
            .collect()
    }

    #[test]
    fn up_and_down_move_the_chosen_row_and_stop_at_the_ends() {
        let mut list = List::new(items(3), 5);
        assert_eq!(list.chosen(), Some(0));
        list.handle(Key::Up);
        assert_eq!(list.chosen(), Some(0));
        list.handle(Key::Down);
        list.handle(Key::Down);
        list.handle(Key::Down);
        assert_eq!(list.chosen(), Some(2));
        assert_eq!(list.handle(Key::Enter), Step::Done(2));
    }

    #[test]
    fn esc_cancels_and_enter_on_an_empty_list_does_nothing() {
        assert_eq!(List::new(items(2), 5).handle(Key::Esc), Step::Cancel);
        let mut empty = List::new(vec![], 5);
        assert_eq!(empty.handle(Key::Enter), Step::Stay);
        assert_eq!(shown(&empty, Icons::Auto, 40), ["no match"]);
    }

    #[test]
    fn the_chosen_row_has_a_marker_and_the_window_scrolls_with_counts() {
        let mut list = List::new(items(6), 3);
        assert_eq!(
            shown(&list, Icons::Auto, 40),
            [
                "❯ item 0  /tmp/0",
                "  item 1  /tmp/1",
                "  item 2  /tmp/2",
                "↓ 3 more",
            ]
        );
        for _ in 0..4 {
            list.handle(Key::Down);
        }
        assert_eq!(
            shown(&list, Icons::Auto, 40),
            [
                "↑ 2 more",
                "  item 2  /tmp/2",
                "  item 3  /tmp/3",
                "❯ item 4  /tmp/4",
                "↓ 1 more",
            ]
        );
        list.handle(Key::Down);
        assert_eq!(
            shown(&list, Icons::Auto, 40).last().map(String::as_str),
            Some("❯ item 5  /tmp/5")
        );
        for _ in 0..5 {
            list.handle(Key::Up);
        }
        assert_eq!(shown(&list, Icons::Auto, 40)[0], "❯ item 0  /tmp/0");
    }

    #[test]
    fn the_ascii_list_uses_ascii_markers() {
        let mut list = List::new(items(4), 2);
        list.handle(Key::Down);
        list.handle(Key::Down);
        let lines = shown(&list, Icons::Ascii, 40);
        assert_eq!(
            lines,
            [
                "^ 1 more",
                "  item 1  /tmp/1",
                "> item 2  /tmp/2",
                "v 1 more"
            ]
        );
        assert!(lines.iter().all(|line| line.is_ascii()));
    }

    #[test]
    fn the_chosen_row_has_no_dim_text_and_the_others_do() {
        let list = List::new(items(2), 5);
        let lines = list.render(&caps(Icons::Auto), 40);
        assert!(lines[0].spans().iter().all(|s| s.style.tone != Tone::Dim));
        assert!(lines[0].spans().iter().any(|s| s.style.selected));
        assert!(lines[1].spans().iter().any(|s| s.style.tone == Tone::Dim));
    }

    #[test]
    fn typing_filters_with_the_fuzzy_matcher_and_backspace_widens_it() {
        let rows = vec![
            ListItem::new("notes.txt", "/home/a"),
            ListItem::new("report.pdf", "/home/b"),
            ListItem::new("rep.log", "/var/log"),
        ];
        let mut list = List::new(rows, 5).filterable();
        for c in "rep".chars() {
            list.handle(Key::Char(c));
        }
        assert_eq!(list.typed(), "rep");
        assert_eq!(list.shown(), 2);
        assert_eq!(list.chosen().map(|i| i < 3), Some(true));
        let lines = shown(&list, Icons::Auto, 40);
        assert_eq!(lines[0], "filter rep▏");
        assert!(lines[1].starts_with("❯ "));
        list.handle(Key::Backspace);
        list.handle(Key::Backspace);
        list.handle(Key::Backspace);
        assert_eq!(list.shown(), 3);
        assert_eq!(list.typed(), "");
    }

    #[test]
    fn a_filter_that_matches_nothing_says_so_and_enter_does_nothing() {
        let mut list = List::new(items(3), 5).filterable();
        for c in "zzzz".chars() {
            list.handle(Key::Char(c));
        }
        assert_eq!(list.shown(), 0);
        assert_eq!(shown(&list, Icons::Auto, 40), ["filter zzzz▏", "no match"]);
        assert_eq!(list.handle(Key::Enter), Step::Stay);
    }

    #[test]
    fn a_list_that_is_not_filterable_ignores_typing_and_the_answer_is_the_original_index() {
        let mut list = List::new(items(3), 5);
        list.handle(Key::Char('x'));
        assert_eq!(list.typed(), "");
        assert_eq!(list.shown(), 3);
        let rows = vec![
            ListItem::new("alpha", ""),
            ListItem::new("beta", ""),
            ListItem::new("gamma", ""),
        ];
        let mut filtered = List::new(rows, 5).filterable();
        for c in "gam".chars() {
            filtered.handle(Key::Char(c));
        }
        assert_eq!(
            filtered.handle(Key::Enter),
            Step::Done(2),
            "gamma is item 2 of the original"
        );
    }

    #[test]
    fn long_rows_are_cut_and_bangla_names_stay_inside_the_width() {
        let rows = vec![
            ListItem::new("রিপোর্ট.txt", "/home/user/নথি/ছবি/অনেক/লম্বা/পথ"),
            ListItem::new(&"x".repeat(80), "d"),
        ];
        let list = List::new(rows, 5);
        for width in [10u16, 16, 24, 40] {
            for icons in [Icons::Auto, Icons::Ascii] {
                for line in list.render(&caps(icons), width) {
                    assert!(
                        line.width() <= usize::from(width),
                        "{width}: {:?}",
                        line.plain_text()
                    );
                }
            }
        }
    }

    #[test]
    fn the_numbered_form_lists_every_row_with_no_default() {
        let list = List::new(vec![ListItem::new("a", "x"), ListItem::new("b", "")], 5);
        let numbered = list.to_numbered("Restore which?");
        assert_eq!(
            numbered.render(),
            ["Restore which?", "  1) a  x", "  2) b", "Choose 1-2: "]
        );
    }

    #[test]
    fn a_control_character_typed_into_the_filter_is_ignored() {
        let mut list = List::new(items(2), 5).filterable();
        list.handle(Key::Char('\x1b'));
        list.handle(Key::Char('\n'));
        assert_eq!(list.typed(), "");
    }
}
