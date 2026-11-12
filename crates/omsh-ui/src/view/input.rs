//! The text a person types into a view, and where the cursor is in it.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// A line of text with a cursor (a count of characters). Control characters are never kept.
#[derive(Clone, Debug, Default)]
pub struct QueryLine {
    text: String,
    cursor: usize,
}

impl QueryLine {
    /// Starts with `text` (control characters dropped), the cursor at the end.
    #[must_use]
    pub fn new(text: &str) -> Self {
        let text: String = text.chars().filter(|c| !c.is_control()).collect();
        let cursor = text.chars().count();
        Self { text, cursor }
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The cursor, as a number of characters from the start of the text.
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Text pasted (bracketed paste). Line ends and other control characters are dropped.
    /// True if the text changed.
    pub fn paste(&mut self, text: &str) -> bool {
        let clean: String = text.chars().filter(|c| !c.is_control()).collect();
        self.insert(&clean)
    }

    /// Handles a key that edits the line or moves the cursor. `None`: not an editing key, the
    /// caller decides. `Some(changed)`: it was one, and `changed` says whether the text did.
    pub fn edit(&mut self, key: KeyEvent) -> Option<bool> {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Left => {
                self.cursor = self.cursor.saturating_sub(1);
                Some(false)
            }
            KeyCode::Right => {
                self.cursor = (self.cursor + 1).min(self.text.chars().count());
                Some(false)
            }
            KeyCode::Char('a') if control => {
                self.cursor = 0;
                Some(false)
            }
            KeyCode::Char('e') if control => {
                self.cursor = self.text.chars().count();
                Some(false)
            }
            KeyCode::Char('u') if control => Some(self.delete_range(0, self.cursor)),
            KeyCode::Char('w') if control => {
                let start = self.word_start();
                Some(self.delete_range(start, self.cursor))
            }
            KeyCode::Backspace => Some(if self.cursor > 0 {
                self.delete_range(self.cursor - 1, self.cursor)
            } else {
                false
            }),
            KeyCode::Delete => Some(if self.cursor < self.text.chars().count() {
                self.delete_range(self.cursor, self.cursor + 1)
            } else {
                false
            }),
            KeyCode::Char(c) if !control && !alt && !c.is_control() => {
                let mut one = [0; 4];
                Some(self.insert(c.encode_utf8(&mut one)))
            }
            _ => None,
        }
    }

    fn insert(&mut self, text: &str) -> bool {
        if text.is_empty() {
            return false;
        }
        let at = self.byte_of(self.cursor);
        self.text.insert_str(at, text);
        self.cursor += text.chars().count();
        true
    }

    fn delete_range(&mut self, from: usize, to: usize) -> bool {
        if from >= to {
            return false;
        }
        let (start, end) = (self.byte_of(from), self.byte_of(to));
        self.text.replace_range(start..end, "");
        self.cursor = from;
        true
    }

    fn byte_of(&self, chars: usize) -> usize {
        self.text
            .char_indices()
            .nth(chars)
            .map_or(self.text.len(), |(byte, _)| byte)
    }

    /// The start of the word before the cursor (blanks, then the letters before them).
    fn word_start(&self) -> usize {
        let before: Vec<char> = self.text.chars().take(self.cursor).collect();
        let mut at = before.len();
        while at > 0 && before[at - 1].is_whitespace() {
            at -= 1;
        }
        while at > 0 && !before[at - 1].is_whitespace() {
            at -= 1;
        }
        at
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn type_text(line: &mut QueryLine, text: &str) {
        for c in text.chars() {
            line.edit(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn typing_inserts_at_the_cursor_and_says_the_text_changed() {
        let mut line = QueryLine::new("ac");
        line.edit(key(KeyCode::Left));
        assert_eq!(line.edit(key(KeyCode::Char('b'))), Some(true));
        assert_eq!((line.text(), line.cursor()), ("abc", 2));
    }

    #[test]
    fn moving_does_not_change_the_text() {
        let mut line = QueryLine::new("ab");
        assert_eq!(line.edit(key(KeyCode::Left)), Some(false));
        assert_eq!(line.edit(ctrl('a')), Some(false));
        assert_eq!(line.cursor(), 0);
        assert_eq!(line.edit(ctrl('e')), Some(false));
        assert_eq!(line.cursor(), 2);
        assert_eq!(line.edit(key(KeyCode::Right)), Some(false));
        assert_eq!(line.cursor(), 2);
    }

    #[test]
    fn the_delete_keys() {
        let mut line = QueryLine::new("dock er");
        assert_eq!(line.edit(ctrl('w')), Some(true));
        assert_eq!(line.text(), "dock ");
        assert_eq!(line.edit(ctrl('w')), Some(true));
        assert_eq!(line.text(), "");
        assert_eq!(line.edit(key(KeyCode::Backspace)), Some(false));
        let mut line = QueryLine::new("abc");
        line.edit(ctrl('a'));
        assert_eq!(line.edit(key(KeyCode::Delete)), Some(true));
        assert_eq!(line.text(), "bc");
        line.edit(key(KeyCode::Right));
        assert_eq!(line.edit(ctrl('u')), Some(true));
        assert_eq!((line.text(), line.cursor()), ("c", 0));
    }

    #[test]
    fn other_keys_are_left_to_the_caller() {
        let mut line = QueryLine::default();
        assert_eq!(line.edit(key(KeyCode::Enter)), None);
        assert_eq!(line.edit(key(KeyCode::Up)), None);
        assert_eq!(line.edit(ctrl('r')), None);
        let alt = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT);
        assert_eq!(line.edit(alt), None);
        assert_eq!(line.text(), "");
    }

    #[test]
    fn control_characters_never_get_in() {
        let mut line = QueryLine::new("a\x1b[31mb");
        assert!(!line.text().chars().any(char::is_control));
        assert!(line.paste("x\ny\x07"));
        assert!(!line.text().chars().any(char::is_control));
        assert!(line.text().ends_with("xy"));
        assert!(!line.paste("\n"));
    }

    #[test]
    fn wide_characters_move_by_character() {
        let mut line = QueryLine::default();
        type_text(&mut line, "aöb");
        line.edit(key(KeyCode::Left));
        line.edit(key(KeyCode::Backspace));
        assert_eq!(line.text(), "ab");
    }
}
