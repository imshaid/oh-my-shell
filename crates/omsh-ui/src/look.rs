//! Which of color, Unicode glyphs and motion the terminal gets (blueprint §15.2, "degrade
//! automatically"). One pure function, [`Look::decide`], so every rule can be tested; the
//! environment is read in one place, [`Facts::from_environment`].

use std::env;

use omsh_config::{Icons, Motion};

/// What is known about the terminal and the environment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    /// The stream the panels go to (standard error) is a terminal.
    pub terminal: bool,
    /// `TERM` is `dumb`.
    pub dumb: bool,
    /// `NO_COLOR` is set to something that is not empty.
    pub no_color: bool,
    /// The locale (`LC_ALL`, then `LC_CTYPE`, then `LANG`) names UTF-8.
    pub utf8: bool,
    /// `SSH_CONNECTION` is set.
    pub ssh: bool,
}

impl Facts {
    /// Reads the process environment. `terminal` is whether standard error is a terminal.
    #[must_use]
    pub fn from_environment(terminal: bool) -> Self {
        Self {
            terminal,
            dumb: env::var("TERM").is_ok_and(|term| term == "dumb"),
            no_color: set_and_not_empty("NO_COLOR"),
            utf8: locale_is_utf8(),
            ssh: env::var_os("SSH_CONNECTION").is_some(),
        }
    }
}

/// What the interface may use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Look {
    /// Standard error is a terminal.
    pub terminal: bool,
    /// ANSI colors may be written.
    pub color: bool,
    /// Unicode glyphs and box-drawing characters may be written (else ASCII).
    pub unicode: bool,
    /// How much of the live line may move.
    pub motion: Motion,
}

impl Look {
    /// The rules:
    ///
    /// - **Color** is off when the output is not a terminal, `TERM=dumb`, or `NO_COLOR` is set
    ///   and not empty.
    /// - **Unicode**: off when the output is not a terminal. Otherwise `ui.icons` decides;
    ///   `auto` means "not dumb and a UTF-8 locale".
    /// - **Motion** is `Off` when the output is not a terminal, `TERM=dumb` or the locale is not
    ///   UTF-8. Otherwise `ui.motion` decides; when it is unset, `Reduced` over ssh and `Full`
    ///   anywhere else.
    #[must_use]
    pub fn decide(facts: Facts, icons: Icons, motion: Option<Motion>) -> Self {
        if !facts.terminal {
            return Self {
                terminal: false,
                color: false,
                unicode: false,
                motion: Motion::Off,
            };
        }
        let unicode = match icons {
            Icons::Unicode => true,
            Icons::Ascii => false,
            Icons::Auto => !facts.dumb && facts.utf8,
        };
        let motion = if facts.dumb || !facts.utf8 {
            Motion::Off
        } else {
            motion.unwrap_or(if facts.ssh {
                Motion::Reduced
            } else {
                Motion::Full
            })
        };
        Self {
            terminal: true,
            color: !facts.dumb && !facts.no_color,
            unicode,
            motion,
        }
    }

    /// The look for the environment as it is now.
    #[must_use]
    pub fn detect(terminal: bool, icons: Icons, motion: Option<Motion>) -> Self {
        Self::decide(Facts::from_environment(terminal), icons, motion)
    }

    /// Nothing special: no color, ASCII, no motion. What a pipe gets, and what tests use.
    #[must_use]
    pub const fn plain() -> Self {
        Self {
            terminal: false,
            color: false,
            unicode: false,
            motion: Motion::Off,
        }
    }
}

fn set_and_not_empty(name: &str) -> bool {
    env::var_os(name).is_some_and(|value| !value.is_empty())
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

    const GOOD: Facts = Facts {
        terminal: true,
        dumb: false,
        no_color: false,
        utf8: true,
        ssh: false,
    };

    fn look(facts: Facts) -> Look {
        Look::decide(facts, Icons::Auto, None)
    }

    #[test]
    fn a_good_terminal_gets_everything() {
        assert_eq!(
            look(GOOD),
            Look {
                terminal: true,
                color: true,
                unicode: true,
                motion: Motion::Full
            }
        );
    }

    #[test]
    fn a_pipe_gets_nothing_whatever_the_settings_say() {
        let facts = Facts {
            terminal: false,
            ..GOOD
        };
        assert_eq!(
            Look::decide(facts, Icons::Unicode, Some(Motion::Full)),
            Look::plain()
        );
    }

    #[test]
    fn no_color_removes_color_only() {
        let result = look(Facts {
            no_color: true,
            ..GOOD
        });
        assert!(!result.color);
        assert!(result.unicode);
        assert_eq!(result.motion, Motion::Full);
    }

    #[test]
    fn a_dumb_terminal_gets_ascii_no_color_no_motion() {
        let result = look(Facts { dumb: true, ..GOOD });
        assert!(!result.color && !result.unicode);
        assert_eq!(result.motion, Motion::Off);
    }

    #[test]
    fn a_dumb_terminal_can_still_be_given_unicode_by_the_setting() {
        let result = Look::decide(Facts { dumb: true, ..GOOD }, Icons::Unicode, None);
        assert!(result.unicode);
        assert!(!result.color);
        assert_eq!(
            result.motion,
            Motion::Off,
            "motion never comes back on a dumb terminal"
        );
    }

    #[test]
    fn a_locale_that_is_not_utf8_gives_ascii_and_no_motion() {
        let result = look(Facts {
            utf8: false,
            ..GOOD
        });
        assert!(result.color, "color does not depend on the locale");
        assert!(!result.unicode);
        assert_eq!(result.motion, Motion::Off);
    }

    #[test]
    fn the_icons_setting_beats_the_locale() {
        let c_locale = Facts {
            utf8: false,
            ..GOOD
        };
        assert!(Look::decide(c_locale, Icons::Unicode, None).unicode);
        assert!(!Look::decide(GOOD, Icons::Ascii, None).unicode);
    }

    #[test]
    fn ssh_defaults_to_reduced_motion() {
        let result = look(Facts { ssh: true, ..GOOD });
        assert_eq!(result.motion, Motion::Reduced);
    }

    #[test]
    fn the_motion_setting_beats_ssh() {
        let over_ssh = Facts { ssh: true, ..GOOD };
        assert_eq!(
            Look::decide(over_ssh, Icons::Auto, Some(Motion::Full)).motion,
            Motion::Full
        );
        assert_eq!(
            Look::decide(GOOD, Icons::Auto, Some(Motion::Off)).motion,
            Motion::Off
        );
        assert_eq!(
            Look::decide(GOOD, Icons::Auto, Some(Motion::Reduced)).motion,
            Motion::Reduced
        );
    }

    #[test]
    fn the_setting_cannot_force_motion_where_it_cannot_work() {
        let dumb = Facts { dumb: true, ..GOOD };
        let c_locale = Facts {
            utf8: false,
            ..GOOD
        };
        for facts in [dumb, c_locale] {
            assert_eq!(
                Look::decide(facts, Icons::Auto, Some(Motion::Full)).motion,
                Motion::Off
            );
        }
    }
}
