//! Word expansion, command execution, builtins, job control, signals and the
//! environment table.
//!
//! This is the only crate in the workspace that may use `unsafe`, and only in the modules
//! that say so: `spawn` (fork and exec) and `signals`. Every `unsafe` block needs a
//! `// SAFETY:` comment, which Clippy enforces.
//!
//! Today it can run a whole line of the shell language with every kind of word expansion:
//! pipelines, redirections, `&&` and `||`, compound commands, functions and the core builtins.
//! [`Shell::run_source`] parses a line and runs it. A line that needs something not implemented
//! yet is refused whole by [`Shell::run`] before anything runs (today nothing is refused).
//!
//! In an interactive shell each command is a *job*: a process group of its own that owns the
//! terminal while it runs, so Ctrl+C stops the job and not the shell (see `jobs.rs` and
//! `terminal.rs`). A command that ends with `&` is a background job: it runs without the
//! terminal and is listed in the job table (`jobtable.rs`), with `jobs`, `wait` and `kill`.

mod alias;
mod builtins;
mod check;
mod color;
mod exec;
mod expand;
mod io;
mod jobs;
mod jobtable;
mod login;
mod options;
mod path;
mod redirect;
mod shell;
mod signals;
mod spawn;
mod status;
mod suggest;
mod terminal;
mod traps;
mod user;
mod vars;

pub use builtins::{
    BuiltinOutput, HistoryCommand, JournalCommand, JumpCommand, Key, OmshCommand, ReadError, TRASH,
    Terminal, builtin_names, is_builtin, summary as builtin_summary,
};
pub use check::NotYet;
pub use expand::glob;
pub use io::{FdPlan, Io, Slot};
pub use omsh_syntax::{Span, SyntaxDiagnostic, Trigger, trigger};
pub use path::{DEFAULT_PATH, Lookup, PathCache, find_program};
pub use shell::{LineResult, Outcome, Prepared, Shell};
pub use signals::{
    clear_interrupt, enter_interactive, hung_up, interrupted, note_inherited_signals,
};
pub use spawn::{ChildGroup, ExecRequest, SpawnFailure, start, wait};
pub use status::{ExitStatus, Termination};
pub use suggest::CommandSuggest;
pub use terminal::{release_terminal, start_interactive, terminal_gone, wait_for_input};
pub use user::{effective_uid, is_root};
pub use vars::{ReadOnly, Variables};
