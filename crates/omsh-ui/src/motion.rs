//! The activity spinner and the rule for when a live line may be drawn again (blueprint §15.2).
//!
//! Nothing here touches the terminal. The caller asks "what glyph now?" and "may I redraw?" and
//! writes the answer itself, so the rules are plain functions with plain tests.

use std::time::Duration;

use omsh_config::Motion;

/// How long one frame of the spinner lasts. Within the 150 to 250 ms of §15.2 for one visible
/// change, because the spinner turns through several frames in that time and none of them is
/// a jump.
pub const FRAME: Duration = Duration::from_millis(100);

/// With reduced motion the line is drawn again once a second, with a glyph that does not turn.
pub const REDUCED_STEP: Duration = Duration::from_secs(1);

const UNICODE_FRAMES: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];
const ASCII_FRAMES: [&str; 4] = ["|", "/", "-", "\\"];

/// A spinner in one of two alphabets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Spinner {
    unicode: bool,
}

impl Spinner {
    /// A spinner of braille dots, or of `| / - \` when `unicode` is false.
    #[must_use]
    pub fn new(unicode: bool) -> Self {
        Self { unicode }
    }

    /// The frame that belongs to `elapsed`: it changes every [`FRAME`] and starts over.
    #[must_use]
    pub fn frame(&self, elapsed: Duration) -> &'static str {
        let step = usize::try_from(elapsed.as_millis() / FRAME.as_millis()).unwrap_or(0);
        if self.unicode {
            UNICODE_FRAMES[step % UNICODE_FRAMES.len()]
        } else {
            ASCII_FRAMES[step % ASCII_FRAMES.len()]
        }
    }

    /// The glyph that does not turn, for reduced motion.
    #[must_use]
    pub fn still(&self) -> &'static str {
        if self.unicode { "•" } else { "*" }
    }

    /// The glyph for a live line at `elapsed` under `motion`. `None` means the line carries no
    /// glyph at all (motion is off).
    #[must_use]
    pub fn glyph(&self, motion: Motion, elapsed: Duration) -> Option<&'static str> {
        match motion {
            Motion::Full => Some(self.frame(elapsed)),
            Motion::Reduced => Some(self.still()),
            Motion::Off => None,
        }
    }
}

/// How long to wait between two drawings of a live line. `None`: never draw it again (the line
/// is written once, as a plain line).
#[must_use]
pub fn redraw_interval(motion: Motion) -> Option<Duration> {
    match motion {
        Motion::Full => Some(FRAME),
        Motion::Reduced => Some(REDUCED_STEP),
        Motion::Off => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn the_frame_changes_every_hundred_milliseconds_and_wraps() {
        let spinner = Spinner::new(true);
        assert_eq!(spinner.frame(ms(0)), "⠋");
        assert_eq!(spinner.frame(ms(99)), "⠋");
        assert_eq!(spinner.frame(ms(100)), "⠙");
        assert_eq!(spinner.frame(ms(700)), "⠧");
        assert_eq!(spinner.frame(ms(800)), "⠋", "wraps after eight frames");
    }

    #[test]
    fn the_ascii_spinner_has_four_frames() {
        let spinner = Spinner::new(false);
        let frames: Vec<&str> = (0..5).map(|n| spinner.frame(ms(n * 100))).collect();
        assert_eq!(frames, ["|", "/", "-", "\\", "|"]);
    }

    #[test]
    fn every_frame_is_one_column_wide() {
        for frame in UNICODE_FRAMES.iter().chain(&ASCII_FRAMES) {
            assert_eq!(frame.chars().count(), 1, "{frame:?}");
        }
    }

    #[test]
    fn the_glyph_follows_the_motion_level() {
        let spinner = Spinner::new(true);
        assert_eq!(spinner.glyph(Motion::Full, ms(100)), Some("⠙"));
        assert_eq!(spinner.glyph(Motion::Reduced, ms(100)), Some("•"));
        assert_eq!(spinner.glyph(Motion::Reduced, ms(5000)), Some("•"));
        assert_eq!(spinner.glyph(Motion::Off, ms(100)), None);
        assert_eq!(Spinner::new(false).glyph(Motion::Reduced, ms(0)), Some("*"));
    }

    #[test]
    fn the_redraw_rule_for_each_level() {
        assert_eq!(redraw_interval(Motion::Full), Some(ms(100)));
        assert_eq!(redraw_interval(Motion::Reduced), Some(ms(1000)));
        assert_eq!(redraw_interval(Motion::Off), None);
    }
}
