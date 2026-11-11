//! Risk analysis of parsed commands (§12). Blast radius, the trash and the undo journal come
//! in later segments.
//!
//! [`analyze`] looks at a parsed line, offline, and says how risky each command in it is. The
//! rules are data: [`RULES`] is a table, each entry with an id, a level, a reason in plain
//! words and a matcher over the syntax tree. Nothing here runs a command, opens a network
//! connection, or writes a file.

pub mod command;
mod context;
mod level;
pub mod paths;
mod rules;
mod walk;
pub mod word;

pub use context::{Context, FsProbe, Paths, Probe};
pub use level::Level;
pub use rules::{Env, Hit, Matcher, RULES, Rule};
pub use walk::{Analysis, Finding, analyze};

#[cfg(test)]
mod tests;
