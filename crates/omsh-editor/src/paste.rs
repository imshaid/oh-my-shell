//! Bracketed paste: pasted text goes into the line, it is not run.
//!
//! With bracketed paste on, the terminal wraps pasted text in two marks, and `reedline` reads
//! it as one event and inserts it. [`PasteMode`] sits around the real edit mode (emacs or vi)
//! and changes one thing: what is inserted is first cleaned by [`clean_paste`]. Everything else
//! is passed on.
//!
//! The cleaning matters because a paste is text from somewhere else. A control character in it
//! (an escape, say) must not reach the line, where it could hide part of a command from the
//! person who is about to press Enter.

use crossterm::event::Event;
use reedline::{EditCommand, EditMode, PromptEditMode, ReedlineEvent, ReedlineRawEvent};

/// The text to insert for a paste: line endings made `\n`, control characters other than the
/// line feed and the tab dropped, and the line feeds at the end removed, so that the pasted
/// text waits in the line for one Enter.
pub(crate) fn clean_paste(text: &str) -> String {
    let unified = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut cleaned: String = unified
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect();
    cleaned.truncate(cleaned.trim_end_matches('\n').len());
    cleaned
}

/// An edit mode that cleans what is pasted.
pub(crate) struct PasteMode {
    inner: Box<dyn EditMode>,
}

impl PasteMode {
    pub(crate) fn new(inner: Box<dyn EditMode>) -> Self {
        Self { inner }
    }
}

impl EditMode for PasteMode {
    fn parse_event(&mut self, event: ReedlineRawEvent) -> ReedlineEvent {
        match Event::from(event) {
            Event::Paste(body) => {
                ReedlineEvent::Edit(vec![EditCommand::InsertString(clean_paste(&body))])
            }
            other => match ReedlineRawEvent::try_from(other) {
                Ok(event) => self.inner.parse_event(event),
                Err(()) => ReedlineEvent::None,
            },
        }
    }

    fn edit_mode(&self) -> PromptEditMode {
        self.inner.edit_mode()
    }

    // `handle_mode_specific_event` is not passed on: its type is not public in `reedline`, and
    // it only serves `SwitchMode`, which no binding of this shell uses (vi moves between its
    // insert and normal state by itself).
}

#[cfg(test)]
mod tests {
    use reedline::Emacs;

    use super::*;

    #[test]
    fn line_endings_become_line_feeds_and_the_end_is_trimmed() {
        assert_eq!(clean_paste("a\r\nb\rc\n\n"), "a\nb\nc");
        assert_eq!(clean_paste("ls\n"), "ls");
        assert_eq!(clean_paste("\n\n"), "");
        assert_eq!(clean_paste(""), "");
    }

    #[test]
    fn control_characters_are_dropped_but_tabs_and_line_feeds_stay() {
        assert_eq!(clean_paste("a\x1b[31mb\x07\x00c"), "a[31mbc");
        assert_eq!(clean_paste("a\tb\nc"), "a\tb\nc");
        // The end mark of a bracketed paste, if a pasted text carries one.
        assert_eq!(clean_paste("rm x\x1b[201~; ls"), "rm x[201~; ls");
    }

    #[test]
    fn text_in_other_scripts_is_kept() {
        assert_eq!(clean_paste("echo héllo 日本語\n"), "echo héllo 日本語");
    }

    fn parse(mode: &mut PasteMode, event: Event) -> ReedlineEvent {
        mode.parse_event(ReedlineRawEvent::try_from(event).unwrap())
    }

    #[test]
    fn a_paste_is_inserted_cleaned_and_other_events_reach_the_real_mode() {
        let mut mode = PasteMode::new(Box::new(Emacs::default()));
        assert_eq!(
            parse(&mut mode, Event::Paste("a\x1b b\n".to_owned())),
            ReedlineEvent::Edit(vec![EditCommand::InsertString("a b".to_owned())])
        );
        let enter = Event::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(parse(&mut mode, enter), ReedlineEvent::Enter);
        assert!(matches!(mode.edit_mode(), PromptEditMode::Emacs));
    }
}
