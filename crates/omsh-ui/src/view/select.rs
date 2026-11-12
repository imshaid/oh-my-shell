//! The chosen row of a list, and the window of rows that is on the screen.

/// A chosen row and the row at the edge of the screen, both counted from the first row of the
/// list (0). The caller says how long the list is.
#[derive(Clone, Copy, Debug)]
pub struct Selection {
    selected: usize,
    offset: usize,
    page: usize,
}

impl Default for Selection {
    fn default() -> Self {
        Self::new()
    }
}

impl Selection {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            selected: 0,
            offset: 0,
            page: 10,
        }
    }

    /// The chosen row.
    #[must_use]
    pub const fn selected(&self) -> usize {
        self.selected
    }

    /// The first row on the screen.
    #[must_use]
    pub const fn offset(&self) -> usize {
        self.offset
    }

    #[must_use]
    pub const fn page(&self) -> usize {
        self.page
    }

    /// Back to the first row (a new list).
    pub const fn reset(&mut self) {
        self.selected = 0;
        self.offset = 0;
    }

    /// Tells it how many rows the screen has room for.
    pub fn set_page(&mut self, rows: usize) {
        self.page = rows.max(1);
        self.keep_visible();
    }

    /// Moves away from the first row by `by` rows, not past the last of `len`.
    pub fn forward(&mut self, by: usize, len: usize) {
        self.selected = self.selected.saturating_add(by).min(len.saturating_sub(1));
        self.keep_visible();
    }

    /// Moves toward the first row by `by` rows.
    pub fn back(&mut self, by: usize) {
        self.selected = self.selected.saturating_sub(by);
        self.keep_visible();
    }

    fn keep_visible(&mut self) {
        if self.selected < self.offset {
            self.offset = self.selected;
        } else if self.selected >= self.offset + self.page {
            self.offset = self.selected + 1 - self.page;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_stays_inside_the_list() {
        let mut s = Selection::new();
        s.forward(100, 5);
        assert_eq!(s.selected(), 4);
        s.back(100);
        assert_eq!(s.selected(), 0);
        s.forward(1, 0);
        assert_eq!(s.selected(), 0);
    }

    #[test]
    fn the_window_follows_the_chosen_row() {
        let mut s = Selection::new();
        s.set_page(3);
        s.forward(4, 10);
        assert_eq!((s.selected(), s.offset()), (4, 2));
        s.back(3);
        assert_eq!((s.selected(), s.offset()), (1, 1));
        s.back(1);
        assert_eq!((s.selected(), s.offset()), (0, 0));
        s.forward(5, 10);
        s.reset();
        assert_eq!((s.selected(), s.offset()), (0, 0));
    }

    #[test]
    fn a_smaller_screen_keeps_the_chosen_row_in_view() {
        let mut s = Selection::new();
        s.forward(8, 10);
        s.set_page(4);
        assert_eq!((s.selected(), s.offset()), (8, 5));
        s.set_page(0);
        assert_eq!(s.page(), 1);
    }
}
