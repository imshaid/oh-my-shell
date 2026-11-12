//! The pieces every full-screen view shares: the loop that owns the terminal ([`run`]), the
//! typed line with its cursor ([`input`]), the chosen row of a list ([`select`]) and the drawing
//! helpers and styles ([`draw`]). The history explorer, the command palette and the settings
//! view are each a state, a drawing and a handful of keys on top of these.

pub mod draw;
pub mod input;
pub mod run;
pub mod select;
