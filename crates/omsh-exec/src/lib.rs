//! Word expansion, command execution, builtins, job control, signals and the
//! environment table.
//!
//! This is the only crate in the workspace that may use `unsafe`, and only in the modules
//! that say so: `spawn` (fork and exec) and `signals`. Every `unsafe` block needs a
//! `// SAFETY:` comment, which Clippy enforces.
//!
//! Today it can run simple external commands: [`Shell::run_source`] parses a line and runs
//! it. Expansion (S06), pipes and redirections (S07) and job control (S08, S09) are still to
//! come, and a line that needs them is refused whole by [`Shell::run`] before anything runs.

mod env;
mod expand;
mod path;
mod shell;
mod signals;
mod spawn;
mod status;

pub use env::Environment;
pub use expand::NotYet;
pub use omsh_syntax::{Span, SyntaxDiagnostic};
pub use path::{DEFAULT_PATH, Lookup, PathCache};
pub use shell::{LineResult, Outcome, Shell};
pub use signals::ignore_interrupts;
pub use spawn::{ExecRequest, SpawnFailure};
pub use status::{ExitStatus, Termination};
