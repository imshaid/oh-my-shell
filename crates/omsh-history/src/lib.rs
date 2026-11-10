//! SQLite command history and its query API (blueprint §11).
//!
//! - [`Store`] is the history: a SQLite database in WAL mode that every open shell shares, or
//!   a database in memory. Writing happens on a thread of its own; see `store.rs`.
//! - [`Query`] says what to look for and [`Entry`] is one stored command.
//! - [`Filter`] decides which lines are stored: not the ones that match a `[history] ignore`
//!   pattern, and never one that looks like it holds a secret ([`secrets`]).
//!
//! This crate depends on no other crate of the workspace.

mod filter;
pub mod secrets;
mod store;

pub use filter::Filter;
pub use store::{Entry, Query, Store, Token, now_ms};
