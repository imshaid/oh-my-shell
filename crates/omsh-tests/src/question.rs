//! The safety question in its two forms, for tests (UI upgrade, phase 3).
//!
//! A person at a real terminal gets the **panel** ("Run it?" with buttons: `t`, `y`, `n`, arrows
//! and Enter, which keeps the cursor hidden while it is open). `TERM=dumb`, the plain reader and
//! a pipe get the **text** question (`Run it? [y/N]`, answered by a typed line). Both must give
//! the same outcome, so a test of the outcome runs in both [`Mode`]s and only the keystrokes
//! differ:
//!
//! ```ignore
//! for mode in MODES {
//!     pty_test(BIN, |ctx| {
//!         let mut term = mode.launch(ctx.launch().cwd(ctx.home())).start()?;
//!         term.command("rm -rf proj/build")?;
//!         wait_question(&mut term, mode);
//!         answer(&mut term, mode, Answer::Cancel)?;
//!         term.expect_plain("cancelled; nothing ran", SHORT);
//!         // ... the same assertions about status and files, for both modes
//!         term.quit()
//!     });
//! }
//! ```

use std::io;

use crate::session::{Launch, SHORT, Session};

/// Which form of the question the shell shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// A terminal with the line editor: the panel with buttons.
    Panel,
    /// `TERM=dumb`: the text question.
    Dumb,
}

/// Both forms, panel first.
pub const MODES: [Mode; 2] = [Mode::Panel, Mode::Dumb];

impl Mode {
    /// `launch`, for this mode (`TERM=dumb` for [`Mode::Dumb`]).
    #[must_use]
    pub fn launch(self, launch: Launch) -> Launch {
        match self {
            Mode::Panel => launch,
            Mode::Dumb => launch.dumb(),
        }
    }
}

/// What the person answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    /// "Run anyway" (`y`).
    Run,
    /// "Move to trash" (`t`).
    Trash,
    /// "Cancel" (`n`).
    Cancel,
    /// Nothing but Enter: the default, which is to cancel.
    Enter,
    /// Ctrl+C.
    CtrlC,
}

/// Waits until the question can be answered. For the panel that is when its buttons are on the
/// screen: the frame is written in one piece, after the keys that were waiting were thrown
/// away, so a key sent after this is a real answer.
pub fn wait_question(term: &mut Session, mode: Mode) {
    match mode {
        Mode::Panel => term.expect_plain("Run anyway", SHORT),
        Mode::Dumb => term.expect_plain("Run it? [y/N]", SHORT),
    }
}

/// Answers the question the way `mode` takes answers.
///
/// # Errors
///
/// When the terminal cannot be written to.
pub fn answer(term: &mut Session, mode: Mode, answer: Answer) -> io::Result<()> {
    if answer == Answer::CtrlC {
        return match mode {
            Mode::Panel => term.press(&[3]),
            Mode::Dumb => term.key(3),
        };
    }
    let (key, text) = match answer {
        Answer::Run => (&b"y"[..], "y"),
        Answer::Trash => (&b"t"[..], "t"),
        Answer::Cancel => (&b"n"[..], "n"),
        Answer::Enter => (&b"\r"[..], ""),
        Answer::CtrlC => unreachable!("answered above"),
    };
    match mode {
        Mode::Panel => term.press(key),
        Mode::Dumb => term.line(text),
    }
}

/// Answers the question of `undo` (no number): in the panel the picker shows the list, `Enter`
/// opens the question about the action chosen (**Enter alone restores nothing**), and `y`
/// restores it or `n` leaves it; in the text form it is the line `Undo it? [y/N]`.
///
/// # Errors
///
/// When the terminal cannot be written to.
pub fn undo_question(term: &mut Session, mode: Mode, restore: bool) -> io::Result<()> {
    match mode {
        Mode::Panel => {
            term.expect_plain("Enter review", SHORT);
            term.press(b"\r")?;
            term.expect_plain("Undo this?", SHORT);
            term.press(if restore { b"y" } else { b"n" })
        }
        Mode::Dumb => {
            term.expect_plain("Undo it? [y/N]", SHORT);
            term.line(if restore { "y" } else { "n" })
        }
    }
}
