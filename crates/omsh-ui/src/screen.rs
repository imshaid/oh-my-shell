//! The alternate screen: a full-screen view takes the terminal, and gives it back as it was.
//!
//! [`FullScreen::enter`] turns on raw mode (keys arrive one by one, `Ctrl+C` is a key and not a
//! signal), switches to the alternate screen (`ESC [ ? 1049 h`: the scrollback and the cursor
//! stay where they were) and asks for bracketed paste. Dropping it undoes all of that.
//!
//! A panic would leave the terminal in the alternate screen with raw mode on, and the panic
//! message would be printed there and lost. [`install_panic_restore`] puts a hook in front of
//! the existing one that leaves the alternate screen first, so the message is readable.
//! Everything is drawn on standard error, the stream the line editor draws on.

use std::io::{self, Write};
use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};

/// Switch to the alternate screen, and ask for bracketed paste.
const ENTER: &[u8] = b"\x1b[?1049h\x1b[?2004h";
/// Show the cursor, stop bracketed paste, and leave the alternate screen.
const LEAVE: &[u8] = b"\x1b[?25h\x1b[?2004l\x1b[?1049l";

/// True from [`FullScreen::enter`] to the moment the screen is given back.
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// The terminal is in the alternate screen with raw mode on, as long as this lives.
#[derive(Debug)]
pub struct FullScreen {
    _private: (),
}

impl FullScreen {
    /// Takes the terminal.
    ///
    /// # Errors
    ///
    /// The terminal could not be put in raw mode (it is not a terminal, or it has gone away) or
    /// could not be written to. Nothing is changed in that case.
    pub fn enter() -> io::Result<Self> {
        crossterm::terminal::enable_raw_mode()?;
        let mut err = io::stderr();
        if let Err(error) = err.write_all(ENTER).and_then(|()| err.flush()) {
            let _ = crossterm::terminal::disable_raw_mode();
            return Err(error);
        }
        ACTIVE.store(true, Ordering::SeqCst);
        Ok(Self { _private: () })
    }
}

impl Drop for FullScreen {
    fn drop(&mut self) {
        restore_if_active(&mut io::stderr());
    }
}

/// Gives the terminal back if a full-screen view has it: writes the sequence to `out` and turns
/// raw mode off. True if it did anything. Safe to call twice; only the first call acts, so the
/// panic hook and the destructor of an unwinding view do not both write.
pub fn restore_if_active(out: &mut impl Write) -> bool {
    if !ACTIVE.swap(false, Ordering::SeqCst) {
        return false;
    }
    let _ = out.write_all(LEAVE);
    let _ = out.flush();
    let _ = crossterm::terminal::disable_raw_mode();
    true
}

/// Puts a panic hook in front of the current one that gives the terminal back first. Call once,
/// at start; later calls do nothing. Costs nothing until a view opens.
pub fn install_panic_restore() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_if_active(&mut io::stderr());
            previous(info);
        }));
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// `ACTIVE` is one flag for the process, so these tests take turns.
    static TURN: Mutex<()> = Mutex::new(());

    #[test]
    fn the_terminal_is_given_back_once_and_only_while_a_view_has_it() {
        let _turn = TURN
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ACTIVE.store(true, Ordering::SeqCst);
        let mut first = Vec::new();
        assert!(restore_if_active(&mut first));
        assert_eq!(first, LEAVE);
        // The leave sequence ends the alternate screen and shows the cursor again.
        let text = String::from_utf8(first).unwrap();
        assert!(text.contains("\x1b[?1049l") && text.contains("\x1b[?25h"));
        // A second call (the destructor after the panic hook) writes nothing.
        let mut second = Vec::new();
        assert!(!restore_if_active(&mut second));
        assert!(second.is_empty());
    }

    #[test]
    fn nothing_is_written_when_no_view_is_open() {
        let _turn = TURN
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ACTIVE.store(false, Ordering::SeqCst);
        let mut out = Vec::new();
        assert!(!restore_if_active(&mut out));
        assert!(out.is_empty());
    }

    #[test]
    fn the_panic_hook_leaves_the_screen_before_the_message_is_printed() {
        let _turn = TURN
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // The hook calls `restore_if_active`; what it writes goes to standard error, which a
        // test cannot capture, so this checks the flag the hook acts on: a panic while a view
        // is open clears it (the terminal was given back), and the destructor then does nothing.
        install_panic_restore();
        ACTIVE.store(true, Ordering::SeqCst);
        let result = std::panic::catch_unwind(|| panic!("a view crashed"));
        assert!(result.is_err());
        assert!(!ACTIVE.load(Ordering::SeqCst), "the hook did not run");
    }
}
