//! Keyboard widgets for inline menus (UI upgrade, U1): [`buttons::Buttons`], a filterable
//! [`list::List`], and [`numbered::Numbered`], the fallback for when nobody can press arrow keys
//! (not a terminal, `-c`, tests, `TERM=dumb`).
//!
//! A widget is state, a `handle` for one [`Key`] and a `render` that returns [`Line`]s. It never
//! touches the terminal: [`crate::inline`] draws it and reads the keys. The answer type is the
//! shared [`Step`] of the full-screen views.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::caps::Caps;
use crate::text::Line;

pub mod buttons;
pub mod list;
pub mod numbered;

pub use crate::view::run::Step;

/// The keys the widgets know. Anything else is [`None`] from [`Key::from_crossterm`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// Arrow up.
    Up,
    /// Arrow down.
    Down,
    /// Arrow left.
    Left,
    /// Arrow right.
    Right,
    /// Tab.
    Tab,
    /// Shift+Tab.
    BackTab,
    /// Enter.
    Enter,
    /// Esc, and Ctrl+C: leave without choosing.
    Esc,
    /// Backspace.
    Backspace,
    /// A character, with no Ctrl or Alt held.
    Char(char),
}

impl Key {
    /// The widget key for a terminal key event. A key release or a key with Ctrl or Alt held is
    /// not a widget key (so that `Ctrl+T` is not taken for the shortcut `t`), except Ctrl+C,
    /// which leaves like Esc.
    #[must_use]
    pub fn from_crossterm(event: KeyEvent) -> Option<Key> {
        if event.kind == KeyEventKind::Release {
            return None;
        }
        let held = event.modifiers;
        if held.contains(KeyModifiers::CONTROL) {
            return matches!(event.code, KeyCode::Char('c' | 'C')).then_some(Key::Esc);
        }
        if held.contains(KeyModifiers::ALT) {
            return None;
        }
        match event.code {
            KeyCode::Up => Some(Key::Up),
            KeyCode::Down => Some(Key::Down),
            KeyCode::Left => Some(Key::Left),
            KeyCode::Right => Some(Key::Right),
            KeyCode::Tab => Some(Key::Tab),
            KeyCode::BackTab => Some(Key::BackTab),
            KeyCode::Enter => Some(Key::Enter),
            KeyCode::Esc => Some(Key::Esc),
            KeyCode::Backspace => Some(Key::Backspace),
            KeyCode::Char(c) => Some(Key::Char(c)),
            _ => None,
        }
    }
}

/// Something [`crate::inline::run`] can show and steer.
pub trait Widget {
    /// What the person chose.
    type Output;

    /// One key.
    fn handle(&mut self, key: Key) -> Step<Self::Output>;

    /// The lines to show, each at most `width` columns.
    fn render(&self, caps: &Caps, width: u16) -> Vec<Line>;

    /// True while something on screen moves (a spinner, an easing number). While it is false the
    /// menu waits for a key with no timer, so an idle menu costs no CPU.
    fn animating(&self) -> bool {
        false
    }

    /// Called before every drawing with the time since the menu opened, so that an animated
    /// widget can move. Does nothing by default.
    fn tick(&mut self, _since_open: std::time::Duration) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn plain_keys_map_to_widget_keys() {
        let none = KeyModifiers::NONE;
        assert_eq!(Key::from_crossterm(event(KeyCode::Up, none)), Some(Key::Up));
        assert_eq!(
            Key::from_crossterm(event(KeyCode::Enter, none)),
            Some(Key::Enter)
        );
        assert_eq!(
            Key::from_crossterm(event(KeyCode::Esc, none)),
            Some(Key::Esc)
        );
        assert_eq!(
            Key::from_crossterm(event(KeyCode::Char('t'), none)),
            Some(Key::Char('t'))
        );
        assert_eq!(
            Key::from_crossterm(event(KeyCode::Char('T'), KeyModifiers::SHIFT)),
            Some(Key::Char('T'))
        );
        assert_eq!(
            Key::from_crossterm(event(KeyCode::BackTab, KeyModifiers::SHIFT)),
            Some(Key::BackTab)
        );
        assert_eq!(Key::from_crossterm(event(KeyCode::F(5), none)), None);
    }

    #[test]
    fn ctrl_and_alt_are_not_shortcuts_except_ctrl_c_which_leaves() {
        assert_eq!(
            Key::from_crossterm(event(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(Key::Esc)
        );
        assert_eq!(
            Key::from_crossterm(event(KeyCode::Char('t'), KeyModifiers::CONTROL)),
            None
        );
        assert_eq!(
            Key::from_crossterm(event(KeyCode::Char('y'), KeyModifiers::ALT)),
            None
        );
    }

    #[test]
    fn a_key_release_is_ignored() {
        let mut release = event(KeyCode::Enter, KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        assert_eq!(Key::from_crossterm(release), None);
    }
}
