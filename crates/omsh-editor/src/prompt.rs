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
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use reedline::{
    Color, Prompt, PromptEditMode, PromptHistorySearch, PromptHistorySearchStatus, PromptViMode,
};

use crate::git_status::Handle;
use crate::segments::{self, Segment};
use crate::toast::{self, ToastHandle};

/// How the prompt is drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Look {
    /// Box and arrow characters from Unicode (`❯`), else ASCII (`$`).
    pub unicode: bool,
    /// Colors are allowed: the symbol turns red after a failure. Off for `NO_COLOR`. Bold and dim
    /// do not depend on it.
    pub color: bool,
    /// The SGR parameters of the symbol after a failure: the theme's `danger` token (`31`, ANSI
    /// red, in `nord`). `None` when the theme gives that token no look.
    pub failure: Option<String>,
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
    /// The shell's values of the variables that `git` is given (see
    /// [`passed_variables`](crate::passed_variables)). Only the git segment reads them.
    pub git_env: Vec<(OsString, OsString)>,
}

/// One part of a header: text that is known, or the git segment, which is read each time the
/// prompt is drawn because the helper thread may have learned more since.
#[derive(Clone, Debug)]
enum Piece {
    Text(String),
    Git,
}

/// The prompt, built once for a line. Everything is fixed except the git segment.
#[derive(Clone, Debug)]
pub struct ShellPrompt {
    left: Vec<Piece>,
    right: Vec<Piece>,
    git: Option<Handle>,
    /// The toast, read each time the prompt is drawn: it can appear and expire while the prompt
    /// is on the screen. Only the main prompt has one.
    toast: Option<ToastHandle>,
    /// Unicode or ASCII bullet for the toast.
    unicode: bool,
    indicator: String,
    vi_normal_indicator: String,
}

const RESET: &str = "\x1b[0m";
/// SGR codes: bold and dim (faint).
const BOLD: &str = "1";
const DIM: &str = "2";

fn styled(code: &str, text: &str) -> String {
    format!("\x1b[{code}m{text}{RESET}")
}

impl ShellPrompt {
    /// Builds the prompt. `now` is the `HH:MM` text of the `time` segment. The git segment shows
    /// nothing until [`with_git`](Self::with_git) says where to read it from.
    #[must_use]
    pub fn new(look: &Look, state: &PromptState, now: impl Fn() -> String) -> Self {
        let piece = |segment: Segment| -> Option<Piece> {
            Some(Piece::Text(match segment {
                Segment::Cwd => styled(
                    BOLD,
                    &segments::cwd(&state.cwd, state.home.as_deref(), look.unicode),
                ),
                Segment::Git => return Some(Piece::Git),
                Segment::Venv => {
                    let name =
                        segments::venv(state.virtual_env.as_deref(), state.conda_env.as_deref())?;
                    styled(DIM, &format!("[{name}]"))
                }
                Segment::Status => styled(DIM, &segments::status(state.last_status, look.unicode)?),
                Segment::Duration => styled(DIM, &segments::duration(state.last_duration?)?),
                Segment::Time => styled(DIM, &now()),
            }))
        };
        let left: Vec<Piece> = look.left.iter().filter_map(|s| piece(*s)).collect();
        let right: Vec<Piece> = look.right.iter().filter_map(|s| piece(*s)).collect();

        let glyph = if state.root {
            "#"
        } else if look.unicode {
            "❯"
        } else {
            "$"
        };
        // Plain, except red after a failure. The space is inside the escape sequence, so
        // `$ ` stays one piece of text in the output.
        let symbol = |text: String| match (&look.failure, look.color && state.last_status != 0) {
            (Some(code), true) => styled(code, &text),
            _ => text,
        };
        Self {
            left,
            right,
            git: None,
            toast: None,
            unicode: look.unicode,
            indicator: symbol(format!("{glyph} ")),
            vi_normal_indicator: symbol(": ".to_owned()),
        }
    }

    /// Where the git segment reads the branch and the marker from.
    #[must_use]
    pub(crate) fn with_git(mut self, git: Handle) -> Self {
        self.git = Some(git);
        self
    }

    /// Where the toast line above the header is read from.
    #[must_use]
    pub fn with_toast(mut self, toast: ToastHandle) -> Self {
        self.toast = Some(toast);
        self
    }

    /// The prompt for the rest of a command that is not finished: only `> `.
    #[must_use]
    pub fn continuation() -> Self {
        Self::only("> ".to_owned(), "> ".to_owned())
    }

    /// A prompt that is only a label, such as `edit> `, for asking about one thing (the
    /// command of a plan step that is being changed).
    #[must_use]
    pub fn labeled(label: &str) -> Self {
        let indicator = format!("{label}> ");
        Self::only(indicator.clone(), indicator)
    }

    fn only(indicator: String, vi_normal_indicator: String) -> Self {
        Self {
            left: Vec::new(),
            right: Vec::new(),
            git: None,
            toast: None,
            unicode: false,
            indicator,
            vi_normal_indicator,
        }
    }

    /// The prompt old lines collapse to (`[prompt] transient`): only the indicator.
    #[must_use]
    pub fn transient(&self) -> Self {
        Self::only(self.indicator.clone(), self.vi_normal_indicator.clone())
    }

    /// The pieces joined with one space; the git segment is left out while it has nothing to say.
    fn join(&self, pieces: &[Piece]) -> String {
        let git = || {
            let snapshot = self.git.as_ref()?.snapshot();
            Some(styled(DIM, &snapshot.text()?))
        };
        pieces
            .iter()
            .filter_map(|piece| match piece {
                Piece::Text(text) => Some(text.clone()),
                Piece::Git => git(),
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

impl Prompt for ShellPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        // The header ends the first line (the right side is drawn on it); with nothing to show
        // there is no first line.
        let mut left = self.join(&self.left);
        if !left.is_empty() {
            left.push('\n');
        }
        // A toast is a line of its own between the header and the symbol, so that the header
        // keeps its place (and the right side with it) while the toast comes and goes.
        if let Some(text) = self.toast.as_ref().and_then(ToastHandle::current) {
            left.push_str(&toast::line(&text, self.unicode));
            left.push('\n');
        }
        Cow::Owned(left)
    }

    fn render_prompt_right(&self) -> Cow<'_, str> {
        Cow::Owned(self.join(&self.right))
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
            failure: Some("31".to_owned()),
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
            git_env: Vec::new(),
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

    fn toast_handle() -> ToastHandle {
        ToastHandle::with_lifetime(
            std::sync::Arc::new(|| {}),
            std::time::Duration::from_secs(60),
        )
    }

    #[test]
    fn a_toast_is_a_line_between_the_header_and_the_symbol() {
        let toast = toast_handle();
        let p = build(&look(&[Segment::Cwd], &[]), &state()).with_toast(toast.clone());
        assert_eq!(left(&p), "~/code\n", "no toast yet");
        toast.post("2 settings change at the next start");
        assert_eq!(left(&p), "~/code\n• 2 settings change at the next start\n");
        toast.clear();
        assert_eq!(left(&p), "~/code\n", "gone once cleared");
    }

    #[test]
    fn a_toast_without_a_header_still_gets_its_own_line() {
        let toast = toast_handle();
        let p = build(&look(&[], &[]), &state()).with_toast(toast.clone());
        toast.post("hello");
        assert_eq!(left(&p), "• hello\n");
        toast.clear();
        assert_eq!(left(&p), "");
    }

    #[test]
    fn the_toast_bullet_is_ascii_without_unicode() {
        let toast = toast_handle();
        let mut l = look(&[Segment::Cwd], &[]);
        l.unicode = false;
        let p = build(&l, &state()).with_toast(toast.clone());
        toast.post("hello");
        assert_eq!(left(&p), "~/code\n* hello\n");
    }

    #[test]
    fn the_continuation_the_label_and_the_transient_prompt_never_show_a_toast() {
        let toast = toast_handle();
        toast.post("hello");
        let main = build(&look(&[Segment::Cwd], &[]), &state()).with_toast(toast);
        assert_eq!(main.transient().render_prompt_left(), "");
        assert_eq!(ShellPrompt::continuation().render_prompt_left(), "");
        assert_eq!(ShellPrompt::labeled("edit").render_prompt_left(), "");
    }

    use crate::git_status::{Dirtiness, HeadState, Snapshot};
    use crate::segments::Head;

    fn with_git(head: HeadState, dirt: Dirtiness, left: &[Segment]) -> ShellPrompt {
        build(&look(left, &[]), &state()).with_git(Handle::fixed(Snapshot { head, dirt }))
    }

    fn on(branch: &str) -> HeadState {
        HeadState::Known(Head::Branch(branch.into()))
    }

    #[test]
    fn the_git_branch_is_dim() {
        let p = with_git(on("topic"), Dirtiness::Clean, &[Segment::Git]);
        assert_eq!(p.render_prompt_left(), "\x1b[2m(topic)\x1b[0m\n");
    }

    #[test]
    fn a_dirty_marker_follows_the_branch_only_for_a_known_change() {
        let shown = |dirt| with_git(on("dev"), dirt, &[Segment::Cwd, Segment::Git]);
        let text = |p: &ShellPrompt| plain(&p.render_prompt_left());
        assert_eq!(text(&shown(Dirtiness::Dirty)), "~/code (dev*)\n");
        assert_eq!(text(&shown(Dirtiness::Clean)), "~/code (dev)\n");
        // Not known yet, or not known at all: the branch alone.
        assert_eq!(text(&shown(Dirtiness::Pending)), "~/code (dev)\n");
        assert_eq!(text(&shown(Dirtiness::Unknown)), "~/code (dev)\n");
    }

    #[test]
    fn the_git_segment_shows_nothing_until_it_knows_and_never_leaves_a_gap() {
        for head in [HeadState::Pending, HeadState::Absent] {
            let p = with_git(
                head,
                Dirtiness::Dirty,
                &[Segment::Cwd, Segment::Git, Segment::Venv],
            );
            assert_eq!(plain(&p.render_prompt_left()), "~/code\n");
        }
        // Without a source at all (the continuation and label prompts, a shell without a
        // repository watch) it is the same.
        let p = build(&look(&[Segment::Git], &[]), &state());
        assert_eq!(
            p.render_prompt_left(),
            "",
            "nothing, so no header line either"
        );
    }

    #[test]
    fn the_git_segment_keeps_its_place_between_the_others() {
        let mut s = state();
        s.conda_env = Some("base".into());
        let p = build(&look(&[Segment::Cwd, Segment::Git, Segment::Venv], &[]), &s).with_git(
            Handle::fixed(Snapshot {
                head: on("main"),
                dirt: Dirtiness::Dirty,
            }),
        );
        assert_eq!(plain(&p.render_prompt_left()), "~/code (main*) [base]\n");
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
    fn the_failure_symbol_follows_the_themes_danger_token() {
        let mut l = look(&[], &[]);
        l.color = true;
        let mut s = state();
        s.last_status = 2;
        l.failure = Some("35".to_owned());
        assert_eq!(indicator(&build(&l, &s)), "\x1b[35m❯ \x1b[0m");
        // A token with several parts keeps them all.
        l.failure = Some("2;91".to_owned());
        assert_eq!(indicator(&build(&l, &s)), "\x1b[2;91m❯ \x1b[0m");
        // A theme that gives `danger` no look leaves the symbol plain.
        l.failure = None;
        assert_eq!(indicator(&build(&l, &s)), "❯ ");
        // And `NO_COLOR` still wins over any theme.
        l.failure = Some("31".to_owned());
        l.color = false;
        assert_eq!(indicator(&build(&l, &s)), "❯ ");
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
    fn the_transient_prompt_has_no_git_part() {
        let p = with_git(on("dev"), Dirtiness::Dirty, &[Segment::Git]).transient();
        assert_eq!(p.render_prompt_left(), "");
    }

    #[test]
    fn the_continuation_prompt_is_a_plain_greater_than() {
        let p = ShellPrompt::continuation();
        assert_eq!(p.render_prompt_left(), "");
        assert_eq!(p.render_prompt_right(), "");
        assert_eq!(indicator(&p), "> ");
    }

    #[test]
    fn a_labeled_prompt_is_the_label_and_a_greater_than() {
        let p = ShellPrompt::labeled("edit");
        assert_eq!(p.render_prompt_left(), "");
        assert_eq!(indicator(&p), "edit> ");
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
