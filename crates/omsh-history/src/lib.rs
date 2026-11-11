//! SQLite command history and its query API (blueprint §11).
//!
//! - [`Store`] is the history: a SQLite database in WAL mode that every open shell shares, or
//!   a database in memory. Writing happens on a thread of its own; see `store.rs`.
//! - [`Query`] says what to look for and [`Entry`] is one stored command.
//! - [`Filter`] decides which lines are stored: not the ones that match a `[history] ignore`
//!   pattern, and never one that looks like it holds a secret (the rules are in `omsh-secrets`).
//!
//! This crate depends on `omsh-secrets` only.

mod filter;
mod store;

pub use filter::Filter;
pub use store::{Entry, Query, SETTLE, Store, Token, now_ms};
