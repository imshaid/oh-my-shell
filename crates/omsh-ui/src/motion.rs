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

/// How long a number takes to ease to a new value (the context meter, a count-up).
pub const TWEEN: Duration = Duration::from_millis(600);

/// How long a status chip stays reversed after a job or a step ends.
pub const FLASH: Duration = Duration::from_millis(150);

/// Fast at the start, slow at the end (cubic ease-out). `t` is clamped to 0 to 1.
#[must_use]
pub fn ease_out(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// A value that moves from one number to another over a time. Only with full motion does it
/// move: with reduced motion or none it is at the end value from the first moment (motion never
/// carries meaning alone, so the end value is always what is shown).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tween {
    from: f64,
    to: f64,
    duration: Duration,
}

impl Tween {
    /// A move from `from` to `to` taking `duration`.
    #[must_use]
    pub fn new(from: f64, to: f64, duration: Duration) -> Self {
        Self { from, to, duration }
    }

    /// The value `elapsed` after the start.
    #[must_use]
    pub fn value(&self, elapsed: Duration, motion: Motion) -> f64 {
        if self.done(elapsed, motion) {
            return self.to;
        }
        let t = elapsed.as_secs_f64() / self.duration.as_secs_f64();
        self.from + (self.to - self.from) * ease_out(t)
    }

    /// True once the value is at its end, so no more frames are needed.
    #[must_use]
    pub fn done(&self, elapsed: Duration, motion: Motion) -> bool {
        motion != Motion::Full || self.duration.is_zero() || elapsed >= self.duration
    }
}

/// A short highlight: on for `duration` after it starts, and only with full motion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Flash {
    duration: Duration,
}

impl Flash {
    /// A flash that lasts `duration`.
    #[must_use]
    pub fn new(duration: Duration) -> Self {
        Self { duration }
    }

    /// The usual flash ([`FLASH`]).
    #[must_use]
    pub fn short() -> Self {
        Self::new(FLASH)
    }

    /// True while the highlight is on, `elapsed` after it started.
    #[must_use]
    pub fn active(&self, elapsed: Duration, motion: Motion) -> bool {
        motion == Motion::Full && elapsed < self.duration
    }
}

/// When to draw the next frame of something that moves. When nothing moves there is no next
/// frame and no timer: an idle menu waits for a key and uses no CPU.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameClock {
    interval: Option<Duration>,
}

impl FrameClock {
    /// The clock for a motion level. Full motion is [`FRAME`] (10 frames a second, under the
    /// 12 to 15 of the spec); inside tmux it is half of that, because tmux redraws what it is
    /// given and a slow link feels every frame. Reduced is one frame a second; off has none.
    #[must_use]
    pub fn new(motion: Motion, tmux: bool) -> Self {
        let interval = redraw_interval(motion).map(|interval| {
            if tmux && motion == Motion::Full {
                interval * 2
            } else {
                interval
            }
        });
        Self { interval }
    }

    /// The time between two frames, if there are any.
    #[must_use]
    pub fn interval(&self) -> Option<Duration> {
        self.interval
    }

    /// How long to wait for a key before drawing again. `None`: wait for ever (nothing moves, or
    /// motion is off). `since_last_frame` is how long ago the last frame was drawn.
    #[must_use]
    pub fn next_wake(&self, animating: bool, since_last_frame: Duration) -> Option<Duration> {
        if !animating {
            return None;
        }
        self.interval
            .map(|interval| interval.saturating_sub(since_last_frame))
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
    fn easing_runs_from_zero_to_one_slowing_down_and_is_clamped() {
        assert!((ease_out(0.0)).abs() < 1e-12);
        assert!((ease_out(1.0) - 1.0).abs() < 1e-12);
        assert!((ease_out(-3.0)).abs() < 1e-12);
        assert!((ease_out(7.0) - 1.0).abs() < 1e-12);
        let mut last = -1.0;
        for step in 0..=20 {
            let value = ease_out(f64::from(step) / 20.0);
            assert!(value > last, "rises at every step");
            last = value;
        }
        assert!(ease_out(0.5) > 0.5, "the first half covers more than half");
    }

    #[test]
    fn a_tween_eases_with_full_motion_and_is_at_the_end_at_once_otherwise() {
        let tween = Tween::new(10.0, 90.0, TWEEN);
        assert!((tween.value(ms(0), Motion::Full) - 10.0).abs() < 1e-9);
        let middle = tween.value(ms(300), Motion::Full);
        assert!(middle > 50.0 && middle < 90.0, "{middle}");
        assert!((tween.value(ms(600), Motion::Full) - 90.0).abs() < 1e-9);
        assert!((tween.value(ms(9000), Motion::Full) - 90.0).abs() < 1e-9);
        for motion in [Motion::Reduced, Motion::Off] {
            assert!((tween.value(ms(0), motion) - 90.0).abs() < 1e-9);
            assert!(tween.done(ms(0), motion));
        }
        assert!(!tween.done(ms(599), Motion::Full));
        assert!(tween.done(ms(600), Motion::Full));
        assert!(Tween::new(0.0, 1.0, Duration::ZERO).done(ms(0), Motion::Full));
    }

    #[test]
    fn a_tween_can_go_down() {
        let tween = Tween::new(80.0, 20.0, TWEEN);
        let middle = tween.value(ms(300), Motion::Full);
        assert!(middle < 50.0 && middle > 20.0, "{middle}");
    }

    #[test]
    fn a_flash_is_short_and_only_with_full_motion() {
        let flash = Flash::short();
        assert!(flash.active(ms(0), Motion::Full));
        assert!(flash.active(ms(149), Motion::Full));
        assert!(!flash.active(ms(150), Motion::Full));
        assert!(!flash.active(ms(0), Motion::Reduced));
        assert!(!flash.active(ms(0), Motion::Off));
    }

    #[test]
    fn the_clock_has_no_timer_when_nothing_moves() {
        for motion in [Motion::Full, Motion::Reduced, Motion::Off] {
            let clock = FrameClock::new(motion, false);
            assert_eq!(clock.next_wake(false, ms(0)), None, "{motion:?} idle");
        }
        assert_eq!(
            FrameClock::new(Motion::Off, false).next_wake(true, ms(0)),
            None
        );
    }

    #[test]
    fn the_clock_waits_what_is_left_of_the_frame_and_stays_under_15_fps() {
        let clock = FrameClock::new(Motion::Full, false);
        assert_eq!(clock.interval(), Some(ms(100)));
        assert_eq!(clock.next_wake(true, ms(0)), Some(ms(100)));
        assert_eq!(clock.next_wake(true, ms(30)), Some(ms(70)));
        assert_eq!(
            clock.next_wake(true, ms(500)),
            Some(ms(0)),
            "late: draw now"
        );
        let frames_per_second = 1000.0 / clock.interval().map_or(1.0, |i| i.as_secs_f64() * 1000.0);
        assert!(frames_per_second <= 15.0);
    }

    #[test]
    fn tmux_halves_full_motion_and_does_not_touch_the_other_levels() {
        assert_eq!(
            FrameClock::new(Motion::Full, true).interval(),
            Some(ms(200))
        );
        assert_eq!(
            FrameClock::new(Motion::Reduced, true).interval(),
            Some(ms(1000))
        );
        assert_eq!(FrameClock::new(Motion::Off, true).interval(), None);
    }

    #[test]
    fn the_redraw_rule_for_each_level() {
        assert_eq!(redraw_interval(Motion::Full), Some(ms(100)));
        assert_eq!(redraw_interval(Motion::Reduced), Some(ms(1000)));
        assert_eq!(redraw_interval(Motion::Off), None);
    }
}
