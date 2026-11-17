//! The pieces of the shell's look (UI upgrade, phase 2): the rule between commands, the chip that
//! says how the last command ended, the `ask AI` tag, the hint bar, the OSC 133 marks, and the
//! rule for when the hint bar is shown.
//!
//! Everything here is plain data or text. A [`Line`] says what a piece means (`Ok`, `Danger`,
//! `Ai`, `Key`, `Dim`) and never which color; [`crate::text::render_line`] is the only place that
//! turns it into escape sequences. The meaning is always in the words or symbols as well (`✓` and
//! `ok`, `✘` and `x`), so nothing depends on color alone.

use std::time::Duration;

use omsh_config::Greeting;

use crate::caps::Caps;
use crate::text::{Line, Style, Tone};

/// A failed command shows how long it took only above this.
pub const SLOW: Duration = Duration::from_secs(2);

/// How many sessions show the hint bar when `ui.greeting` is `auto`.
pub const GREETING_SESSIONS: u32 = 5;

/// The thin line printed between two commands, `cols` wide.
#[must_use]
pub fn rule(cols: usize, unicode: bool) -> Line {
    let glyph = if unicode { "─" } else { "-" };
    Line::new().tone(Tone::Dim, &glyph.repeat(cols.max(1)))
}

/// How long a command took: `<1 ms`, `4 ms`, `1.2 s`, `1m05s`, `2h03m`. Always a real value
/// rounded down, never a made-up one.
#[must_use]
pub fn duration_text(took: Duration) -> String {
    let millis = took.as_millis();
    if millis < 1 {
        return "<1 ms".to_owned();
    }
    if millis < 1000 {
        return format!("{millis} ms");
    }
    let total = took.as_secs();
    if total < 60 {
        return format!("{}.{} s", total, millis % 1000 / 100);
    }
    let (hours, minutes, seconds) = (total / 3600, total % 3600 / 60, total % 60);
    if hours > 0 {
        format!("{hours}h{minutes:02}m")
    } else {
        format!("{minutes}m{seconds:02}s")
    }
}

/// How the last command ended: `✓ 4 ms` (green) after a success, `✘ 127` (red) after a failure,
/// with the time added to a failure only when it was slow. `None` when there is nothing to say:
/// no command ran yet.
#[must_use]
pub fn chip(status: u8, took: Option<Duration>, unicode: bool) -> Option<Line> {
    if status == 0 {
        let took = took?;
        let mark = if unicode { "✓" } else { "ok" };
        return Some(Line::new().tone(Tone::Ok, &format!("{mark} {}", duration_text(took))));
    }
    let mark = if unicode { "✘" } else { "x" };
    let mut text = format!("{mark} {status}");
    if let Some(took) = took.filter(|took| *took > SLOW) {
        text.push_str(if unicode { " · " } else { " - " });
        text.push_str(&duration_text(took));
    }
    Some(Line::new().tone(Tone::Danger, &text))
}

/// The tag shown at the right while the line being typed is a request to the AI.
#[must_use]
pub fn ai_tag() -> Line {
    Line::new().tone(Tone::Ai, "ask AI")
}

/// The dim hint beside the status chip after a command failed, while the AI can explain it:
/// the key that does it (`Alt+?`). Words and a key, not a color.
#[must_use]
pub fn explain_hint() -> Line {
    Line::new()
        .then("Alt+?", Style::of(Tone::Dim))
        .tone(Tone::Dim, " explain")
}

/// What the hint bar may say about the AI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AiHint {
    /// Switched off (`ai.enabled = false` or `--no-ai`): `?` is not offered.
    Off,
    /// Switched on but no provider chosen yet: `omsh ai setup` is offered.
    NotSetUp,
    /// Ready.
    Ready,
}

/// The one line shown above the first prompt of a session: the keys worth knowing.
#[must_use]
pub fn hint_bar(ai: AiHint, unicode: bool) -> Line {
    let separator = if unicode { " · " } else { " - " };
    let key = Style::of(Tone::Key);
    let dim = Style::of(Tone::Dim);
    let mut line = Line::new();
    let item = |line: &mut Line, keys: &str, what: &str| {
        if !line.is_empty() {
            line.push(separator, dim);
        }
        line.push(keys, key);
        if !what.is_empty() {
            line.push(" ", dim);
            line.push(what, dim);
        }
    };
    if ai != AiHint::Off {
        item(&mut line, "?", "ask AI");
    }
    item(&mut line, "Ctrl+P", "palette");
    item(&mut line, "Ctrl+R", "history");
    item(&mut line, "omsh tour", "");
    if ai == AiHint::NotSetUp {
        item(&mut line, "omsh ai setup", "");
    }
    line
}

/// Whether the hint bar is shown at this start. `shown` is how many sessions showed it before;
/// `welcome_now` is true when this start printed the first-run welcome (which teaches the same
/// things); `have_state` is false when there is nowhere to keep the count (`--no-config`).
#[must_use]
pub fn greeting_due(mode: Greeting, shown: u32, welcome_now: bool, have_state: bool) -> bool {
    match mode {
        Greeting::Never => false,
        Greeting::Always => true,
        Greeting::Auto => have_state && !welcome_now && shown < GREETING_SESSIONS,
    }
}

/// The OSC 133 marks of "semantic prompts" (a convention that kitty, WezTerm, iTerm2 and
/// others read): where a prompt starts, where the output of a command starts and where the
/// command ended with its status. They are written with the string terminator `ESC \`.
pub mod marks {
    use crate::caps::Caps;

    /// Before the prompt.
    pub const PROMPT_START: &str = "\x1b]133;A\x1b\\";
    /// After the command was typed, before it runs.
    pub const OUTPUT_START: &str = "\x1b]133;C\x1b\\";

    /// After the command ended, with its exit status.
    #[must_use]
    pub fn command_end(status: u8) -> String {
        format!("\x1b]133;D;{status}\x1b\\")
    }

    /// Whether the marks may be written: only to a terminal that is not `dumb`, and not on the
    /// Linux console (`TERM=linux`), which does not know them. `term` is the value of `TERM`.
    #[must_use]
    pub fn wanted(caps: &Caps, term: Option<&str>) -> bool {
        caps.stderr_tty && !caps.dumb && term != Some("linux")
    }
}

/// How wide the rule is for a window of this many columns: the last column stays free, as for
/// every panel.
#[must_use]
pub fn rule_width(caps: &Caps, window: Option<u16>) -> usize {
    usize::from(window.unwrap_or(caps.width).saturating_sub(1)).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn text(line: &Line) -> String {
        line.plain_text()
    }

    #[test]
    fn durations_are_real_values_rounded_down() {
        assert_eq!(duration_text(Duration::from_micros(300)), "<1 ms");
        assert_eq!(duration_text(ms(4)), "4 ms");
        assert_eq!(duration_text(ms(999)), "999 ms");
        assert_eq!(duration_text(ms(1000)), "1.0 s");
        assert_eq!(duration_text(ms(1299)), "1.2 s");
        assert_eq!(duration_text(ms(59_999)), "59.9 s");
        assert_eq!(duration_text(Duration::from_secs(65)), "1m05s");
        assert_eq!(
            duration_text(Duration::from_secs(2 * 3600 + 3 * 60)),
            "2h03m"
        );
    }

    #[test]
    fn a_success_is_a_green_check_with_its_time() {
        let chip = chip(0, Some(ms(4)), true).unwrap();
        assert_eq!(text(&chip), "✓ 4 ms");
        assert!(chip.spans().iter().all(|s| s.style.tone == Tone::Ok));
        assert_eq!(
            text(&super::chip(0, Some(ms(4)), false).unwrap()),
            "ok 4 ms"
        );
    }

    #[test]
    fn a_failure_is_a_red_cross_with_its_status_and_no_time_unless_slow() {
        let quick = chip(127, Some(ms(3)), true).unwrap();
        assert_eq!(text(&quick), "✘ 127");
        assert!(quick.spans().iter().all(|s| s.style.tone == Tone::Danger));
        assert_eq!(text(&chip(2, Some(SLOW), true).unwrap()), "✘ 2", "not over");
        assert_eq!(text(&chip(2, Some(ms(3200)), true).unwrap()), "✘ 2 · 3.2 s");
        assert_eq!(
            text(&chip(1, Some(ms(3200)), false).unwrap()),
            "x 1 - 3.2 s"
        );
    }

    #[test]
    fn nothing_is_said_before_a_command_has_run() {
        assert!(chip(0, None, true).is_none());
        // A failure without a time (a line that was refused) still says so.
        assert_eq!(text(&chip(2, None, true).unwrap()), "✘ 2");
    }

    #[test]
    fn the_rule_is_dim_and_as_wide_as_asked() {
        let line = rule(5, true);
        assert_eq!(text(&line), "─────");
        assert!(line.spans().iter().all(|s| s.style.tone == Tone::Dim));
        assert_eq!(text(&rule(3, false)), "---");
        assert_eq!(text(&rule(0, true)), "─", "never empty");
    }

    #[test]
    fn the_rule_leaves_the_last_column_free() {
        let caps = Caps::plain();
        assert_eq!(rule_width(&caps, Some(80)), 79);
        assert_eq!(rule_width(&caps, Some(1)), 1);
        assert_eq!(rule_width(&caps, None), usize::from(caps.width) - 1);
    }

    #[test]
    fn the_hint_bar_names_the_keys() {
        assert_eq!(
            text(&hint_bar(AiHint::Ready, true)),
            "? ask AI · Ctrl+P palette · Ctrl+R history · omsh tour"
        );
        assert_eq!(
            text(&hint_bar(AiHint::NotSetUp, true)),
            "? ask AI · Ctrl+P palette · Ctrl+R history · omsh tour · omsh ai setup"
        );
        assert_eq!(
            text(&hint_bar(AiHint::Off, false)),
            "Ctrl+P palette - Ctrl+R history - omsh tour"
        );
    }

    #[test]
    fn the_hint_bar_marks_keys_and_dims_the_words() {
        let line = hint_bar(AiHint::Ready, true);
        let tone_of = |what: &str| {
            line.spans()
                .iter()
                .find(|s| s.text().contains(what))
                .map(|s| s.style.tone)
        };
        assert_eq!(tone_of("Ctrl+P"), Some(Tone::Key));
        assert_eq!(tone_of("palette"), Some(Tone::Dim));
        assert_eq!(tone_of("·"), Some(Tone::Dim));
    }

    #[test]
    fn no_text_of_the_look_holds_the_prompt_symbol_text() {
        // The test harness looks for "$ " to find a prompt (and `omsh` as a program must not
        // print it), so no piece may contain it.
        for line in [
            hint_bar(AiHint::NotSetUp, false),
            hint_bar(AiHint::Ready, true),
            ai_tag(),
        ] {
            assert!(!text(&line).contains("$ "));
        }
    }

    #[test]
    fn the_greeting_rules() {
        use Greeting::{Always, Auto, Never};
        // auto: the first five sessions after the welcome.
        assert!(
            !greeting_due(Auto, 0, true, true),
            "not in the welcome session"
        );
        assert!(greeting_due(Auto, 0, false, true));
        assert!(greeting_due(Auto, 4, false, true));
        assert!(!greeting_due(Auto, 5, false, true), "the sixth is quiet");
        assert!(!greeting_due(Auto, 0, false, false), "nowhere to count");
        // always and never ignore the count.
        assert!(greeting_due(Always, 99, false, true));
        assert!(!greeting_due(Never, 0, false, true));
    }

    #[test]
    fn the_marks_are_the_three_osc_133_sequences_with_a_string_terminator() {
        assert_eq!(marks::PROMPT_START, "\x1b]133;A\x1b\\");
        assert_eq!(marks::OUTPUT_START, "\x1b]133;C\x1b\\");
        assert_eq!(marks::command_end(0), "\x1b]133;D;0\x1b\\");
        assert_eq!(marks::command_end(130), "\x1b]133;D;130\x1b\\");
    }

    fn terminal() -> Caps {
        Caps {
            stderr_tty: true,
            ..Caps::plain()
        }
    }

    #[test]
    fn the_marks_are_for_a_terminal_that_knows_them_only() {
        let tty = terminal();
        assert!(marks::wanted(&tty, Some("xterm-kitty")));
        assert!(
            marks::wanted(&tty, None),
            "an unset TERM is not the console"
        );
        assert!(!marks::wanted(&tty, Some("linux")), "the Linux console");
        assert!(!marks::wanted(&Caps { dumb: true, ..tty }, Some("dumb")));
        assert!(
            !marks::wanted(&Caps::plain(), Some("xterm")),
            "not a terminal"
        );
    }
}
