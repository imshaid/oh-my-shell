//! The line shown while the model thinks (§15.2: only real numbers).
//!
//! What it looks like depends on the look of the terminal (`ui.motion`, S26):
//!
//! - **full**: one line, `⠙ Asking <provider> (<model>)... 2.4 s`, redrawn in place every 100 ms.
//!   The glyph turns: it is the activity indicator, the one thing that moves while the model
//!   thinks. The time is the real elapsed time, and whatever other real number the caller has
//!   (such as the characters that have arrived) follows it.
//! - **reduced**: the same line with a glyph that does not turn, redrawn once a second.
//! - **off**, and anywhere that is not a terminal: one plain line, `Asking ...`, printed once.

use std::cell::Cell;
use std::time::{Duration, Instant};

use omsh_exec::Terminal;
use omsh_ui::{Paint, Spinner, Token, redraw_interval};

pub struct WaitLine {
    label: String,
    paint: Paint,
    spinner: Spinner,
    /// Whether the line is redrawn at all (a terminal with motion on).
    live: bool,
    started: Instant,
    /// The elapsed time of the last drawing.
    last_draw: Cell<Option<Duration>>,
}

impl WaitLine {
    /// Starts waiting. Without a live line this prints the plain line now.
    pub fn start(label: &str, paint: &Paint, terminal: &mut dyn Terminal) -> Self {
        let look = paint.look();
        let live = look.terminal && redraw_interval(look.motion).is_some();
        if !live {
            terminal.err(&format!("{label}...\n"));
        }
        Self {
            label: label.to_owned(),
            paint: paint.clone(),
            spinner: Spinner::new(look.unicode),
            live,
            started: Instant::now(),
            last_draw: Cell::new(None),
        }
    }

    /// Redraws the line if it is due. `extra` is appended as it is (for example `, 310
    /// characters`).
    pub fn tick(&self, terminal: &mut dyn Terminal, extra: &str) {
        self.tick_at(self.started.elapsed(), terminal, extra);
    }

    /// [`tick`](Self::tick) for a given elapsed time, so that the timing can be tested.
    pub fn tick_at(&self, elapsed: Duration, terminal: &mut dyn Terminal, extra: &str) {
        if !self.live {
            return;
        }
        let Some(every) = redraw_interval(self.paint.look().motion) else {
            return;
        };
        let due = self
            .last_draw
            .get()
            .is_none_or(|at| elapsed.saturating_sub(at) >= every);
        if !due {
            return;
        }
        let glyph = self
            .spinner
            .glyph(self.paint.look().motion, elapsed)
            .map(|glyph| format!("{} ", self.paint.token(Token::Ai, glyph)))
            .unwrap_or_default();
        terminal.err(&format!(
            "\r\x1b[K{glyph}{}... {}{extra}",
            self.paint.token(Token::Ai, &self.label),
            self.paint
                .token(Token::Dim, &format!("{:.1} s", elapsed.as_secs_f64())),
        ));
        self.last_draw.set(Some(elapsed));
    }

    /// Erases the line, if one was drawn.
    pub fn clear(&self, terminal: &mut dyn Terminal) {
        if self.last_draw.take().is_some() {
            terminal.err("\r\x1b[K");
        }
    }
}

#[cfg(test)]
mod tests {
    use omsh_config::Motion;
    use omsh_exec::{ReadError, Terminal};
    use omsh_ui::{Look, Theme};

    use super::*;

    /// Keeps what was written to standard error.
    #[derive(Default)]
    struct Recorder {
        err: String,
    }

    impl Terminal for Recorder {
        fn input_is_terminal(&self) -> bool {
            false
        }
        fn out(&mut self, _text: &str) {}
        fn err(&mut self, text: &str) {
            self.err.push_str(text);
        }
        fn read_line(
            &mut self,
            _prompt: &str,
            _hidden: bool,
            _line: &mut Vec<u8>,
        ) -> Result<(), ReadError> {
            Err(ReadError::EndOfInput)
        }
        fn interrupted(&self) -> bool {
            false
        }
    }

    fn paint(motion: Motion, color: bool, unicode: bool) -> Paint {
        Paint::new(
            &Theme::default(),
            Look {
                terminal: true,
                color,
                unicode,
                motion,
            },
        )
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn full_motion_turns_the_glyph_every_hundred_milliseconds() {
        let mut tty = Recorder::default();
        let wait = WaitLine::start("Asking X (m)", &paint(Motion::Full, false, true), &mut tty);
        assert_eq!(tty.err, "", "a live line prints nothing at the start");
        wait.tick_at(ms(0), &mut tty, "");
        wait.tick_at(ms(50), &mut tty, "");
        wait.tick_at(ms(100), &mut tty, "");
        wait.tick_at(ms(250), &mut tty, ", 12 characters");
        assert_eq!(
            tty.err,
            "\r\x1b[K⠋ Asking X (m)... \x1b[2m0.0 s\x1b[0m\
             \r\x1b[K⠙ Asking X (m)... \x1b[2m0.1 s\x1b[0m\
             \r\x1b[K⠹ Asking X (m)... \x1b[2m0.2 s\x1b[0m, 12 characters"
        );
    }

    #[test]
    fn reduced_motion_redraws_once_a_second_with_a_still_glyph() {
        let mut tty = Recorder::default();
        let wait = WaitLine::start(
            "Asking X (m)",
            &paint(Motion::Reduced, false, true),
            &mut tty,
        );
        for at in [0, 100, 500, 999, 1000, 1500, 2000] {
            wait.tick_at(ms(at), &mut tty, "");
        }
        assert_eq!(
            tty.err,
            "\r\x1b[K• Asking X (m)... \x1b[2m0.0 s\x1b[0m\
             \r\x1b[K• Asking X (m)... \x1b[2m1.0 s\x1b[0m\
             \r\x1b[K• Asking X (m)... \x1b[2m2.0 s\x1b[0m"
        );
    }

    #[test]
    fn motion_off_prints_one_plain_line_and_never_redraws() {
        let mut tty = Recorder::default();
        let wait = WaitLine::start("Asking X (m)", &paint(Motion::Off, true, true), &mut tty);
        assert_eq!(tty.err, "Asking X (m)...\n");
        for at in [0, 1000, 5000] {
            wait.tick_at(ms(at), &mut tty, "");
        }
        wait.clear(&mut tty);
        assert_eq!(
            tty.err, "Asking X (m)...\n",
            "nothing more, not even a clear"
        );
    }

    #[test]
    fn a_pipe_gets_the_same_one_plain_line() {
        let mut tty = Recorder::default();
        let wait = WaitLine::start("Asking X (m)", &Paint::plain(), &mut tty);
        wait.tick_at(ms(5000), &mut tty, "");
        assert_eq!(tty.err, "Asking X (m)...\n");
    }

    #[test]
    fn ascii_uses_the_four_frame_spinner() {
        let mut tty = Recorder::default();
        let wait = WaitLine::start("A", &paint(Motion::Full, false, false), &mut tty);
        wait.tick_at(ms(0), &mut tty, "");
        wait.tick_at(ms(100), &mut tty, "");
        assert_eq!(
            tty.err,
            "\r\x1b[K| A... \x1b[2m0.0 s\x1b[0m\r\x1b[K/ A... \x1b[2m0.1 s\x1b[0m"
        );
    }

    #[test]
    fn color_paints_the_glyph_and_label_in_the_ai_color_and_the_time_dim() {
        let mut tty = Recorder::default();
        let wait = WaitLine::start("A", &paint(Motion::Full, true, true), &mut tty);
        wait.tick_at(ms(0), &mut tty, "");
        assert_eq!(
            tty.err,
            "\r\x1b[K\x1b[35m⠋\x1b[0m \x1b[35mA\x1b[0m... \x1b[2;90m0.0 s\x1b[0m"
        );
    }

    #[test]
    fn clear_erases_a_drawn_line_once() {
        let mut tty = Recorder::default();
        let wait = WaitLine::start("A", &paint(Motion::Full, false, true), &mut tty);
        wait.clear(&mut tty);
        assert_eq!(tty.err, "", "nothing was drawn, so nothing is erased");
        wait.tick_at(ms(0), &mut tty, "");
        wait.clear(&mut tty);
        wait.clear(&mut tty);
        assert!(tty.err.ends_with("0.0 s\x1b[0m\r\x1b[K"), "{:?}", tty.err);
        assert_eq!(tty.err.matches("\r\x1b[K").count(), 2);
    }
}
