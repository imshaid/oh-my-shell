//! Word expansion, command execution, builtins, job control, signals and the
//! environment table.
//!
//! This is the only crate in the workspace that may use `unsafe`, and only on the
//! fork/exec path. Every `unsafe` block needs a `// SAFETY:` comment, which Clippy
//! enforces.
//!
//! Not implemented yet: this crate only holds its place in the workspace.

#![allow(unsafe_code, reason = "the fork/exec path needs `unsafe`")]
