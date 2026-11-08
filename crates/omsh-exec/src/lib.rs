//! Word expansion, command execution, builtins, job control, signals and the
//! environment table.
//!
//! This is the only crate in the workspace that may use `unsafe`, and only in the modules
//! that say so: `spawn` (fork and exec) and `signals`. Every `unsafe` block needs a
//! `// SAFETY:` comment, which Clippy enforces.
//!
//! Today it can run a whole line of the shell language with every kind of word expansion:
//! pipelines, redirections, `&&` and `||`, compound commands, functions and the core builtins.
//! [`Shell::run_source`] parses a line and runs it. Background jobs (S09) are still to come,
//! and a line that needs them is refused whole by [`Shell::run`] before anything runs.
//!
//! In an interactive shell each command is a *job*: a process group of its own that owns the
//! terminal while it runs, so Ctrl+C stops the job and not the shell (see `jobs.rs` and
//! `terminal.rs`).

mod alias;
mod builtins;
mod check;
mod exec;
mod expand;
mod io;
mod jobs;
mod options;
mod path;
mod redirect;
mod shell;
mod signals;
mod spawn;
mod status;
mod terminal;
mod traps;
mod vars;

pub use check::NotYet;
pub use io::{FdPlan, Io, Slot};
pub use omsh_syntax::{Span, SyntaxDiagnostic};
pub use path::{DEFAULT_PATH, Lookup, PathCache};
pub use shell::{LineResult, Outcome, Shell};
pub use signals::{clear_interrupt, enter_interactive, interrupted, note_inherited_signals};
pub use spawn::{ChildGroup, ExecRequest, SpawnFailure, start, wait};
pub use status::{ExitStatus, Termination};
pub use terminal::{release_terminal, start_interactive};
pub use vars::{ReadOnly, Variables};
