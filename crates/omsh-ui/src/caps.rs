//! What the terminal and the environment allow, decided in one pure function (UI upgrade, U1).
//!
//! [`Caps::decide`] turns plain facts ([`CapsFacts`]) and the two settings (`ui.icons`,
//! `ui.motion`) into the answers every panel and widget asks: may I write escapes, may I draw
//! Unicode, may I move, how wide is the window. [`Caps::detect`] reads the facts from the
//! process. It is cheap (a few environment reads, `isatty` and one `ioctl`), so it is meant to be
//! called once, after the configuration is loaded, and the value kept. It never writes to the
//! terminal and never asks it anything: the one question that needs an answer from the terminal
//! (synchronized output) is [`crate::sync`], asked lazily by whoever opens a menu.
//!
//! Where it differs from [`crate::Look`], which still drives today's panels: a locale that is
//! not UTF-8 gives ASCII glyphs but does not stop motion (the spinner turns through `| / - \`),
//! and `CI` stops motion.

use std::env;
use std::io::IsTerminal;

use omsh_config::{Icons, Motion};

use crate::motion::Spinner;

/// The stream the panels are written to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Out {
    /// Standard output.
    Stdout,
    /// Standard error: where the shell's own panels go today.
    Stderr,
}

/// What is known about the process, with nothing decided yet.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CapsFacts {
    /// Standard input is a terminal.
    pub stdin_tty: bool,
    /// Standard output is a terminal.
    pub stdout_tty: bool,
    /// Standard error is a terminal.
    pub stderr_tty: bool,
    /// `TERM` is `dumb`.
    pub dumb: bool,
    /// `NO_COLOR` is set and not empty.
    pub no_color: bool,
    /// The locale (`LC_ALL`, then `LC_CTYPE`, then `LANG`) names UTF-8.
    pub utf8: bool,
    /// `CI` is set to something other than empty, `0` or `false`.
    pub ci: bool,
    /// `SSH_CONNECTION` is set.
    pub ssh: bool,
    /// `TMUX` is set.
    pub tmux: bool,
    /// `OMSH_MOTION` holds a word we know (`on`, `full`, `reduced`, `off`).
    pub env_motion: Option<Motion>,
    /// Columns and rows of the window, when known.
    pub size: Option<(u16, u16)>,
}

/// What the interface may do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caps {
    /// Standard input is a terminal.
    pub stdin_tty: bool,
    /// Standard output is a terminal.
    pub stdout_tty: bool,
    /// Standard error is a terminal.
    pub stderr_tty: bool,
    /// The stream the panels go to is a terminal. Without it nothing but plain text is written.
    pub out_tty: bool,
    /// The locale names UTF-8.
    pub utf8: bool,
    /// Box-drawing characters and symbols may be written (else ASCII).
    pub unicode: bool,
    /// `NO_COLOR` is set. Only colors go; bold, dim, reverse and underline stay.
    pub no_color: bool,
    /// ANSI colors may be written.
    pub color: bool,
    /// `TERM=dumb`.
    pub dumb: bool,
    /// Running in continuous integration.
    pub ci: bool,
    /// Running over ssh.
    pub ssh: bool,
    /// Running inside tmux.
    pub tmux: bool,
    /// How much may move.
    pub motion: Motion,
    /// The terminal understands synchronized output (DEC mode 2026). Always false here: it is
    /// asked for by [`crate::sync`] and set with [`Caps::with_sync_output`].
    pub sync_output: bool,
    /// Columns of the window (80 when it is not known).
    pub width: u16,
    /// Rows of the window (24 when it is not known).
    pub height: u16,
}

/// The width and height used when the window size cannot be read.
pub const DEFAULT_SIZE: (u16, u16) = (80, 24);

impl Caps {
    /// The rules:
    ///
    /// - **Escapes** (anything but text) only when the stream the panels go to is a terminal.
    /// - **Color** is off for a non-terminal, `TERM=dumb` and `NO_COLOR`. `NO_COLOR` takes
    ///   nothing else away.
    /// - **Unicode**: off for a non-terminal. Otherwise `ui.icons` decides; `auto` means "a
    ///   UTF-8 locale and not dumb".
    /// - **Motion** is `Off` for a non-terminal, `TERM=dumb` and CI. Otherwise `OMSH_MOTION`,
    ///   then `ui.motion`, then `Reduced` over ssh and `Full` anywhere else. Motion does not
    ///   depend on the locale and `NO_COLOR` does not touch it.
    #[must_use]
    pub fn decide(facts: CapsFacts, out: Out, icons: Icons, motion: Option<Motion>) -> Self {
        let out_tty = match out {
            Out::Stdout => facts.stdout_tty,
            Out::Stderr => facts.stderr_tty,
        };
        let unicode = out_tty
            && match icons {
                Icons::Unicode => true,
                Icons::Ascii => false,
                Icons::Auto => facts.utf8 && !facts.dumb,
            };
        let motion = if !out_tty || facts.dumb || facts.ci {
            Motion::Off
        } else {
            facts.env_motion.or(motion).unwrap_or(if facts.ssh {
                Motion::Reduced
            } else {
                Motion::Full
            })
        };
        let (width, height) = facts
            .size
            .filter(|(columns, rows)| *columns > 0 && *rows > 0)
            .unwrap_or(DEFAULT_SIZE);
        Self {
            stdin_tty: facts.stdin_tty,
            stdout_tty: facts.stdout_tty,
            stderr_tty: facts.stderr_tty,
            out_tty,
            utf8: facts.utf8,
            unicode,
            no_color: facts.no_color,
            color: out_tty && !facts.dumb && !facts.no_color,
            dumb: facts.dumb,
            ci: facts.ci,
            ssh: facts.ssh,
            tmux: facts.tmux,
            motion,
            sync_output: false,
            width,
            height,
        }
    }

    /// The facts of this process, decided for `out`.
    #[must_use]
    pub fn detect(out: Out, icons: Icons, motion: Option<Motion>) -> Self {
        Self::decide(CapsFacts::from_environment(), out, icons, motion)
    }

    /// Nothing but text: what a pipe gets, and what tests that do not care use.
    #[must_use]
    pub fn plain() -> Self {
        Self::decide(CapsFacts::default(), Out::Stderr, Icons::Auto, None)
    }

    /// This value with synchronized output set to what [`crate::sync`] found.
    #[must_use]
    pub fn with_sync_output(mut self, supported: bool) -> Self {
        self.sync_output = supported && self.out_tty;
        self
    }

    /// A person can answer a menu: standard input and the output stream are terminals. When it
    /// is false the numbered fallback is used.
    #[must_use]
    pub fn interactive(&self) -> bool {
        self.stdin_tty && self.out_tty
    }

    /// The width of a panel: the window less one column, but at most `max` and at least one
    /// column. The last column stays free: a line that fills it leaves the cursor in the
    /// "about to wrap" state, and the cursor arithmetic of an inline redraw would be wrong.
    #[must_use]
    pub fn panel_width(&self, max: u16) -> u16 {
        self.width.saturating_sub(1).min(max).max(1)
    }

    /// The spinner for this terminal's alphabet.
    #[must_use]
    pub fn spinner(&self) -> Spinner {
        Spinner::new(self.unicode)
    }
}

impl CapsFacts {
    /// Reads the process: `isatty` of the three streams, the environment, and the window size.
    #[must_use]
    pub fn from_environment() -> Self {
        let stdout_tty = std::io::stdout().is_terminal();
        let stderr_tty = std::io::stderr().is_terminal();
        let size = if stdout_tty || stderr_tty {
            crossterm::terminal::size().ok()
        } else {
            None
        };
        Self {
            stdin_tty: std::io::stdin().is_terminal(),
            stdout_tty,
            stderr_tty,
            dumb: env::var("TERM").is_ok_and(|term| term == "dumb"),
            no_color: set_and_not_empty("NO_COLOR"),
            utf8: locale_is_utf8(),
            ci: env::var("CI").is_ok_and(|value| is_yes(&value)),
            ssh: env::var_os("SSH_CONNECTION").is_some(),
            tmux: env::var_os("TMUX").is_some_and(|value| !value.is_empty()),
            env_motion: env::var("OMSH_MOTION")
                .ok()
                .and_then(|word| parse_motion(&word)),
            size,
        }
    }
}

/// `on` (or `full`), `reduced` and `off`, in any case. The spec's word for the full level is
/// `on`; the configuration file says `full`.
#[must_use]
pub fn parse_motion(word: &str) -> Option<Motion> {
    match word.trim().to_ascii_lowercase().as_str() {
        "on" | "full" => Some(Motion::Full),
        "reduced" => Some(Motion::Reduced),
        "off" => Some(Motion::Off),
        _ => None,
    }
}

fn set_and_not_empty(name: &str) -> bool {
    env::var_os(name).is_some_and(|value| !value.is_empty())
}

/// `CI=true`, `CI=1` and the like. Empty, `0` and `false` are "no".
fn is_yes(value: &str) -> bool {
    let value = value.trim().to_ascii_lowercase();
    !(value.is_empty() || value == "0" || value == "false")
}

/// The first of `LC_ALL`, `LC_CTYPE` and `LANG` that is set and not empty decides.
fn locale_is_utf8() -> bool {
    let locale = ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .find_map(|name| env::var(name).ok().filter(|value| !value.is_empty()));
    locale.is_some_and(|value| {
        let value = value.to_ascii_lowercase();
        value.contains("utf-8") || value.contains("utf8")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: CapsFacts = CapsFacts {
        stdin_tty: true,
        stdout_tty: true,
        stderr_tty: true,
        dumb: false,
        no_color: false,
        utf8: true,
        ci: false,
        ssh: false,
        tmux: false,
        env_motion: None,
        size: Some((100, 30)),
    };

    fn caps(facts: CapsFacts) -> Caps {
        Caps::decide(facts, Out::Stderr, Icons::Auto, None)
    }

    #[test]
    fn a_good_terminal_gets_everything() {
        let result = caps(GOOD);
        assert!(result.out_tty && result.interactive());
        assert!(result.color && result.unicode);
        assert_eq!(result.motion, Motion::Full);
        assert_eq!((result.width, result.height), (100, 30));
        assert!(!result.sync_output, "asked for later, never guessed");
    }

    #[test]
    fn the_three_streams_are_kept_apart_and_out_picks_one() {
        let facts = CapsFacts {
            stdout_tty: false,
            ..GOOD
        };
        let on_stderr = Caps::decide(facts, Out::Stderr, Icons::Auto, None);
        assert!(on_stderr.out_tty && !on_stderr.stdout_tty && on_stderr.stderr_tty);
        let on_stdout = Caps::decide(facts, Out::Stdout, Icons::Auto, None);
        assert!(!on_stdout.out_tty);
        assert!(!on_stdout.color && !on_stdout.unicode);
        assert!(!on_stdout.interactive());
    }

    #[test]
    fn a_pipe_gets_text_only_whatever_the_settings_say() {
        let facts = CapsFacts {
            stdout_tty: false,
            stderr_tty: false,
            env_motion: Some(Motion::Full),
            ..GOOD
        };
        let result = Caps::decide(facts, Out::Stderr, Icons::Unicode, Some(Motion::Full));
        assert!(!result.out_tty && !result.color && !result.unicode);
        assert_eq!(result.motion, Motion::Off);
        assert!(!result.interactive());
    }

    #[test]
    fn typing_into_a_pipe_is_not_interactive() {
        let facts = CapsFacts {
            stdin_tty: false,
            ..GOOD
        };
        let result = caps(facts);
        assert!(result.out_tty && !result.interactive());
    }

    #[test]
    fn no_color_removes_color_and_nothing_else() {
        let result = caps(CapsFacts {
            no_color: true,
            ..GOOD
        });
        assert!(!result.color);
        assert!(result.no_color);
        assert!(result.unicode);
        assert_eq!(result.motion, Motion::Full, "NO_COLOR does not stop motion");
    }

    #[test]
    fn a_dumb_terminal_gets_ascii_no_color_no_motion() {
        let result = caps(CapsFacts { dumb: true, ..GOOD });
        assert!(!result.color && !result.unicode);
        assert_eq!(result.motion, Motion::Off);
        assert_eq!(
            Caps::decide(
                CapsFacts { dumb: true, ..GOOD },
                Out::Stderr,
                Icons::Auto,
                Some(Motion::Full)
            )
            .motion,
            Motion::Off,
            "the setting cannot bring motion back"
        );
    }

    #[test]
    fn a_locale_that_is_not_utf8_gives_ascii_but_keeps_motion() {
        let result = caps(CapsFacts {
            utf8: false,
            ..GOOD
        });
        assert!(!result.unicode);
        assert!(result.color, "color does not depend on the locale");
        assert_eq!(result.motion, Motion::Full);
        assert_eq!(
            result.spinner().frame(std::time::Duration::ZERO),
            "|",
            "the spinner turns through | / - \\"
        );
    }

    #[test]
    fn the_icons_setting_beats_the_locale() {
        let c_locale = CapsFacts {
            utf8: false,
            ..GOOD
        };
        assert!(Caps::decide(c_locale, Out::Stderr, Icons::Unicode, None).unicode);
        assert!(!Caps::decide(GOOD, Out::Stderr, Icons::Ascii, None).unicode);
    }

    #[test]
    fn ci_stops_motion_only() {
        let result = caps(CapsFacts { ci: true, ..GOOD });
        assert_eq!(result.motion, Motion::Off);
        assert!(result.color && result.unicode);
        assert_eq!(
            Caps::decide(
                CapsFacts { ci: true, ..GOOD },
                Out::Stderr,
                Icons::Auto,
                Some(Motion::Full)
            )
            .motion,
            Motion::Off
        );
    }

    #[test]
    fn motion_is_chosen_by_the_environment_then_the_setting_then_ssh() {
        let over_ssh = CapsFacts { ssh: true, ..GOOD };
        assert_eq!(caps(over_ssh).motion, Motion::Reduced, "ssh default");
        assert_eq!(
            Caps::decide(over_ssh, Out::Stderr, Icons::Auto, Some(Motion::Full)).motion,
            Motion::Full,
            "the setting beats ssh"
        );
        let with_env = CapsFacts {
            ssh: true,
            env_motion: Some(Motion::Off),
            ..GOOD
        };
        assert_eq!(
            Caps::decide(with_env, Out::Stderr, Icons::Auto, Some(Motion::Full)).motion,
            Motion::Off,
            "OMSH_MOTION beats the setting"
        );
        let wants_motion = CapsFacts {
            env_motion: Some(Motion::Reduced),
            ..GOOD
        };
        assert_eq!(
            Caps::decide(wants_motion, Out::Stderr, Icons::Auto, Some(Motion::Off)).motion,
            Motion::Reduced
        );
    }

    #[test]
    fn the_size_defaults_to_80_by_24_when_unknown_or_zero() {
        for size in [None, Some((0, 0)), Some((80, 0)), Some((0, 24))] {
            let result = caps(CapsFacts { size, ..GOOD });
            assert_eq!((result.width, result.height), DEFAULT_SIZE, "{size:?}");
        }
    }

    #[test]
    fn a_panel_is_as_wide_as_the_window_up_to_a_limit() {
        let result = caps(GOOD);
        assert_eq!(result.panel_width(72), 72);
        assert_eq!(
            result.panel_width(500),
            99,
            "one column less than the window"
        );
        assert_eq!(result.panel_width(0), 1);
    }

    #[test]
    fn synchronized_output_is_only_ever_set_for_a_terminal() {
        assert!(caps(GOOD).with_sync_output(true).sync_output);
        assert!(!caps(GOOD).with_sync_output(false).sync_output);
        let pipe = caps(CapsFacts {
            stderr_tty: false,
            ..GOOD
        });
        assert!(!pipe.with_sync_output(true).sync_output);
    }

    #[test]
    fn plain_is_a_pipe() {
        let plain = Caps::plain();
        assert!(!plain.out_tty && !plain.color && !plain.unicode && !plain.interactive());
        assert_eq!(plain.motion, Motion::Off);
    }

    #[test]
    fn the_motion_words() {
        assert_eq!(parse_motion("on"), Some(Motion::Full));
        assert_eq!(parse_motion(" FULL "), Some(Motion::Full));
        assert_eq!(parse_motion("Reduced"), Some(Motion::Reduced));
        assert_eq!(parse_motion("off"), Some(Motion::Off));
        assert_eq!(parse_motion(""), None);
        assert_eq!(parse_motion("fast"), None);
    }

    #[test]
    fn the_ci_words() {
        for yes in ["true", "1", "yes", "TRUE", "github"] {
            assert!(is_yes(yes), "{yes}");
        }
        for no in ["", "0", "false", "False", "  "] {
            assert!(!is_yes(no), "{no:?}");
        }
    }
}
