//! Shows a widget on the real terminal.

use std::io;

use crate::inline::{Ended, HelperKeys, run_menu};
use crate::panels::MenuUi;
use crate::sync;
use crate::widgets::Widget;

/// The caps of `ui` with the size of the window now (the ones kept from the start of the session
/// may be old).
#[must_use]
pub fn current_caps(ui: &MenuUi) -> crate::caps::Caps {
    let mut caps = ui.caps;
    if let Ok((columns, rows)) = crossterm::terminal::size() {
        caps.width = columns.max(1);
        caps.height = rows.max(1);
    }
    caps
}

/// Shows `widget` below the cursor, on standard error, and returns how it ended. The cursor must
/// be at the start of an empty line. `should_stop` is asked every tenth of a second while no key
/// comes: true ends the menu as [`Ended::Closed`] (the shell was hung up).
///
/// The window size is read again here (the one in `ui.caps` is from the start of the session),
/// and synchronized output is asked about now, lazily, and the answer is kept for the process.
///
/// # Errors
///
/// When the terminal is not interactive, or cannot be taken, written or read. The caller then
/// uses the plain question.
pub fn run_on_terminal<W: Widget>(
    widget: &mut W,
    ui: &MenuUi,
    should_stop: Box<dyn FnMut() -> bool>,
) -> io::Result<Ended<W::Output>> {
    let mut caps = current_caps(ui);
    // `TERM=dumb` cannot move the cursor: it gets the plain question, whatever else is true.
    if !caps.interactive() || caps.dumb {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "a panel needs a terminal for input and output",
        ));
    }
    caps = caps.with_sync_output(sync::supported());
    let mut keys = HelperKeys::open(should_stop)?;
    let mut out = io::stderr().lock();
    run_menu(widget, &mut keys, &mut out, &ui.theme, &caps)
}
