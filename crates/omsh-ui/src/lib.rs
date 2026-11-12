//! Theme tokens, UI components, motion and full-screen views.
//!
//! - [`theme`]: the table from role and token to look, made of the 16 ANSI color names only.
//! - [`theme_file`]: `themes/<name>.toml`, which overrides any of them.
//! - [`look`]: which of color, Unicode and motion the terminal gets (the degrade rules).
//! - [`paint`]: wraps text in the theme's colors, and the glyphs panels use.
//! - [`motion`]: the activity spinner and its redraw rule.
//!
//! Full-screen views come in S27.

pub mod look;
pub mod motion;
pub mod paint;
pub mod theme;
pub mod theme_file;

pub use look::{Facts, Look};
pub use motion::{Spinner, redraw_interval};
pub use paint::{Glyphs, Paint};
pub use theme::{AnsiColor, Role, RoleStyle, Theme, Token};
pub use theme_file::{LoadedTheme, load_theme, parse_theme};
