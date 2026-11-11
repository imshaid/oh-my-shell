//! Risk analysis of parsed commands (§12), the blast radius of file commands (§12.3), the trash
//! (§12.4) and the undo journal (§12.5).
//!
//! [`analyze`] looks at a parsed line, offline, and says how risky each command in it is. The
//! rules are data: [`RULES`] is a table, each entry with an id, a level, a reason in plain
//! words and a matcher over the syntax tree. Nothing here runs a command, opens a network
//! connection, or writes a file.

pub mod blast;
pub mod changes;
pub mod command;
mod context;
mod find;
pub mod journal;
mod level;
pub mod paths;
mod rules;
pub mod trash;
mod walk;
pub mod word;

pub use context::{Context, FsProbe, Paths, Probe};
pub use level::Level;
pub use rules::{Env, Hit, Matcher, RULES, Rule};
pub use walk::{Analysis, Finding, analyze};

#[cfg(test)]
mod blast_tests;
#[cfg(test)]
mod sites_tests;
#[cfg(test)]
mod tests;
