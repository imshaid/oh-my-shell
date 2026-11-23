//! Theme tokens, UI components, motion and full-screen views.
//!
//! - [`theme`]: the table from role and token to look, made of the 16 ANSI color names only.
//! - [`theme_file`]: `themes/<name>.toml`, which overrides any of them.
//! - [`look`]: which of color, Unicode and motion the terminal gets (the degrade rules).
//! - [`paint`]: wraps text in the theme's colors, and the glyphs panels use.
//! - [`motion`]: the activity spinner and its redraw rule, easing and the frame clock.
//!
//! The UI upgrade (`docs/UI_UPGRADE.md`): a foundation that every panel is built on, and the panels.
//! A panel is data: `render(state, caps)` gives lines of styled text, and one writer turns them
//! into escape sequences for the terminal at hand.
//!
//! Faint text (the `dim` token) is for decoration only (a rule, a separator, the empty part of a
//! bar). Text that carries information is the `muted` token, the terminal's own text color,
//! because faint text is hard to read on some terminal themes.
//!
//! - [`caps`]: what the terminal allows, decided by one pure function.
//! - [`text`]: styled lines, widths, and the one writer of escape sequences.
//! - [`boxes`]: rounded boxes, full or open on the right.
//! - [`widgets`]: buttons, a filterable list and the numbered fallback.
//! - [`inline`]: drawing a menu below the prompt and erasing it again.
//! - [`shell_look`]: the rule between commands, the status chip, the hint bar and the OSC 133 marks
//!   (UI upgrade, phase 2).
//! - [`sync`]: does the terminal understand synchronized output (DEC mode 2026).
//! - [`panels`]: what the shell asks and shows in a box: the "Run it?" question, the `undo`
//!   picker, the AI plan, the details of a call, the setup lists, the first-run questions,
//!   `omsh status`, the jobs table and the panels of a running plan.
//!
//! - [`explorer`], [`fuzzy`], [`screen`], [`style`]: the full-screen history explorer (S27a): its
//!   state, the small fuzzy matcher, the alternate screen with its panic restore, and the mapping
//!   from theme to ratatui styles.
//! - [`view`], [`palette`], [`settings`]: the loop and the pieces the full-screen views share, the
//!   command palette (`Ctrl+P`) and the settings view.

pub mod boxes;
pub mod caps;
pub mod explorer;
pub mod fuzzy;
pub mod inline;
pub mod look;
pub mod motion;
pub mod paint;
pub mod palette;
pub mod panels;
pub mod screen;
pub mod settings;
pub mod shell_look;
pub mod style;
pub mod sync;
pub mod text;
pub mod theme;
pub mod theme_file;
pub mod view;
pub mod widgets;

pub use caps::{Caps, CapsFacts, Out};
pub use fuzzy::{Haystack, Query};
pub use look::{Facts, Look};
pub use motion::{Spinner, redraw_interval};
pub use paint::{Glyphs, Paint};
pub use screen::install_panic_restore;
pub use text::{Line, Span, Style, Tone};
pub use theme::{AnsiColor, Role, RoleStyle, Theme, Token};
pub use theme_file::{LoadedTheme, load_theme, parse_theme};
