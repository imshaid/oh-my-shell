//! A row of buttons: `←→` or `Tab` move, `Enter` confirms, `Esc` cancels, and a letter picks a
//! button at once. The chosen button is drawn in reverse video, the others as `[label]`.
//!
//! The button that starts chosen is the safe one: a confirmation says `[y/N]` with the answer
//! that does nothing as the default, so `Enter` at once must do nothing dangerous.

use crate::caps::Caps;
use crate::text::{Line, Style, Tone};
use crate::widgets::numbered::{Numbered, NumberedOption};
use crate::widgets::{Key, Step, Widget};

/// One button.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Button {
    /// What it says.
    pub label: String,
    /// The letter that picks it.
    pub shortcut: char,
}

/// A row of buttons with one chosen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Buttons {
    items: Vec<Button>,
    default: usize,
    chosen: usize,
}

/// The space between two buttons in a row.
const GAP: &str = "  ";

impl Buttons {
    /// Buttons from `(label, shortcut)` pairs. `default` is the button that starts chosen (the
    /// safe one); a number past the end means the last button. If two buttons have the same
    /// letter (in any case), the first one owns it.
    #[must_use]
    pub fn new(items: &[(&str, char)], default: usize) -> Self {
        let items: Vec<Button> = items
            .iter()
            .map(|(label, shortcut)| Button {
                label: (*label).to_owned(),
                shortcut: *shortcut,
            })
            .collect();
        let default = default.min(items.len().saturating_sub(1));
        Self {
            items,
            default,
            chosen: default,
        }
    }

    /// The index of the chosen button.
    #[must_use]
    pub fn chosen(&self) -> usize {
        self.chosen
    }

    /// The buttons.
    #[must_use]
    pub fn items(&self) -> &[Button] {
        &self.items
    }

    /// The same buttons as a numbered list, for when nobody can press an arrow key. The
    /// default is kept: an empty answer picks the safe button.
    #[must_use]
    pub fn to_numbered(&self, prompt: &str) -> Numbered {
        Numbered::new(
            prompt,
            self.items
                .iter()
                .map(|button| NumberedOption {
                    label: button.label.clone(),
                    shortcut: Some(button.shortcut),
                })
                .collect(),
            Some(self.default),
        )
    }

    fn step(&mut self, forward: bool) {
        let count = self.items.len();
        if count == 0 {
            return;
        }
        self.chosen = if forward {
            (self.chosen + 1) % count
        } else {
            (self.chosen + count - 1) % count
        };
    }

    /// The button a letter picks: the first whose letter matches in any case.
    fn by_letter(&self, c: char) -> Option<usize> {
        self.items
            .iter()
            .position(|button| button.shortcut.to_lowercase().eq(c.to_lowercase()))
    }

    /// One button: `[label]`, or the label with a blank on each side in reverse video when it is
    /// the chosen one. The shortcut letter is underlined where the label has it.
    fn button(&self, index: usize) -> Line {
        let button = &self.items[index];
        let chosen = index == self.chosen;
        let base = if chosen {
            Style::PLAIN.selected()
        } else {
            Style::of(Tone::Plain)
        };
        let mut line = Line::new().then(if chosen { " " } else { "[" }, base);
        let mut underlined = false;
        for c in button.label.chars() {
            let mut one = [0u8; 4];
            let text: &str = c.encode_utf8(&mut one);
            let is_letter = !underlined && c.to_lowercase().eq(button.shortcut.to_lowercase());
            if is_letter {
                underlined = true;
                line.push(text, base.underlined());
            } else {
                line.push(text, base);
            }
        }
        line.push(if chosen { " " } else { "]" }, base);
        line
    }
}

impl Widget for Buttons {
    type Output = usize;

    fn handle(&mut self, key: Key) -> Step<usize> {
        if self.items.is_empty() {
            return match key {
                Key::Enter | Key::Esc => Step::Cancel,
                _ => Step::Stay,
            };
        }
        match key {
            Key::Right | Key::Tab | Key::Down => self.step(true),
            Key::Left | Key::BackTab | Key::Up => self.step(false),
            Key::Enter => return Step::Done(self.chosen),
            Key::Esc => return Step::Cancel,
            Key::Char(c) => {
                if let Some(index) = self.by_letter(c) {
                    self.chosen = index;
                    return Step::Done(index);
                }
            }
            Key::Backspace => {}
        }
        Step::Stay
    }

    /// One row when it fits in `width`; otherwise one button to a row.
    fn render(&self, _caps: &Caps, width: u16) -> Vec<Line> {
        let mut row = Line::new();
        for index in 0..self.items.len() {
            if index > 0 {
                row.push(GAP, Style::PLAIN);
            }
            row.append(&self.button(index));
        }
        if row.width() <= usize::from(width) {
            return vec![row];
        }
        (0..self.items.len())
            .map(|index| self.button(index))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caps::{CapsFacts, Out};
    use omsh_config::Icons;

    fn caps() -> Caps {
        let facts = CapsFacts {
            stdin_tty: true,
            stdout_tty: true,
            stderr_tty: true,
            utf8: true,
            ..CapsFacts::default()
        };
        Caps::decide(facts, Out::Stderr, Icons::Auto, None)
    }

    fn run_it() -> Buttons {
        Buttons::new(
            &[("Move to trash", 't'), ("Run anyway", 'y'), ("Cancel", 'n')],
            2,
        )
    }

    fn row(buttons: &Buttons, width: u16) -> Vec<String> {
        buttons
            .render(&caps(), width)
            .iter()
            .map(Line::plain_text)
            .collect()
    }

    #[test]
    fn the_safe_button_starts_chosen_and_enter_picks_it() {
        let mut buttons = run_it();
        assert_eq!(buttons.chosen(), 2);
        assert_eq!(buttons.handle(Key::Enter), Step::Done(2));
    }

    #[test]
    fn the_chosen_button_is_reversed_and_the_others_are_in_brackets() {
        let buttons = run_it();
        assert_eq!(
            row(&buttons, 80),
            ["[Move to trash]  [Run anyway]   Cancel "]
        );
        let lines = buttons.render(&caps(), 80);
        let selected: Vec<bool> = lines[0].spans().iter().map(|s| s.style.selected).collect();
        assert!(selected.contains(&true) && selected.contains(&false));
        let reversed: String = lines[0]
            .spans()
            .iter()
            .filter(|s| s.style.selected)
            .map(|s| s.text())
            .collect();
        assert_eq!(reversed, " Cancel ");
    }

    #[test]
    fn the_shortcut_letter_is_underlined_in_its_label_once() {
        let buttons = run_it();
        let line = &buttons.render(&caps(), 80)[0];
        let underlined: Vec<&str> = line
            .spans()
            .iter()
            .filter(|s| s.style.underline)
            .map(|s| s.text())
            .collect();
        // t in "Move to trash" (the first t), y in "Run anyway" (the first y), n in "Cancel".
        assert_eq!(underlined, ["t", "y", "n"]);
        assert_eq!(
            line.spans()
                .iter()
                .filter(|s| s.style.underline && s.text() == "t")
                .count(),
            1
        );
    }

    #[test]
    fn arrows_and_tab_move_and_wrap() {
        let mut buttons = run_it();
        assert_eq!(buttons.handle(Key::Right), Step::Stay);
        assert_eq!(buttons.chosen(), 0, "wraps from the last to the first");
        buttons.handle(Key::Tab);
        assert_eq!(buttons.chosen(), 1);
        buttons.handle(Key::Left);
        buttons.handle(Key::Left);
        assert_eq!(buttons.chosen(), 2, "wraps from the first to the last");
        buttons.handle(Key::BackTab);
        assert_eq!(buttons.chosen(), 1);
        assert_eq!(buttons.handle(Key::Enter), Step::Done(1));
    }

    #[test]
    fn a_letter_picks_at_once_in_either_case() {
        let mut lower = run_it();
        assert_eq!(lower.handle(Key::Char('t')), Step::Done(0));
        let mut upper = run_it();
        assert_eq!(upper.handle(Key::Char('Y')), Step::Done(1));
        let mut other = run_it();
        assert_eq!(other.handle(Key::Char('x')), Step::Stay);
        assert_eq!(other.chosen(), 2, "an unknown letter changes nothing");
    }

    #[test]
    fn esc_cancels() {
        assert_eq!(run_it().handle(Key::Esc), Step::Cancel);
    }

    #[test]
    fn two_buttons_with_the_same_letter_leave_it_to_the_first() {
        let mut buttons = Buttons::new(&[("Yes", 'y'), ("Yet more", 'Y')], 0);
        assert_eq!(buttons.handle(Key::Char('y')), Step::Done(0));
    }

    #[test]
    fn a_default_past_the_end_is_the_last_button_and_no_buttons_cancel() {
        let buttons = Buttons::new(&[("a", 'a'), ("b", 'b')], 9);
        assert_eq!(buttons.chosen(), 1);
        let mut none = Buttons::new(&[], 0);
        assert_eq!(none.handle(Key::Enter), Step::Cancel);
        assert_eq!(none.handle(Key::Right), Step::Stay);
        assert_eq!(none.render(&caps(), 80).len(), 1);
    }

    #[test]
    fn a_row_that_is_too_wide_becomes_one_button_to_a_line() {
        let buttons = run_it();
        let lines = row(&buttons, 30);
        assert_eq!(lines, ["[Move to trash]", "[Run anyway]", " Cancel "]);
    }

    #[test]
    fn the_numbered_form_keeps_the_safe_default_and_the_letters() {
        let numbered = run_it().to_numbered("Run it?");
        assert_eq!(
            numbered.parse(""),
            crate::widgets::numbered::Answer::Choice(2)
        );
        assert_eq!(
            numbered.parse("Y"),
            crate::widgets::numbered::Answer::Choice(1)
        );
        assert_eq!(
            numbered.parse("1"),
            crate::widgets::numbered::Answer::Choice(0)
        );
    }
}
