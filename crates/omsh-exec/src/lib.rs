//! Word expansion, command execution, builtins, job control, signals and the
//! environment table.
//!
//! This is the only crate in the workspace that may use `unsafe`, and only in the modules
//! that say so: `spawn` (fork and exec) and `signals`. Every `unsafe` block needs a
//! `// SAFETY:` comment, which Clippy enforces.
//!
//! Today it can run a whole line of the shell language with every kind of word expansion:
//! pipelines, redirections, `&&` and `||`, compound commands, functions and the core builtins.
//! [`Shell::run_source`] parses a line and runs it. Background jobs and job control (S08, S09)
//! are still to come, and a line that needs them is refused whole by [`Shell::run`] before
//! anything runs.

mod alias;
mod builtins;
mod check;
mod exec;
mod expand;
mod io;
mod options;
mod path;
mod redirect;
mod shell;
mod signals;
mod spawn;
mod status;
mod vars;

pub use check::NotYet;
pub use io::{FdPlan, Io, Slot};
pub use omsh_syntax::{Span, SyntaxDiagnostic};
pub use path::{DEFAULT_PATH, Lookup, PathCache};
pub use shell::{LineResult, Outcome, Shell};
pub use signals::{catch_interrupts, clear_interrupt, interrupted};
pub use spawn::{ExecRequest, SpawnFailure, start, wait};
pub use status::{ExitStatus, Termination};
pub use vars::Variables;
