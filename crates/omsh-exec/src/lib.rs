//! Word expansion, command execution, builtins, job control, signals and the
//! environment table.
//!
//! This is the only crate in the workspace that may use `unsafe`, and only in the modules
//! that say so: `spawn` (fork and exec) and `signals`. Every `unsafe` block needs a
//! `// SAFETY:` comment, which Clippy enforces.
//!
//! Today it can run simple external commands with every kind of word expansion:
//! [`Shell::run_source`] parses a line and runs it. Pipes and redirections (S07) and job
//! control (S08, S09) are still to come, and a line that needs them is refused whole by
//! [`Shell::run`] before anything runs.

mod check;
mod expand;
mod path;
mod shell;
mod signals;
mod spawn;
mod status;
mod vars;

pub use check::NotYet;
pub use omsh_syntax::{Span, SyntaxDiagnostic};
pub use path::{DEFAULT_PATH, Lookup, PathCache};
pub use shell::{LineResult, Outcome, Shell};
pub use signals::ignore_interrupts;
pub use spawn::{ExecRequest, SpawnFailure};
pub use status::{ExitStatus, Termination};
pub use vars::Variables;
