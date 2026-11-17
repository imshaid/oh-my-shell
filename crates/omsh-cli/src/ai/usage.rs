//! What this shell session knows about its AI calls (UI upgrade, phase 4).
//!
//! After every answer the call is recorded: the provider and model, the numbers the provider
//! reported (all `Option`: a number that was not reported is shown as a dash, never as 0), the
//! time the shell measured, and the size of the context window when it is known. The newest call
//! of each provider is kept, for the info line, the plan panel and the details panel
//! (`omsh ai details`, `i` in the plan panel).
//!
//! Nothing here touches the network: the window was asked about next to the request (see
//! `omsh_ai::alongside`) and arrives as a plain value.

use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use omsh_ai::{Limit, LimitSource, Usage};
use omsh_config::{Ai, AiInline, Provider as Kind};
use omsh_ui::panels::MenuUi;
use omsh_ui::panels::facts::CallFacts;
use omsh_ui::panels::info::{InfoOptions, info_lines};
use omsh_ui::panels::meter::Meter;
use omsh_ui::panels::plan::CallLine;
use omsh_ui::text::render;

/// One finished call.
#[derive(Clone, Debug)]
pub struct Record {
    pub kind: Kind,
    pub model: String,
    /// What the provider reported; `None` when it reported nothing.
    pub usage: Option<Usage>,
    pub time: Duration,
    /// The window, when a lookup found it. A `[ai] context_limit` is applied on top of this.
    pub limit: Option<Limit>,
    /// What the meter showed after the call before this one of the same provider, for easing.
    pub before: Option<u32>,
}

/// The calls of this session and the settings that decide how they are shown.
pub struct Calls {
    records: Mutex<Vec<Record>>,
    config_limit: Option<u32>,
    inline: AiInline,
}

impl Calls {
    #[must_use]
    pub fn new(ai: &Ai) -> Self {
        Self {
            records: Mutex::default(),
            config_limit: ai.context_limit,
            inline: ai.inline.clone(),
        }
    }

    /// With the defaults of a shell with no configuration.
    #[cfg(test)]
    #[must_use]
    pub fn standard() -> Self {
        Self::new(&Ai::default())
    }

    /// The window the person wrote in `config.toml`, if any. When it is set no lookup is made.
    #[must_use]
    pub const fn config_limit(&self) -> Option<u32> {
        self.config_limit
    }

    /// Remembers a finished call (it replaces the one before it of the same provider) and gives
    /// the record back.
    pub fn record(
        &self,
        kind: Kind,
        model: &str,
        usage: Option<Usage>,
        time: Duration,
        limit: Option<Limit>,
    ) -> Record {
        let mut records = self.records.lock().unwrap_or_else(PoisonError::into_inner);
        let slot = records.iter().position(|record| record.kind == kind);
        let before = slot.and_then(|index| context_used(records[index].usage.as_ref()));
        let record = Record {
            kind,
            model: model.to_owned(),
            usage,
            time,
            limit,
            before,
        };
        match slot {
            Some(index) => records[index] = record.clone(),
            None => records.push(record.clone()),
        }
        record
    }

    /// The newest call of each provider, in the order the providers were first used.
    #[must_use]
    pub fn records(&self) -> Vec<Record> {
        self.records
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The window of a record: the person's own number if there is one, else what was found.
    fn window(&self, record: &Record) -> (Option<u32>, Option<&'static str>) {
        match (self.config_limit, record.limit) {
            (Some(tokens), _) => (Some(tokens), Some(LimitSource::Config.note())),
            (None, Some(limit)) => (Some(limit.tokens), Some(limit.source.note())),
            (None, None) => (None, None),
        }
    }

    /// The facts of a call, for the panels.
    #[must_use]
    pub fn facts(&self, record: &Record) -> CallFacts {
        let usage = record.usage.unwrap_or_default();
        let (limit, note) = self.window(record);
        CallFacts {
            provider: record.kind.label().to_owned(),
            model: record.model.clone(),
            effort: None,
            time: Some(record.time),
            input: usage.input,
            thinking: usage.thinking,
            output: usage.output,
            cached: usage.cached,
            total: usage.total,
            tokens_per_sec: usage.tokens_per_sec(),
            context_used: context_used(record.usage.as_ref()),
            context_limit: limit,
            limit_note: note.map(str::to_owned),
        }
    }

    /// The facts of every provider used so far, for the details panel.
    #[must_use]
    pub fn tabs(&self) -> Vec<CallFacts> {
        self.records()
            .iter()
            .map(|record| self.facts(record))
            .collect()
    }

    /// The call as the plan panel shows it.
    #[must_use]
    pub fn call_line(&self, record: &Record) -> CallLine {
        let facts = self.facts(record);
        let meter = Meter::new(facts.context_used, facts.context_limit, record.before);
        CallLine {
            facts,
            fields: self.inline.fields.clone(),
            thinking_warn: self.inline.thinking_warn,
            meter,
        }
    }

    /// The info line (and the warning, if one is due) of a call, written for this terminal: each
    /// line indented by two columns, ending in a newline. Empty when `[ai.inline] fields` is
    /// empty.
    #[must_use]
    pub fn info_text(&self, ui: &MenuUi, record: &Record) -> String {
        let facts = self.facts(record);
        let meter = Meter::new(facts.context_used, facts.context_limit, record.before);
        let options = InfoOptions {
            fields: &self.inline.fields,
            thinking_warn: self.inline.thinking_warn,
            expanded: false,
        };
        let lines = info_lines(&facts, &options, &meter, &ui.caps);
        let mut out = String::new();
        for row in render(&lines, &ui.theme, &ui.caps) {
            out.push_str("  ");
            out.push_str(&row);
            out.push('\n');
        }
        out
    }
}

/// What the next call starts with, roughly: the provider's total for this one, or the input plus
/// the completion when there is no total.
fn context_used(usage: Option<&Usage>) -> Option<u32> {
    let usage = usage?;
    usage.total.or_else(|| {
        let completion = usage.completion();
        match (usage.input, completion) {
            (Some(input), Some(completion)) => input.checked_add(completion),
            _ => None,
        }
    })
}

#[cfg(test)]
mod tests;
