//! The line under an AI answer on a terminal (spec section 6, "Inline info line"): the fields
//! `[ai.inline] fields` names, in that order, dim, joined by ` · `.
//!
//! - A field whose number the provider did not report shows a dash: `in –`. Never 0.
//! - `Tab` in the plan panel shows the expanded line: the three fields that are not on the line
//!   yet among `thinking`, `cached` and `total`.
//! - A second line in yellow, starting with the word `warning`, appears when the thinking tokens
//!   are far above the answer's (`[ai.inline] thinking_warn` times, 0 turns it off).
//! - The speed is **derived** (tokens over generation time) and says so.
//!
//! A pipe or a plain terminal does not get this line: it keeps the plain `3.2 s, 1,204 tokens in,
//! 310 out` of the shell.

use crate::caps::Caps;
use crate::panels::facts::{CallFacts, Marks, count_or_dash, seconds};
use crate::panels::meter::Meter;
use crate::text::{Line, Style, Tone};

/// What to show.
#[derive(Clone, Debug)]
pub struct InfoOptions<'a> {
    /// The names of `[ai.inline] fields`, already checked by the config loader.
    pub fields: &'a [String],
    /// Warn at this many times; 0 is off.
    pub thinking_warn: u32,
    /// The expanded line (`Tab`).
    pub expanded: bool,
}

/// The fields that `Tab` adds, in this order.
const EXPANDED: [&str; 3] = ["thinking", "cached", "total"];

/// The text of one field.
fn field(name: &str, facts: &CallFacts, caps: &Caps, meter: &Meter) -> Option<String> {
    let marks = Marks::of(caps);
    let dash = marks.dash;
    Some(match name {
        "model" => facts.model.clone(),
        "effort" => format!("effort {}", facts.effort.as_deref().unwrap_or(dash)),
        "time" => facts.time.map_or_else(|| dash.to_owned(), seconds),
        "in" => format!("in {}", count_or_dash(facts.input, marks)),
        "out" => format!("out {}", count_or_dash(facts.output, marks)),
        "thinking" => format!("thinking {}", count_or_dash(facts.thinking, marks)),
        "cached" => format!("cached {}", count_or_dash(facts.cached, marks)),
        "total" => format!("total {}", count_or_dash(facts.total, marks)),
        "speed" => facts.tokens_per_sec.map_or_else(
            || format!("speed {dash}"),
            |speed| format!("speed {speed:.1} tok/s (derived)"),
        ),
        "ctx" => meter.summary(caps).plain_text(),
        _ => return None,
    })
}

/// The warning for a thinking spike, if one is due.
fn warning(facts: &CallFacts, warn_at: u32, caps: &Caps) -> Option<Line> {
    let thinking = facts.thinking.filter(|t| *t > 0)?;
    let output = facts.output?;
    if warn_at == 0 {
        return None;
    }
    let marks = Marks::of(caps);
    let text = if output == 0 {
        format!(
            "warning: all {} output tokens went to thinking and the answer is empty",
            crate::panels::facts::group(thinking)
        )
    } else if u64::from(thinking) >= u64::from(output) * u64::from(warn_at) {
        format!(
            "warning: thinking used {:.1}{} the answer's tokens ({} vs {})",
            f64::from(thinking) / f64::from(output),
            marks.times,
            crate::panels::facts::group(thinking),
            crate::panels::facts::group(output)
        )
    } else {
        return None;
    };
    Some(Line::new().then(&text, Style::of(Tone::Warn)))
}

/// The lines under an answer: the info line, and the warning if one is due.
#[must_use]
pub fn info_lines(
    facts: &CallFacts,
    options: &InfoOptions<'_>,
    meter: &Meter,
    caps: &Caps,
) -> Vec<Line> {
    let mut names: Vec<&str> = options.fields.iter().map(String::as_str).collect();
    if options.expanded {
        for extra in EXPANDED {
            if !names.contains(&extra) {
                names.push(extra);
            }
        }
    }
    let parts: Vec<String> = names
        .iter()
        .filter_map(|name| field(name, facts, caps, meter))
        .collect();
    let mut out = Vec::new();
    if !parts.is_empty() {
        let separator = Marks::of(caps).sep;
        let mut line = Line::new();
        for (index, part) in parts.iter().enumerate() {
            if index > 0 {
                line.push(separator, Style::of(Tone::Dim));
            }
            line.push(part, Style::of(Tone::Muted));
        }
        out.push(line);
    }
    out.extend(warning(facts, options.thinking_warn, caps));
    out
}

#[cfg(test)]
mod tests;
