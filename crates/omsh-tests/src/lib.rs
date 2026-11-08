//! End-to-end tests that drive the shell in a pseudo-terminal (development only).
//!
//! The library is the harness; the tests are in `tests/`. A test looks like this:
//!
//! ```ignore
//! const BIN: &str = env!("CARGO_BIN_EXE_omsh-pty-shell");
//!
//! #[test]
//! fn ctrl_c_stops_the_job() {
//!     pty_test(BIN, |ctx| {
//!         let mut term = ctx.start()?;     // the shell, at its first prompt
//!         term.line("sleep 30")?;
//!         term.key(3)?;                    // Ctrl+C
//!         term.sync("alive")?;             // the shell answers
//!         Ok(())
//!     });
//! }
//! ```
//!
//! No test can hang: every wait in [`Session`] has a deadline, and [`pty_test`] fails a test that
//! takes longer than a minute and kills every process it started.
//!
//! `omsh-pty-shell` (`src/bin`) is the shell: it calls the entry function of `omsh-cli`, the same
//! one the `oh-my-shell` binary calls. Cargo only passes a test the path of a binary of its own
//! package, which is why it exists.
//!
//! A test that cannot run here (for example SIGHUP is ignored) calls [`skip::skip`]: a visible
//! note locally, a failure when `CI=true`, so CI never passes without running it.
//!
//! Needs Linux, `/proc` and `setsid` from util-linux.

pub mod procfs;
pub mod session;
pub mod skip;
pub mod watchdog;

pub use session::{Launch, SHORT, Session};
pub use watchdog::{Ctx, WHOLE_TEST, pty_test, pty_test_within};

#[cfg(test)]
mod tests;
