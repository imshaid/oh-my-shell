//! Configuration schema, loading, validation and XDG paths (blueprint §9).
//!
//! - [`Paths`] works out the directories from `XDG_*_HOME` and `HOME`.
//! - [`Config`] is the typed configuration with the defaults of the blueprint.
//! - [`load`] reads `config.toml`, writes the commented first-run file when there is none, and
//!   never fails: a wrong setting keeps its default and is returned as a [`Problem`] with its
//!   line and column.
//! - [`parse`] does the same for a text, which is what the tests use.
//!
//! This crate depends on no other crate of the workspace.

mod ai_state;
mod edit;
mod first_run;
mod load;
mod paths;
mod read;
mod schema;
pub mod settings;

pub use ai_state::AiState;
pub use edit::{AiChoice, Change, EditError, Edited, set_ai, set_setting};
pub use first_run::{TEMPLATE, create as write_if_missing};
pub use load::{Loaded, Parsed, Problem, Severity, check_url, load, parse};
pub use paths::Paths;
pub use read::read_text_file;
pub use schema::{
    Ai, AiContext, AiLanguage, Channel, Config, ConfirmLevel, EditMode, Editor, History, Icons,
    Motion, NOTIFY_IGNORE, Notify, PROMPT_SEGMENTS, Prompt, Provider, SECTIONS, Safety, Ui, Update,
};
