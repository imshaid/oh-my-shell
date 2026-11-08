//! The prompt: two lines, like fish. The first line is a header: the segments of `[prompt] left`
//! (the directory, the git branch, a virtual environment) on the left and those of
//! `[prompt] right` (a failed status, a long duration, the time) at the right edge. The second
//! line is the symbol, and the line being typed follows it.
//!
//! No color is forced. The directory is bold and the other segments are dim, in whatever colors
//! the terminal uses; the symbol is plain. The one color is ANSI red (color 1, which the user's
//! terminal theme defines) on the symbol after a command that failed. `NO_COLOR` turns that
//! red off and keeps bold and dim, which are not colors.

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
    /// Colors are allowed: the symbol turns red after a failure. Off for `NO_COLOR`. Bold and dim
    /// do not depend on it.
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
/// SGR codes: bold, dim (faint), and the foreground "ANSI color 1".
const BOLD: &str = "1";
const DIM: &str = "2";
const RED: &str = "31";

fn styled(code: &str, text: &str) -> String {
    format!("\x1b[{code}m{text}{RESET}")
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
            Some(match segment {
                Segment::Cwd => styled(
                    BOLD,
                    &segments::cwd(&state.cwd, state.home.as_deref(), look.unicode),
                ),
                Segment::Git => {
                    let head = segments::git_head(&state.cwd)?;
                    styled(DIM, &format!("({})", head.label()))
                }
                Segment::Venv => {
                    let name =
                        segments::venv(state.virtual_env.as_deref(), state.conda_env.as_deref())?;
                    styled(DIM, &format!("[{name}]"))
                }
                Segment::Status => styled(DIM, &segments::status(state.last_status, look.unicode)?),
                Segment::Duration => styled(DIM, &segments::duration(state.last_duration?)?),
                Segment::Time => styled(DIM, &now()),
            })
        };
        // The header ends the first line; with nothing to show there is no first line.
        let mut left = join(look.left.iter().filter_map(|s| text(*s)));
        if !left.is_empty() {
            left.push('\n');
        }
        let right = join(look.right.iter().filter_map(|s| text(*s)));

        let glyph = if state.root {
            "#"
        } else if look.unicode {
            "❯"
        } else {
            "$"
        };
        // Plain, except red after a failure. The space is inside the escape sequence, so
        // `$ ` stays one piece of text in the output.
        let symbol = |text: String| {
            if look.color && state.last_status != 0 {
                styled(RED, &text)
            } else {
                text
            }
        };
        Self {
            left,
            right,
            indicator: symbol(format!("{glyph} ")),
            vi_normal_indicator: symbol(": ".to_owned()),
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

    // The segments carry their own styles, so reedline adds none.
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

    /// The text without `ESC [ ... m` style sequences.
    fn plain(text: &str) -> String {
        let mut out = String::new();
        let mut chars = text.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for c in chars.by_ref() {
                    if c == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    fn left(prompt: &ShellPrompt) -> String {
        plain(&prompt.render_prompt_left())
    }

    fn right(prompt: &ShellPrompt) -> String {
        plain(&prompt.render_prompt_right())
    }

    fn indicator(prompt: &ShellPrompt) -> String {
        prompt
            .render_prompt_indicator(PromptEditMode::Emacs)
            .into_owned()
    }

    #[test]
    fn the_header_is_the_first_line_and_the_symbol_the_second() {
        let p = build(&look(&[Segment::Cwd], &[]), &state());
        // The header ends with a line break; the indicator is what starts the second line.
        assert_eq!(left(&p), "~/code\n");
        assert_eq!(indicator(&p), "❯ ");
        assert_eq!(right(&p), "");
    }

    #[test]
    fn nothing_on_the_left_leaves_one_line_with_only_the_indicator() {
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
        assert_eq!(left(&p), "/\n");
    }

    #[test]
    fn a_venv_is_shown_in_brackets() {
        let mut s = state();
        s.conda_env = Some("base".into());
        let p = build(&look(&[Segment::Cwd, Segment::Venv], &[]), &s);
        assert_eq!(left(&p), "~/code [base]\n");
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
        assert_eq!(right(&p), "✘ 2 1m05s 12:34");
    }

    #[test]
    fn a_quick_successful_command_leaves_the_right_side_empty() {
        let mut s = state();
        s.last_duration = Some(Duration::from_millis(40));
        let p = build(&look(&[], &[Segment::Status, Segment::Duration]), &s);
        assert_eq!(right(&p), "");
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
        assert_eq!(left(&p), ".../b/c/d\n");
        assert_eq!(right(&p), "x 1");
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

    /// True if `text` sets a foreground or background color (30-37, 40-47, 90-97, 100-107 or an
    /// extended 38/48), as opposed to bold, dim, reset or the default color.
    fn has_color(text: &str) -> bool {
        let mut rest = text;
        while let Some(at) = rest.find("\x1b[") {
            rest = &rest[at + 2..];
            let end = rest
                .find(|c: char| !(c.is_ascii_digit() || c == ';'))
                .unwrap_or(0);
            for code in rest[..end].split(';').filter_map(|c| c.parse::<u16>().ok()) {
                if matches!(code, 30..=38 | 40..=48 | 90..=97 | 100..=107) {
                    return true;
                }
            }
        }
        false
    }

    fn everything(p: &ShellPrompt) -> String {
        format!(
            "{}{}{}",
            p.render_prompt_left(),
            p.render_prompt_right(),
            indicator(p)
        )
    }

    #[test]
    fn the_directory_is_bold_and_the_rest_of_the_header_is_dim() {
        let mut s = state();
        s.conda_env = Some("base".into());
        s.last_status = 2;
        s.last_duration = Some(Duration::from_secs(5));
        let p = build(
            &look(
                &[Segment::Cwd, Segment::Venv],
                &[Segment::Status, Segment::Duration, Segment::Time],
            ),
            &s,
        );
        assert_eq!(
            p.render_prompt_left(),
            "\x1b[1m~/code\x1b[0m \x1b[2m[base]\x1b[0m\n"
        );
        assert_eq!(
            p.render_prompt_right(),
            "\x1b[2m✘ 2\x1b[0m \x1b[2m5s\x1b[0m \x1b[2m12:34\x1b[0m"
        );
    }

    #[test]
    fn the_git_branch_is_dim() {
        let dir = std::env::temp_dir().join(format!("omsh-prompt-git-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join(".git/HEAD"), "ref: refs/heads/topic\n").unwrap();
        let mut s = state();
        s.cwd = dir.clone();
        s.home = None;
        let p = build(&look(&[Segment::Git], &[]), &s);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(p.render_prompt_left(), "\x1b[2m(topic)\x1b[0m\n");
    }

    #[test]
    fn the_symbol_is_plain_after_success_and_red_only_after_a_failure() {
        let mut l = look(&[], &[]);
        l.color = true;
        let ok = build(&l, &state());
        assert_eq!(indicator(&ok), "❯ ", "no escape sequence at all");
        let mut s = state();
        s.last_status = 1;
        let failed = build(&l, &s);
        // ANSI color 1: the terminal's own red.
        assert_eq!(indicator(&failed), "\x1b[31m❯ \x1b[0m");
        // Also the vi normal-mode symbol.
        assert_eq!(
            failed.render_prompt_indicator(PromptEditMode::Vi(PromptViMode::Normal)),
            "\x1b[31m: \x1b[0m"
        );
    }

    #[test]
    fn red_is_the_only_color_the_prompt_ever_sets() {
        let mut l = look(
            &[Segment::Cwd, Segment::Git, Segment::Venv],
            &[Segment::Status, Segment::Duration, Segment::Time],
        );
        l.color = true;
        let mut s = state();
        s.conda_env = Some("base".into());
        s.last_duration = Some(Duration::from_secs(9));
        // A success: no color anywhere, only bold and dim.
        let ok = build(&l, &s);
        assert!(!has_color(&everything(&ok)), "{:?}", everything(&ok));
        // A failure: the symbol is red and nothing else is.
        s.last_status = 3;
        let failed = build(&l, &s);
        assert!(has_color(&indicator(&failed)));
        assert!(!has_color(&failed.render_prompt_left()));
        assert!(!has_color(&failed.render_prompt_right()));
    }

    #[test]
    fn no_color_removes_the_red_and_keeps_bold_and_dim() {
        let mut l = look(&[Segment::Cwd], &[Segment::Duration]);
        l.color = false;
        let mut s = state();
        s.last_status = 1;
        s.last_duration = Some(Duration::from_secs(9));
        let p = build(&l, &s);
        assert_eq!(indicator(&p), "❯ ");
        assert!(p.render_prompt_left().contains("\x1b[1m"), "bold stays");
        assert!(p.render_prompt_right().contains("\x1b[2m"), "dim stays");
        assert!(!has_color(&everything(&p)));
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
