//! The context meter: how much of the model's window the conversation fills (spec section 6).
//!
//! `ctx ≈ 3,400 / 32,768  10%  █░░░░░░░░░`. The number is **approximate** (it is the last call's
//! total, which is what the next call starts with), so it always carries `≈`. The percent is
//! always written; the color only adds to it: green below 70 %, yellow from 70 %, red from 90 %.
//! From 80 % a dim `compact?` hint says the history could be summarized. An unknown window shows
//! `limit unknown` and a dim bar. With full motion the number eases from the old value to the new
//! one in 600 ms; with reduced motion or none it is at the new value at once.

use std::time::Duration;

use crate::caps::Caps;
use omsh_config::Motion;

use crate::motion::{TWEEN, Tween};
use crate::panels::facts::{Marks, group};
use crate::text::{Line, Style, Tone};

/// Percent at which the meter turns yellow.
pub const WARN_AT: u32 = 70;
/// Percent at which the hint to compact appears.
pub const COMPACT_AT: u32 = 80;
/// Percent at which the meter turns red.
pub const DANGER_AT: u32 = 90;
/// Cells of the bar.
pub const CELLS: usize = 10;

/// How much of the window `used` fills, in whole percent (rounded down, so that a value just
/// under a threshold is still below it).
#[must_use]
pub fn percent(used: u32, limit: u32) -> u32 {
    if limit == 0 {
        return 0;
    }
    u32::try_from(u64::from(used) * 100 / u64::from(limit)).unwrap_or(u32::MAX)
}

/// The color of the meter at this percent.
#[must_use]
pub const fn tone_at(percent: u32) -> Tone {
    if percent >= DANGER_AT {
        Tone::Danger
    } else if percent >= WARN_AT {
        Tone::Warn
    } else {
        Tone::Ok
    }
}

/// The meter of one call.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Meter {
    used: Option<u32>,
    limit: Option<u32>,
    /// Where the number starts when it eases (the value before this call).
    from: u32,
}

impl Meter {
    /// `used` of `limit`. `before` is the value the meter had before this call, if there was one
    /// (the number eases from it).
    #[must_use]
    pub fn new(used: Option<u32>, limit: Option<u32>, before: Option<u32>) -> Self {
        Self {
            used,
            limit: limit.filter(|limit| *limit > 0),
            from: before.unwrap_or(0),
        }
    }

    /// True when neither the use nor the window is known: there is nothing to show.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.used.is_none() && self.limit.is_none()
    }

    /// The value shown `elapsed` after the meter appeared.
    #[must_use]
    pub fn shown(&self, elapsed: Duration, motion: Motion) -> Option<u32> {
        let used = self.used?;
        let tween = Tween::new(f64::from(self.from), f64::from(used), TWEEN);
        Some(tween.value(elapsed, motion).round() as u32)
    }

    /// True while the number is still moving.
    #[must_use]
    pub fn moving(&self, elapsed: Duration, motion: Motion) -> bool {
        self.used.is_some()
            && !Tween::new(
                f64::from(self.from),
                f64::from(self.used.unwrap_or(0)),
                TWEEN,
            )
            .done(elapsed, motion)
    }

    /// The meter with its bar, `elapsed` after it appeared.
    #[must_use]
    pub fn line(&self, caps: &Caps, elapsed: Duration) -> Line {
        let shown = self.shown(elapsed, caps.motion);
        line(shown, self.limit, caps, true)
    }

    /// The meter without the bar, for the info line.
    #[must_use]
    pub fn summary(&self, caps: &Caps) -> Line {
        line(self.used, self.limit, caps, false)
    }
}

/// The meter for given numbers.
#[must_use]
pub fn line(used: Option<u32>, limit: Option<u32>, caps: &Caps, bar: bool) -> Line {
    let marks = Marks::of(caps);
    let (full, empty) = if caps.unicode {
        ("█", "░")
    } else {
        ("#", "-")
    };
    let head = format!(
        "ctx {} {}",
        marks.approx,
        used.map_or_else(|| marks.dash.to_owned(), group)
    );
    let mut out = Line::new();
    let (Some(used), Some(limit)) = (used, limit) else {
        let tail = match limit {
            Some(limit) => format!(" / {}", group(limit)),
            None => " / limit unknown".to_owned(),
        };
        out.push(&format!("{head}{tail}"), Style::of(Tone::Dim));
        if bar {
            out.push("  ", Style::PLAIN);
            out.push(&empty.repeat(CELLS), Style::of(Tone::Dim));
        }
        return out;
    };
    let pct = percent(used, limit);
    let tone = tone_at(pct);
    out.push(&format!("{head} / {}", group(limit)), Style::of(tone));
    out.push(&format!("  {pct}%"), Style::of(tone));
    if bar {
        let cells = if used == 0 {
            0
        } else {
            ((pct as usize * CELLS) / 100).clamp(1, CELLS)
        };
        out.push("  ", Style::PLAIN);
        out.push(&full.repeat(cells), Style::of(tone));
        out.push(&empty.repeat(CELLS - cells), Style::of(Tone::Dim));
    }
    if pct >= COMPACT_AT {
        out.push("  compact?", Style::of(Tone::Dim));
    }
    out
}

#[cfg(test)]
mod tests;
