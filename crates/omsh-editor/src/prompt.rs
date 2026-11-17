//! The prompt: two lines, like fish. The first line is a header: the segments of `[prompt] left`
//! (the directory, the git branch, a virtual environment) on the left and those of
//! `[prompt] right` (a failed status, a long duration, the time) at the right edge. The second
//! line is the symbol, and the line being typed follows it.
//!
//! Two layouts (`ui.style`). **Plain** forces no color: the directory is bold and the other
//! segments are dim, in whatever colors the terminal uses; the symbol is plain; the one color is
//! ANSI red on the symbol after a command that failed. **Spaced** (the default) draws the
//! pieces from the theme's semantic tones: the directory blue, the branch and the time dim, and
//! the status and the duration merged into one chip (`✓ 4 ms` green, `✘ 127` red) at the right;
//! while the line being typed is a request to the AI, the tag `ask AI` (magenta) comes before
//! it. The rule between commands is not part of the prompt: the editor prints it once, above.
//! `NO_COLOR` turns every color off and keeps bold and dim, which are not colors.

use std::borrow::Cow;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use omsh_config::UiStyle;
use omsh_ui::caps::Caps;
use omsh_ui::shell_look;
use omsh_ui::text::{Line, Tone, render_line};
use omsh_ui::theme::Theme;
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
    /// The layout of the prompt (`ui.style`).
    pub style: UiStyle,
    /// What the terminal takes, for the spaced layout's pieces.
    pub caps: Caps,
    /// The theme the spaced layout takes its tones from.
    pub theme: Theme,
    /// The AI can be asked (it is not switched off): only then the `ask AI` tag shows.
    pub ai: bool,
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
    /// A command ran since the last prompt was drawn: the spaced layout prints a rule above
    /// this one. False for the first prompt, and after Ctrl+C.
    pub separate: bool,
    /// The shell runs as root: the indicator is `#`.
    pub root: bool,
    /// The last command failed in a way the AI can be asked about, and it can be asked: the
    /// spaced layout shows `Alt+? explain` before the status chip. Decided by the shell once for
    /// each prompt from what it already knows; the prompt does no work for it.
    pub explain_hint: bool,
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
    /// The spaced layout draws the git segment from a tone at each paint, so it keeps what it
    /// needs. `None` for the plain layout.
    tones: Option<Tones>,
    /// The `ask AI` tag, already written, and the flag the highlighter raises while the line
    /// being typed is a request to the AI. Read at each paint.
    ai_tag: Option<(String, Arc<AtomicBool>)>,
}

/// The theme and the terminal's abilities, to turn a tone into escape sequences.
#[derive(Clone, Debug)]
struct Tones {
    theme: Theme,
    caps: Caps,
}

impl Tones {
    fn write(&self, tone: Tone, text: &str) -> String {
        render_line(&Line::new().tone(tone, text), &self.theme, &self.caps)
    }

    fn line(&self, line: &Line) -> String {
        render_line(line, &self.theme, &self.caps)
    }
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
        let tones = (look.style == UiStyle::Spaced).then(|| Tones {
            theme: look.theme.clone(),
            caps: look.caps,
        });
        // One segment as text; `None` when it has nothing to say.
        let text_of = |segment: Segment| -> Option<String> {
            let dim = |text: &str| match &tones {
                Some(tones) => tones.write(Tone::Dim, text),
                None => styled(DIM, text),
            };
            Some(match segment {
                Segment::Cwd => {
                    let shown = segments::cwd(&state.cwd, state.home.as_deref(), look.unicode);
                    match &tones {
                        Some(tones) => tones.write(Tone::Info, &shown),
                        None => styled(BOLD, &shown),
                    }
                }
                Segment::Git => return None,
                Segment::Venv => {
                    let name =
                        segments::venv(state.virtual_env.as_deref(), state.conda_env.as_deref())?;
                    dim(&format!("[{name}]"))
                }
                Segment::Status => dim(&segments::status(state.last_status, look.unicode)?),
                Segment::Duration => dim(&segments::duration(state.last_duration?)?),
                Segment::Time => dim(&now()),
            })
        };
        // A list of segments as pieces. In the spaced layout the status and the duration are one
        // chip, put where the first of them stands.
        let pieces = |list: &[Segment]| -> Vec<Piece> {
            let mut out = Vec::new();
            let mut chip_done = false;
            for &segment in list {
                if segment == Segment::Git {
                    out.push(Piece::Git);
                    continue;
                }
                let text = match &tones {
                    Some(tones) if matches!(segment, Segment::Status | Segment::Duration) => {
                        if std::mem::replace(&mut chip_done, true) {
                            continue;
                        }
                        let chip =
                            shell_look::chip(state.last_status, state.last_duration, look.unicode)
                                .map(|chip| tones.line(&chip));
                        // The hint comes first, so the result stays at the edge. Only after a
                        // failure: a chip is always there then (there is a status to show).
                        match chip {
                            Some(chip) if state.explain_hint && state.last_status != 0 => Some(
                                format!("{} {chip}", tones.line(&shell_look::explain_hint())),
                            ),
                            other => other,
                        }
                    }
                    _ => text_of(segment),
                };
                if let Some(text) = text {
                    out.push(Piece::Text(text));
                }
            }
            out
        };
        let left = pieces(&look.left);
        let right = pieces(&look.right);

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
        let ai_tag = tones
            .as_ref()
            .filter(|_| look.ai)
            .map(|tones| tones.line(&shell_look::ai_tag()));
        Self {
            left,
            right,
            git: None,
            toast: None,
            unicode: look.unicode,
            indicator: symbol(format!("{glyph} ")),
            vi_normal_indicator: symbol(": ".to_owned()),
            tones,
            // The flag is attached by `with_ai_flag`; until then the tag never shows.
            ai_tag: ai_tag.map(|text| (text, Arc::new(AtomicBool::new(false)))),
        }
    }

    /// Where the `ask AI` tag learns that the line being typed is a request: a flag the
    /// highlighter sets at every key, before the prompt is drawn.
    #[must_use]
    pub fn with_ai_flag(mut self, flag: Arc<AtomicBool>) -> Self {
        if let Some((_, own)) = &mut self.ai_tag {
            *own = flag;
        }
        self
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
            tones: None,
            ai_tag: None,
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
            let text = snapshot.text()?;
            Some(match &self.tones {
                Some(tones) => tones.write(Tone::Dim, &text),
                None => styled(DIM, &text),
            })
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
        let right = self.join(&self.right);
        // The tag comes first, so the status stays at the edge. Read at every paint.
        match &self.ai_tag {
            Some((tag, asked)) if asked.load(Ordering::Relaxed) => {
                Cow::Owned(if right.is_empty() {
                    tag.clone()
                } else {
                    format!("{tag} {right}")
                })
            }
            _ => Cow::Owned(right),
        }
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

    /// A look of the plain layout (the prompt of version 1.0), which most of these tests are
    /// about.
    fn look(left: &[Segment], right: &[Segment]) -> Look {
        Look {
            unicode: true,
            color: false,
            failure: Some("31".to_owned()),
            left: left.to_vec(),
            right: right.to_vec(),
            style: UiStyle::Plain,
            caps: terminal(true, false),
            theme: Theme::default(),
            ai: true,
        }
    }

    /// A terminal that takes escapes.
    fn terminal(unicode: bool, no_color: bool) -> Caps {
        Caps {
            out_tty: true,
            stderr_tty: true,
            unicode,
            color: !no_color,
            no_color,
            ..Caps::plain()
        }
    }

    /// The spaced layout, in color.
    fn spaced(left: &[Segment], right: &[Segment]) -> Look {
        Look {
            style: UiStyle::Spaced,
            color: true,
            caps: terminal(true, false),
            ..look(left, right)
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
            separate: false,
            root: false,
            explain_hint: false,
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

    // ----- the spaced layout -------------------------------------------------------------

    fn ran(status: u8, took: Duration) -> PromptState {
        PromptState {
            last_status: status,
            last_duration: Some(took),
            separate: true,
            ..state()
        }
    }

    const DEFAULT_RIGHT: [Segment; 2] = [Segment::Status, Segment::Duration];

    #[test]
    fn spaced_draws_the_directory_in_blue_and_the_branch_dim() {
        let p = build(&spaced(&[Segment::Cwd], &[]), &state());
        // Tone `info` is blue (34); not bold.
        assert_eq!(p.render_prompt_left(), "\x1b[34m~/code\x1b[0m\n");
        let p = with_git_in(&spaced(&[Segment::Cwd, Segment::Git], &[]));
        assert_eq!(
            p.render_prompt_left(),
            "\x1b[34m~/code\x1b[0m \x1b[2;90m(topic)\x1b[0m\n"
        );
    }

    fn with_git_in(look: &Look) -> ShellPrompt {
        build(look, &state()).with_git(Handle::fixed(Snapshot {
            head: on("topic"),
            dirt: Dirtiness::Clean,
        }))
    }

    #[test]
    fn a_success_is_a_green_chip_with_its_time_at_the_right() {
        let p = build(
            &spaced(&[Segment::Cwd], &DEFAULT_RIGHT),
            &ran(0, Duration::from_millis(4)),
        );
        assert_eq!(p.render_prompt_right(), "\x1b[32m✓ 4 ms\x1b[0m");
    }

    #[test]
    fn a_failure_is_a_red_chip_with_the_status() {
        let p = build(
            &spaced(&[Segment::Cwd], &DEFAULT_RIGHT),
            &ran(127, Duration::from_millis(3)),
        );
        assert_eq!(p.render_prompt_right(), "\x1b[31m✘ 127\x1b[0m");
    }

    #[test]
    fn status_and_duration_make_one_chip_not_two() {
        let p = build(
            &spaced(&[], &[Segment::Duration, Segment::Time, Segment::Status]),
            &ran(2, Duration::from_secs(9)),
        );
        assert_eq!(
            right(&p),
            "✘ 2 · 9.0 s 12:34",
            "one chip where the first stood, then the time"
        );
    }

    #[test]
    fn there_is_no_chip_before_a_command_has_run() {
        let p = build(&spaced(&[Segment::Cwd], &DEFAULT_RIGHT), &state());
        assert_eq!(p.render_prompt_right(), "");
    }

    #[test]
    fn the_chip_is_only_where_the_configuration_puts_it() {
        let p = build(
            &spaced(&[Segment::Cwd], &[Segment::Time]),
            &ran(1, Duration::from_millis(3)),
        );
        assert_eq!(right(&p), "12:34", "no status segment, no chip");
    }

    #[test]
    fn the_ask_ai_tag_follows_the_flag_and_comes_first() {
        let flag = Arc::new(AtomicBool::new(false));
        let p = build(
            &spaced(&[Segment::Cwd], &DEFAULT_RIGHT),
            &ran(0, Duration::from_millis(4)),
        )
        .with_ai_flag(Arc::clone(&flag));
        assert_eq!(right(&p), "✓ 4 ms");
        flag.store(true, Ordering::Relaxed);
        assert_eq!(right(&p), "ask AI ✓ 4 ms");
        assert!(
            p.render_prompt_right().starts_with("\x1b[35mask AI\x1b[0m"),
            "magenta"
        );
        flag.store(false, Ordering::Relaxed);
        assert_eq!(right(&p), "✓ 4 ms", "and gone again");
    }

    fn failed_with_hint(hint: bool, status: u8) -> PromptState {
        PromptState {
            explain_hint: hint,
            ..ran(status, Duration::from_millis(3))
        }
    }

    #[test]
    fn a_failure_shows_the_explain_hint_dim_before_the_chip_when_the_shell_says_so() {
        let look = spaced(&[Segment::Cwd], &DEFAULT_RIGHT);
        let p = build(&look, &failed_with_hint(true, 127));
        assert_eq!(right(&p), "Alt+? explain ✘ 127");
        let raw = p.render_prompt_right();
        assert!(raw.starts_with("\x1b[2"), "dim: {raw:?}");
        assert!(
            raw.ends_with("\x1b[31m✘ 127\x1b[0m"),
            "the result stays at the edge: {raw:?}"
        );
        // Words, not only a color: with NO_COLOR the hint is still there.
        let mut plain = spaced(&[Segment::Cwd], &DEFAULT_RIGHT);
        plain.caps = terminal(true, true);
        assert!(right(&build(&plain, &failed_with_hint(true, 1))).starts_with("Alt+? explain "));
    }

    #[test]
    fn the_explain_hint_is_not_shown_without_the_say_so_after_a_success_or_in_the_plain_layout() {
        let look = spaced(&[Segment::Cwd], &DEFAULT_RIGHT);
        assert_eq!(right(&build(&look, &failed_with_hint(false, 127))), "✘ 127");
        let ok = PromptState {
            explain_hint: true,
            ..ran(0, Duration::from_millis(4))
        };
        assert_eq!(right(&build(&look, &ok)), "✓ 4 ms");
        assert_eq!(
            right(&build(
                &look,
                &PromptState {
                    explain_hint: true,
                    ..state()
                }
            )),
            ""
        );
        let plain = self::look(&[Segment::Cwd], &[Segment::Status]);
        assert!(!right(&build(&plain, &failed_with_hint(true, 127))).contains("Alt+?"));
    }

    #[test]
    fn the_explain_hint_is_ascii_in_the_ascii_look() {
        let mut look = spaced(&[Segment::Cwd], &DEFAULT_RIGHT);
        look.unicode = false;
        look.caps = terminal(false, false);
        let p = build(&look, &failed_with_hint(true, 127));
        assert_eq!(right(&p), "Alt+? explain x 127");
        assert!(p.render_prompt_right().is_ascii());
    }

    #[test]
    fn the_tag_stands_alone_when_there_is_no_chip() {
        let flag = Arc::new(AtomicBool::new(true));
        let p = build(&spaced(&[Segment::Cwd], &DEFAULT_RIGHT), &state()).with_ai_flag(flag);
        assert_eq!(right(&p), "ask AI");
    }

    #[test]
    fn the_tag_is_not_offered_when_the_ai_is_switched_off() {
        let mut l = spaced(&[Segment::Cwd], &DEFAULT_RIGHT);
        l.ai = false;
        let flag = Arc::new(AtomicBool::new(true));
        let p = build(&l, &state()).with_ai_flag(flag);
        assert_eq!(right(&p), "");
    }

    #[test]
    fn the_plain_layout_never_shows_the_tag_or_a_success_chip() {
        let flag = Arc::new(AtomicBool::new(true));
        let p = build(
            &look(&[Segment::Cwd], &DEFAULT_RIGHT),
            &ran(0, Duration::from_millis(4)),
        )
        .with_ai_flag(flag);
        assert_eq!(right(&p), "");
    }

    #[test]
    fn spaced_without_color_keeps_dim_and_drops_the_colors() {
        let mut l = spaced(&[Segment::Cwd, Segment::Git], &DEFAULT_RIGHT);
        l.color = false;
        l.caps = terminal(true, true);
        let p = build(&l, &ran(1, Duration::from_millis(3)))
            .with_git(Handle::fixed(Snapshot {
                head: on("topic"),
                dirt: Dirtiness::Clean,
            }))
            .with_ai_flag(Arc::new(AtomicBool::new(true)));
        let all = format!("{}{}", p.render_prompt_left(), p.render_prompt_right());
        assert!(!has_color(&all), "{all:?}");
        assert!(all.contains("\x1b[2m(topic)"), "dim stays: {all:?}");
        assert!(
            right(&p).starts_with("ask AI ✘ 1"),
            "the words carry the meaning: {:?}",
            right(&p)
        );
    }

    #[test]
    fn spaced_in_ascii_has_no_unicode_anywhere() {
        let mut l = spaced(&[Segment::Cwd], &DEFAULT_RIGHT);
        l.unicode = false;
        l.caps = terminal(false, false);
        let ok = build(&l, &ran(0, Duration::from_millis(4)));
        assert_eq!(right(&ok), "ok 4 ms");
        let failed = build(&l, &ran(3, Duration::from_millis(4)));
        assert_eq!(right(&failed), "x 3");
        assert!(failed.render_prompt_right().is_ascii());
        assert!(failed.render_prompt_left().is_ascii());
    }

    #[test]
    fn spaced_everything_is_from_the_16_color_palette() {
        let mut l = spaced(
            &[Segment::Cwd, Segment::Git, Segment::Venv],
            &[Segment::Status, Segment::Time],
        );
        l.failure = Some("31".to_owned());
        let mut s = ran(1, Duration::from_secs(5));
        s.conda_env = Some("base".into());
        let p = build(&l, &s)
            .with_git(Handle::fixed(Snapshot {
                head: on("main"),
                dirt: Dirtiness::Dirty,
            }))
            .with_ai_flag(Arc::new(AtomicBool::new(true)));
        let all = everything(&p);
        // Every parameter of every sequence is reset, dim, or a color 30 to 37 or 90 to 97.
        let mut rest = all.as_str();
        while let Some(at) = rest.find("\x1b[") {
            rest = &rest[at + 2..];
            let end = rest.find('m').unwrap();
            for code in rest[..end].split(';') {
                let n: u16 = code.parse().unwrap();
                assert!(
                    matches!(n, 0 | 2 | 4 | 7 | 30..=37 | 90..=97),
                    "{n} in {all:?}"
                );
            }
        }
    }

    #[test]
    fn a_bangla_directory_stays_whole_beside_the_chip() {
        let mut s = ran(1, Duration::from_millis(40));
        s.cwd = PathBuf::from("/home/me/প্রকল্প");
        let p = build(&spaced(&[Segment::Cwd], &DEFAULT_RIGHT), &s);
        assert_eq!(left(&p), "~/প্রকল্প\n");
        assert_eq!(right(&p), "✘ 1");
    }

    #[test]
    fn the_transient_prompt_of_the_spaced_layout_is_still_only_the_symbol() {
        let p = build(
            &spaced(&[Segment::Cwd], &DEFAULT_RIGHT),
            &ran(0, Duration::from_millis(4)),
        )
        .with_ai_flag(Arc::new(AtomicBool::new(true)))
        .transient();
        assert_eq!(p.render_prompt_left(), "");
        assert_eq!(p.render_prompt_right(), "");
        assert_eq!(indicator(&p), "❯ ");
    }
}
