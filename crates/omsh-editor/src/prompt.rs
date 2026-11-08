//! The prompt: what is drawn left and right of the line being typed.

use std::borrow::Cow;
use std::path::PathBuf;
use std::time::Duration;

use reedline::{
    Color, Prompt, PromptEditMode, PromptHistorySearch, PromptHistorySearchStatus, PromptViMode,
};

use crate::segments::{self, Segment};

/// How the prompt is drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Look {
    /// Box and arrow characters from Unicode (`❯`), else ASCII (`$`).
    pub unicode: bool,
    /// ANSI colors. Off for `NO_COLOR`.
    pub color: bool,
    /// Segments left of the line, in order (`[prompt] left`).
    pub left: Vec<Segment>,
    /// Segments right of the line, in order (`[prompt] right`).
    pub right: Vec<Segment>,
}

/// What the prompt needs to know about the shell right now. The shell fills it in before each
/// line is read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptState {
    /// The working directory (`PWD`).
    pub cwd: PathBuf,
    pub home: Option<PathBuf>,
    /// `VIRTUAL_ENV`.
    pub virtual_env: Option<PathBuf>,
    /// `CONDA_DEFAULT_ENV`.
    pub conda_env: Option<String>,
    /// `$?` of the last command.
    pub last_status: u8,
    /// How long the last command took, if there was one.
    pub last_duration: Option<Duration>,
    /// The shell runs as root: the indicator is `#`.
    pub root: bool,
}

/// The prompt as text, built once for a line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellPrompt {
    left: String,
    right: String,
    indicator: String,
    vi_normal_indicator: String,
}

const RESET: &str = "\x1b[0m";

fn paint(color: bool, code: &str, text: &str) -> String {
    if color {
        format!("\x1b[{code}m{text}{RESET}")
    } else {
        text.to_owned()
    }
}

/// Joins the texts of the segments that have something to say, with one space between.
fn join(texts: impl IntoIterator<Item = String>) -> String {
    texts.into_iter().collect::<Vec<_>>().join(" ")
}

impl ShellPrompt {
    /// Builds the prompt. `now` is the `HH:MM` text of the `time` segment.
    #[must_use]
    pub fn new(look: &Look, state: &PromptState, now: impl Fn() -> String) -> Self {
        let text = |segment: Segment| -> Option<String> {
            let color = look.color;
            Some(match segment {
                Segment::Cwd => paint(
                    color,
                    "36",
                    &segments::cwd(&state.cwd, state.home.as_deref(), look.unicode),
                ),
                Segment::Git => {
                    let head = segments::git_head(&state.cwd)?;
                    paint(color, "35", &format!("({})", head.label()))
                }
                Segment::Venv => {
                    let name =
                        segments::venv(state.virtual_env.as_deref(), state.conda_env.as_deref())?;
                    paint(color, "32", &format!("[{name}]"))
                }
                Segment::Status => paint(
                    color,
                    "31",
                    &segments::status(state.last_status, look.unicode)?,
                ),
                Segment::Duration => paint(color, "33", &segments::duration(state.last_duration?)?),
                Segment::Time => paint(color, "90", &now()),
            })
        };
        let mut left = join(look.left.iter().filter_map(|s| text(*s)));
        if !left.is_empty() {
            left.push(' ');
        }
        let right = join(look.right.iter().filter_map(|s| text(*s)));

        let glyph = if state.root {
            "#"
        } else if look.unicode {
            "❯"
        } else {
            "$"
        };
        let code = if state.last_status == 0 { "32" } else { "31" };
        Self {
            left,
            right,
            // The space is inside the color, so `$ ` stays one piece of text in the output.
            indicator: paint(look.color, code, &format!("{glyph} ")),
            vi_normal_indicator: paint(look.color, code, ": "),
        }
    }

    /// The prompt for the rest of a command that is not finished: only `> `.
    #[must_use]
    pub fn continuation() -> Self {
        Self {
            left: String::new(),
            right: String::new(),
            indicator: "> ".to_owned(),
            vi_normal_indicator: "> ".to_owned(),
        }
    }

    /// The prompt old lines collapse to (`[prompt] transient`): only the indicator.
    #[must_use]
    pub fn transient(&self) -> Self {
        Self {
            left: String::new(),
            right: String::new(),
            ..self.clone()
        }
    }
}

impl Prompt for ShellPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.left)
    }

    fn render_prompt_right(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.right)
    }

    fn render_prompt_indicator(&self, mode: PromptEditMode) -> Cow<'_, str> {
        Cow::Borrowed(match mode {
            PromptEditMode::Vi(PromptViMode::Normal) => &self.vi_normal_indicator,
            _ => &self.indicator,
        })
    }

    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        Cow::Borrowed("> ")
    }

    fn render_prompt_history_search_indicator(&self, search: PromptHistorySearch) -> Cow<'_, str> {
        let failing = match search.status {
            PromptHistorySearchStatus::Passing => "",
            PromptHistorySearchStatus::Failing => "failing ",
        };
        Cow::Owned(format!("({failing}search: {}) ", search.term))
    }

    // The segments carry their own colors, so reedline adds none.
    fn get_prompt_color(&self) -> Color {
        Color::Default
    }

    fn get_prompt_multiline_color(&self) -> Color {
        Color::Default
    }

    fn get_indicator_color(&self) -> Color {
        Color::Default
    }

    fn get_prompt_right_color(&self) -> Color {
        Color::Default
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn look(left: &[Segment], right: &[Segment]) -> Look {
        Look {
            unicode: true,
            color: false,
            left: left.to_vec(),
            right: right.to_vec(),
        }
    }

    fn state() -> PromptState {
        PromptState {
            cwd: PathBuf::from("/home/me/code"),
            home: Some(PathBuf::from("/home/me")),
            virtual_env: None,
            conda_env: None,
            last_status: 0,
            last_duration: None,
            root: false,
        }
    }

    fn build(look: &Look, state: &PromptState) -> ShellPrompt {
        ShellPrompt::new(look, state, || "12:34".to_owned())
    }

    fn indicator(prompt: &ShellPrompt) -> String {
        prompt
            .render_prompt_indicator(PromptEditMode::Emacs)
            .into_owned()
    }

    #[test]
    fn the_left_side_is_the_directory_and_then_the_indicator() {
        let p = build(&look(&[Segment::Cwd], &[]), &state());
        assert_eq!(p.render_prompt_left(), "~/code ");
        assert_eq!(indicator(&p), "❯ ");
        assert_eq!(p.render_prompt_right(), "");
    }

    #[test]
    fn nothing_on_the_left_leaves_only_the_indicator() {
        let p = build(&look(&[], &[]), &state());
        assert_eq!(p.render_prompt_left(), "");
    }

    #[test]
    fn segments_without_anything_to_say_are_skipped() {
        // No git repository here, no venv.
        let mut s = state();
        s.cwd = PathBuf::from("/");
        s.home = None;
        let p = build(&look(&[Segment::Cwd, Segment::Git, Segment::Venv], &[]), &s);
        assert_eq!(p.render_prompt_left(), "/ ");
    }

    #[test]
    fn a_venv_is_shown_in_brackets() {
        let mut s = state();
        s.conda_env = Some("base".into());
        let p = build(&look(&[Segment::Cwd, Segment::Venv], &[]), &s);
        assert_eq!(p.render_prompt_left(), "~/code [base] ");
    }

    #[test]
    fn the_right_side_shows_status_duration_and_time() {
        let mut s = state();
        s.last_status = 2;
        s.last_duration = Some(Duration::from_secs(65));
        let p = build(
            &look(&[], &[Segment::Status, Segment::Duration, Segment::Time]),
            &s,
        );
        assert_eq!(p.render_prompt_right(), "✘ 2 1m05s 12:34");
    }

    #[test]
    fn a_quick_successful_command_leaves_the_right_side_empty() {
        let mut s = state();
        s.last_duration = Some(Duration::from_millis(40));
        let p = build(&look(&[], &[Segment::Status, Segment::Duration]), &s);
        assert_eq!(p.render_prompt_right(), "");
    }

    #[test]
    fn ascii_mode_has_no_unicode() {
        let mut l = look(&[Segment::Cwd], &[Segment::Status]);
        l.unicode = false;
        let mut s = state();
        s.last_status = 1;
        s.cwd = PathBuf::from("/a/b/c/d");
        s.home = None;
        let p = build(&l, &s);
        assert_eq!(p.render_prompt_left(), ".../b/c/d ");
        assert_eq!(p.render_prompt_right(), "x 1");
        assert_eq!(indicator(&p), "$ ");
        assert_eq!(p.render_prompt_multiline_indicator(), "> ");
        assert!(p.render_prompt_left().is_ascii());
    }

    #[test]
    fn root_gets_a_hash() {
        let mut s = state();
        s.root = true;
        assert_eq!(indicator(&build(&look(&[], &[]), &s)), "# ");
    }

    #[test]
    fn colors_are_ansi_and_can_be_turned_off() {
        let mut l = look(&[Segment::Cwd], &[]);
        l.color = true;
        let p = build(&l, &state());
        assert_eq!(p.render_prompt_left(), "\x1b[36m~/code\x1b[0m ");
        assert_eq!(indicator(&p), "\x1b[32m❯ \x1b[0m");
        l.color = false;
        assert!(!build(&l, &state()).render_prompt_left().contains('\x1b'));
    }

    #[test]
    fn the_indicator_turns_red_after_a_failure() {
        let mut l = look(&[], &[]);
        l.color = true;
        let mut s = state();
        s.last_status = 1;
        assert_eq!(indicator(&build(&l, &s)), "\x1b[31m❯ \x1b[0m");
    }

    #[test]
    fn vi_normal_mode_shows_a_colon() {
        let p = build(&look(&[], &[]), &state());
        assert_eq!(
            p.render_prompt_indicator(PromptEditMode::Vi(PromptViMode::Normal)),
            ": "
        );
        assert_eq!(
            p.render_prompt_indicator(PromptEditMode::Vi(PromptViMode::Insert)),
            "❯ "
        );
    }

    #[test]
    fn the_transient_prompt_keeps_only_the_indicator() {
        let p = build(&look(&[Segment::Cwd], &[Segment::Time]), &state()).transient();
        assert_eq!(p.render_prompt_left(), "");
        assert_eq!(p.render_prompt_right(), "");
        assert_eq!(indicator(&p), "❯ ");
    }

    #[test]
    fn the_continuation_prompt_is_a_plain_greater_than() {
        let p = ShellPrompt::continuation();
        assert_eq!(p.render_prompt_left(), "");
        assert_eq!(p.render_prompt_right(), "");
        assert_eq!(indicator(&p), "> ");
    }

    #[test]
    fn the_search_prompt_names_the_term() {
        let p = build(&look(&[], &[]), &state());
        let passing = PromptHistorySearch::new(PromptHistorySearchStatus::Passing, "ls".into());
        let failing = PromptHistorySearch::new(PromptHistorySearchStatus::Failing, "zz".into());
        assert_eq!(
            p.render_prompt_history_search_indicator(passing),
            "(search: ls) "
        );
        assert_eq!(
            p.render_prompt_history_search_indicator(failing),
            "(failing search: zz) "
        );
    }
}
