//! Theme tokens, UI components, motion and full-screen views.
//!
//! So far this crate holds the [`theme`]: the table from role to look, made of the 16 ANSI color
//! names only. Components, motion and views come in S26 and S27.

pub mod theme;

pub use theme::{AnsiColor, Role, RoleStyle, Theme};
