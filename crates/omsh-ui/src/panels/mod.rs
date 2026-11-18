//! The panels of the safety questions (UI upgrade, phase 3): the "Run it?" question
//! ([`run_it`]) and the `undo` picker ([`undo_picker`]).
//!
//! Each panel is a [`crate::widgets::Widget`]: state, keys and `render(caps, width) -> Vec<Line>`.
//! None of them touches the terminal. [`menu::run_on_terminal`] is the one place that shows a
//! widget on the real terminal (standard error, raw mode, keys on a helper thread, waiting
//! keys thrown away before and after). The callers decide **whether** to use it: only a person at
//! a terminal gets a panel; everything else keeps the plain text questions.

use crate::caps::Caps;
use crate::theme::Theme;

pub mod choose;
pub mod details;
pub mod facts;
pub mod first_run;
pub mod info;
pub mod jobs_table;
pub mod menu;
pub mod meter;
pub mod plan;
pub mod plan_run;
pub mod run_it;
pub mod set_up;
pub mod status;
pub mod step;
pub mod undo_picker;

/// Helpers for the unit tests of the panels.
#[cfg(test)]
pub(crate) mod test_support {
    use omsh_config::{Icons, Motion};

    use crate::caps::{Caps, CapsFacts, Out};

    /// The caps of a color terminal, 100 columns wide, with the given motion and charset.
    pub fn caps(motion: Motion, unicode: bool) -> Caps {
        caps_with(motion, unicode, false)
    }

    pub fn caps_with(motion: Motion, unicode: bool, no_color: bool) -> Caps {
        let facts = CapsFacts {
            stdin_tty: true,
            stdout_tty: true,
            stderr_tty: true,
            utf8: unicode,
            no_color,
            size: Some((100, 30)),
            ..CapsFacts::default()
        };
        Caps::decide(facts, Out::Stderr, Icons::Auto, Some(motion))
    }
}

/// The widest a panel is drawn, in columns.
pub const MAX_WIDTH: u16 = 100;

/// What a panel needs to be shown on the terminal: what the terminal allows and the theme. Made
/// once at start, after the configuration is read; `None` where panels are not wanted.
#[derive(Clone, Debug)]
pub struct MenuUi {
    pub caps: Caps,
    pub theme: Theme,
}

/// How many columns the text of a row may take inside a panel on this terminal: the panel's
/// width less the rail. For text that is wrapped before it is made into a row (a reason, a
/// command), so that none of it is cut.
#[must_use]
pub fn inner_width(caps: &Caps) -> usize {
    usize::from(caps.panel_width(MAX_WIDTH))
        .saturating_sub(2)
        .max(8)
}
