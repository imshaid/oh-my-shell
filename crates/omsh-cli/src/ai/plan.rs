//! `? <request>`: ask a model for a plan, show it, and run it after a key press (S21, §13).
//!
//! The flow, in order:
//!
//! 1. The request is masked (secrets, §14.4) and sent with a little context (OS, working
//!    directory, git branch, the last commands of this shell; each can be turned off, §13.5)
//!    to the provider, which answers with a JSON plan (§13.4). `[v]` on the panel shows exactly
//!    what was sent.
//! 2. The answer must parse, fit the schema, and every command must parse in *our* shell
//!    (no `[[ ]]`, arrays, ...). If not, the model gets the error once and tries again; after
//!    that: "couldn't make a plan".
//! 3. Every step goes through the same safety analyzer as a typed line. The level shown is the
//!    higher of the analyzer's and the model's own estimate (§13.1: our level wins if higher).
//! 4. The plan panel is shown and **nothing runs until a key is pressed on it**, whatever the
//!    risk: Enter runs, `e` edits a command, Esc cancels. A plan with a critical step also asks
//!    for `yes` or the target typed (§12.1).
//! 5. The steps run one by one through the normal executor. Each step is prepared and analyzed
//!    again just before it runs, because an earlier step (`cd`) can change what it touches; one
//!    that has become riskier than the panel showed is asked about again. A step that fails
//!    stops the plan.
//!
//! The model never runs anything itself, and no code path below step 4 is reached without the
//! key press.

use std::cell::RefCell;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use omsh_ai::{
    ChatRequest, Conversation, ErrorKind, Gathered, GitState, Limit, ModelRisk, Plan, PlanContext,
    PlanError, PlanRequest, Policy, Provider, ProviderError, Role, SOFT_TURN_LIMIT, Settings,
    alongside, collect_text_counting, follow_up_hides_secret, hides_secret, plan_request,
    retry_request, runtime,
};
use omsh_config::{AiContext, ConfirmLevel};
use omsh_editor::Head;
use omsh_exec::{
    ExitStatus, Io, JobState, Key, LineResult, Outcome, ReadError, Shell, TRASH, Terminal,
    name_list,
};
use omsh_safety::blast::Radius;
use omsh_safety::journal::Origin;
use omsh_safety::{Analysis, Finding, Level};
use omsh_syntax::{HighlightKind, lex_for_highlight};
use omsh_ui::panels::MenuUi;
use omsh_ui::panels::plan_run::{Progress, RowState, RunRow};
use omsh_ui::{Paint, Token};

use super::recent::Recent;
use super::usage::{Calls, Record};
use super::wait::WaitLine;
use crate::confirm::{self, Decision, StartDir, Trash};
use crate::history::History;
use crate::radius::{self, BlastLimits};
use crate::undo::Undo;
use crate::undo::watch::Watch;

/// How many lines of a command, and of an explanation, the panel shows, and how wide.
const SHOWN_LINES: usize = 5;
const SHOWN_WIDTH: usize = 160;
/// Exit status after a request that could not be answered or was given up.
const FAILED: u8 = 1;
const USAGE_STATUS: u8 = 2;
const INTERRUPTED: u8 = 130;
/// Exit status of a job that was stopped (Ctrl+Z): 128 + SIGTSTP.
const STOPPED: u8 = 148;

/// What `?` alone prints.
pub const USAGE: &str =
    "usage: ? <what you want done>   (example: ? show the 5 biggest files here)\n";

const KEYS: &str = "[Enter] run  [e] edit  [c] chat  [v] view context  [Esc] cancel ";

/// [`KEYS`] with each `[key]` in the accent color.
fn keys_line(paint: &Paint) -> String {
    let mut out = String::new();
    let mut rest = KEYS;
    while let Some(start) = rest.find('[') {
        let Some(end) = rest[start..].find(']') else {
            break;
        };
        out.push_str(&rest[..start]);
        out.push_str(&paint.token(Token::Accent, &rest[start..=start + end]));
        rest = &rest[start + end + 1..];
    }
    out.push_str(rest);
    out
}

/// What the line for a follow-up message asks.
const CHAT_PROMPT: &str = "chat> ";

// -- what the REPL hands over ---------------------------------------------------------------

/// Makes a provider for one request. Each request (the first plan, each chat turn) gets a new
/// one that is dropped when the request ends, so no connection stays open while the panel
/// waits for a key.
pub type Connect = Box<dyn FnMut() -> Result<Box<dyn Provider>, ProviderError>>;

/// A provider that can be asked, with the settings that decide what is sent.
pub struct PlanSetup {
    pub connect: Connect,
    pub settings: Settings,
    pub context: AiContext,
    /// The calls of this session, for the info line and the details (UI upgrade, phase 4).
    pub calls: Arc<Calls>,
    /// The panels with buttons, when this shell draws them (for the info line of an answer).
    pub menu: Option<MenuUi>,
}

/// What the person did when asked to change a command.
#[derive(Debug, PartialEq, Eq)]
pub enum Edited {
    /// The new text of the command.
    Text(String),
    /// They gave up (Ctrl+C or Esc in the editor): keep the old command.
    Cancelled,
    /// There is no line editor in this shell (plain input): ask with a plain prompt.
    Unavailable,
}

/// Where terminals come from. A terminal borrows the shell and its files while it lives, and
/// running a step needs them mutably, so the flow asks for a terminal each time it needs one
/// and lets it go again. Tests supply scripted ones.
pub trait TerminalSource {
    fn open<'a>(&'a mut self, shell: &'a Shell, files: &'a Io) -> Box<dyn Terminal + 'a>;
}

/// The shell's own terminal: standard input, output and error.
pub struct RealTerminal;

impl TerminalSource for RealTerminal {
    fn open<'a>(&'a mut self, shell: &'a Shell, files: &'a Io) -> Box<dyn Terminal + 'a> {
        Box::new(shell.terminal(files))
    }
}

pub struct PlanEnv<'a> {
    pub shell: &'a mut Shell,
    pub files: &'a mut Io,
    pub tty: &'a mut dyn TerminalSource,
    pub history: Option<&'a History>,
    /// The last commands of this shell: given to the model as context, and added to by the
    /// steps that run.
    pub recent: &'a mut Recent,
    pub confirm: ConfirmLevel,
    /// The limits of the blast-radius count (§12.3).
    pub blast: BlastLimits,
    /// Colors and glyphs of the panel and the step results (S26).
    pub paint: &'a Paint,
    /// The trash and the undo journal. Without it (`--no-config`) an `rm` deletes for good.
    pub undo: Option<&'a Undo>,
    /// Changes the text of a command in the line editor.
    pub edit: &'a mut dyn FnMut(&str) -> Edited,
    /// What the panels with buttons need. `None`: every question is asked in words, as before.
    pub menu: Option<&'a MenuUi>,
}

// -- masking and context --------------------------------------------------------------------

/// What is known about the shell right now, for the items `allowed` lets through. What is off
/// is not even collected; `PlanContext::build` looks at the switches again and masks.
pub(crate) fn gather(shell: &Shell, allowed: &AiContext, recent: &Recent) -> Gathered {
    let dir = crate::repl::directory(shell);
    Gathered {
        os: if allowed.os { os_name() } else { None },
        git: allowed.git.then(|| git_state(&dir)),
        recent: if allowed.recent_commands {
            recent.items()
        } else {
            Vec::new()
        },
        cwd: allowed.cwd.then_some(dir),
    }
}

fn git_state(dir: &str) -> GitState {
    match omsh_editor::git_head(Path::new(dir)) {
        Some(Head::Branch(name)) => GitState::Branch(name),
        Some(Head::Detached(commit)) => GitState::Detached(commit),
        None => GitState::NotARepository,
    }
}

/// The `PRETTY_NAME` of an `os-release` file, without quotes and control characters.
#[must_use]
pub fn parse_os_release(text: &str) -> Option<String> {
    let value = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("PRETTY_NAME="))?;
    let value = value.trim().trim_matches(['"', '\'']);
    let clean: String = value
        .chars()
        .filter(|c| !c.is_control())
        .take(100)
        .collect();
    let clean = clean.trim().to_owned();
    (!clean.is_empty()).then_some(clean)
}

fn os_name() -> Option<String> {
    let text = std::fs::read_to_string("/etc/os-release")
        .or_else(|_| std::fs::read_to_string("/usr/lib/os-release"))
        .ok()?;
    parse_os_release(&text)
}

// -- a plan whose steps have been looked at -------------------------------------------------

/// A step with the result of preparing and analyzing its command.
#[derive(Clone, Debug)]
struct Reviewed {
    title: String,
    explanation: String,
    needs_root: bool,
    model: ModelRisk,
    /// The command as the plan (or the person, after an edit) has it.
    command: String,
    analysis: Analysis,
    /// Commands of the step that would not start anything in this shell, each with the names its
    /// "did you mean" offers. Only a note in the panel: the step still runs if it is chosen.
    missing: Vec<(String, Vec<String>)>,
    /// Command words that were replaced by the program of a desktop entry (see `desktop`), shown
    /// as `fixed:` lines. The panel shows, and the step runs, the corrected command.
    fixed: Vec<desktop::Fix>,
    /// What the findings of the analysis would touch: one entry per finding, counted when the
    /// plan is shown. Empty until then.
    radii: Vec<Option<Radius>>,
    edited: bool,
}

/// The level shown for a step, and whether it comes from the model's estimate.
struct Rating {
    level: Level,
    from_model: bool,
}

impl Reviewed {
    /// The analyzer's level wins when it is higher (§13.1). An edited command is rated by the
    /// analyzer alone: the model never saw it.
    fn rating(&self) -> Rating {
        let analyzer = self.analysis.level();
        if self.edited {
            return Rating {
                level: analyzer,
                from_model: false,
            };
        }
        let model = match self.model {
            ModelRisk::Low => Level::Safe,
            ModelRisk::Medium => Level::Caution,
            ModelRisk::High => Level::Dangerous,
        };
        Rating {
            level: analyzer.max(model),
            from_model: model > analyzer,
        }
    }
}

/// Prepares and analyzes `command` on a copy of the shell, so that nothing about the real one
/// (its status, its source text) changes. Aliases are not applied: a plan step runs as the model
/// wrote it (`ls -S` is not `eza -S`), and the analysis and the panel see that same text. `start` is where the command starts: for a step of a
/// plan, where the step before it ended. `Err` is a short reason why it cannot run here.
fn review(shell: &Shell, command: &str, start: &StartDir) -> Result<Analysis, String> {
    let mut scratch = shell.clone();
    match scratch.prepare_unaliased(command, true) {
        Ok(prepared) => Ok(confirm::review_at(&scratch, &prepared, start)),
        Err(result) => Err(message_of(&result)),
    }
}

/// The commands of `command` that would not start anything in `shell` (see
/// [`Shell::finds_command`]), each with the names the shell would offer for it, once each in the
/// order they appear. Only plain command names are looked at: a word with a `/`, an expansion
/// (`$cmd`, `$(...)`) or a pattern may depend on where the step starts or on what an earlier step
/// does, so it is left alone. Commands inside `$(...)` and compound commands are found as well.
fn missing_commands(shell: &Shell, command: &str) -> Vec<(String, Vec<String>)> {
    let words = lex_for_highlight(command).words;
    let commands = || {
        words
            .iter()
            .filter(|word| word.kind == HighlightKind::Command)
            .filter(|word| !word.span.text(command).contains('/'))
            .filter_map(|word| {
                let name = crate::notify::plain_name(word.span.text(command))?;
                Some((name, word.span.end))
            })
    };
    // `f() { ...; }; f`: a name followed by `(` is a definition, and the line may use it.
    let defined: Vec<String> = commands()
        .filter(|(_, end)| command[*end..].trim_start().starts_with('('))
        .map(|(name, _)| name)
        .collect();
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for (name, _) in commands() {
        if defined.contains(&name)
            || out.iter().any(|(seen, _)| *seen == name)
            || shell.finds_command(&name)
        {
            continue;
        }
        let near = shell.suggestions(&name);
        out.push((name, near));
    }
    out
}

/// Replaces the commands of `command` that are not found but that the desktop entries of this
/// machine name as exactly one program (`google-chrome` for `google-chrome-stable`). Only the
/// command word changes: arguments, `&`, parentheses and the rest stay as they are. The entries
/// are read only when something is missing, and once: `entries` keeps them for the next step of
/// the same plan. Nothing is sent anywhere. With no entry folders, nothing is read and nothing
/// changes.
fn fix_missing(
    shell: &Shell,
    command: &str,
    entries: &mut Option<Vec<desktop::Entry>>,
) -> (String, Vec<desktop::Fix>) {
    let missing = missing_commands(shell, command);
    if missing.is_empty() {
        return (command.to_owned(), Vec::new());
    }
    let entries = entries.get_or_insert_with(|| desktop::scan(&desktop::dirs_of(shell)));
    let fixes: Vec<desktop::Fix> = missing
        .iter()
        .filter_map(|(name, _)| desktop::find_fix(shell, entries, name))
        .collect();
    if fixes.is_empty() {
        return (command.to_owned(), fixes);
    }
    // The same words `missing_commands` looked at, replaced from the end so that the spans of
    // the earlier ones stay right.
    let mut spans: Vec<(usize, usize, &str)> = lex_for_highlight(command)
        .words
        .iter()
        .filter(|word| word.kind == HighlightKind::Command)
        .filter(|word| !word.span.text(command).contains('/'))
        .filter_map(|word| {
            let name = crate::notify::plain_name(word.span.text(command))?;
            let fix = fixes.iter().find(|fix| fix.from == name)?;
            Some((word.span.start, word.span.end, fix.to.as_str()))
        })
        .collect();
    spans.sort_by_key(|(start, ..)| std::cmp::Reverse(*start));
    let mut fixed = command.to_owned();
    for (start, end, to) in spans {
        fixed.replace_range(start..end, to);
    }
    (fixed, fixes)
}

/// `command` (already reviewed as `analysis`) with its missing commands fixed from the desktop
/// entries, and reviewed again as it now reads. If the corrected text cannot be reviewed (not
/// expected: only a command word changed), the command stays as it was.
fn with_fixes(
    shell: &Shell,
    command: &str,
    analysis: Analysis,
    start: &StartDir,
    entries: &mut Option<Vec<desktop::Entry>>,
) -> (String, Analysis, Vec<desktop::Fix>) {
    let (fixed, fixes) = fix_missing(shell, command, entries);
    if fixes.is_empty() {
        return (command.to_owned(), analysis, fixes);
    }
    match review(shell, &fixed, start) {
        Ok(reviewed) => (fixed, reviewed, fixes),
        Err(_) => (command.to_owned(), analysis, Vec::new()),
    }
}

/// Where step `index` starts: the shell's directory for the first, else where the one before
/// it ended (as far as the analysis could follow its `cd`s).
fn start_of(steps: &[Reviewed], index: usize) -> StartDir {
    match index.checked_sub(1).map(|before| &steps[before].analysis) {
        None => StartDir::Shell,
        Some(analysis) => analysis
            .cwd_after
            .as_ref()
            .map_or(StartDir::Unknown, |dir| StartDir::At(dir.into())),
    }
}

fn message_of(result: &LineResult) -> String {
    match result {
        LineResult::Rejected(report) => report.message().to_owned(),
        LineResult::Incomplete => "the command is unfinished".to_owned(),
        LineResult::Ran(_) => "the command cannot be run".to_owned(),
    }
}

/// Every step of a plan must parse in this shell. Each is analyzed from where the step before
/// it ended, so `cd /` followed by `rm -rf *` is seen for what it is.
fn review_plan(shell: &Shell, plan: &Plan) -> Result<Vec<Reviewed>, PlanError> {
    let mut out: Vec<Reviewed> = Vec::new();
    let mut entries = None;
    for (index, step) in plan.steps.iter().enumerate() {
        let start = start_of(&out, index);
        let analysis = review(shell, &step.command, &start).map_err(|why| {
            PlanError::new(format!(
                "the command of step {} cannot be used here ({why}); use only the supported \
                 POSIX sh syntax",
                index + 1
            ))
        })?;
        let (command, analysis, fixed) =
            with_fixes(shell, &step.command, analysis, &start, &mut entries);
        out.push(Reviewed {
            title: step.title.clone(),
            explanation: step.explanation.clone(),
            needs_root: step.needs_root,
            model: step.risk,
            missing: missing_commands(shell, &command),
            command,
            analysis,
            fixed,
            radii: Vec::new(),
            edited: false,
        });
    }
    Ok(out)
}

/// Counts what the findings of `step` that the person is asked about would touch. Dangerous
/// findings always get a count (the panel is the question for them); a lower level only when
/// `[safety] confirm` is lower.
fn count_step(step: &mut Reviewed, confirm: ConfirmLevel, blast: BlastLimits) {
    let from = confirm::threshold(confirm).min(Level::Dangerous);
    let shown: Vec<&Finding> = step
        .analysis
        .findings
        .iter()
        .filter(|finding| finding.level >= from)
        .collect();
    let mut counted = radius::measure(&shown, blast).into_iter();
    step.radii = step
        .analysis
        .findings
        .iter()
        .map(|finding| {
            if finding.level >= from {
                counted.next().flatten()
            } else {
                None
            }
        })
        .collect();
}

struct Checked {
    summary: String,
    steps: Vec<Reviewed>,
}

// -- asking ---------------------------------------------------------------------------------

pub(crate) enum Failure {
    Provider(ProviderError),
    Invalid(PlanError),
}

/// The request that was sent last (the first one, or the second try), for `[v]`.
#[derive(Clone)]
pub(crate) struct Sent {
    request: ChatRequest,
    /// On a second try: how long the model's refused first answer was.
    first_answer_chars: Option<usize>,
}

/// Asks, checks the answer, and asks once more with the error if it was no good.
async fn rounds<T>(
    provider: &dyn Provider,
    first: &ChatRequest,
    check: &dyn Fn(&str) -> Result<T, PlanError>,
    received: &AtomicUsize,
    tries: &AtomicUsize,
    sent: &RefCell<Option<Sent>>,
) -> Result<(T, String), Failure> {
    let policy = Policy::standard();
    let mut request = first.clone();
    let mut first_answer_chars = None;
    let mut outcome = Err(Failure::Invalid(PlanError::new("there was no answer")));
    for attempt in 1..=2 {
        tries.store(attempt, Ordering::Relaxed);
        *sent.borrow_mut() = Some(Sent {
            request: request.clone(),
            first_answer_chars,
        });
        let answer = match collect_text_counting(provider, &request, &policy, received).await {
            Ok(answer) => answer,
            Err(error) => {
                outcome = Err(Failure::Provider(error));
                break;
            }
        };
        match check(&answer) {
            Ok(value) => {
                outcome = Ok((value, answer));
                break;
            }
            Err(error) if attempt == 1 => {
                first_answer_chars = Some(answer.chars().count());
                request = retry_request(first, &answer, &error);
            }
            Err(error) => outcome = Err(Failure::Invalid(error)),
        }
    }
    outcome
}

/// What a request gave back.
struct Answered<T> {
    result: Result<(T, String), Failure>,
    /// What the server counted for the last answer.
    usage: Option<omsh_ai::Usage>,
    /// The window of the model, if it was asked for and found while the request ran.
    limit: Option<Limit>,
}

/// Runs [`rounds`]. With `probe`, the size of the model's context window is asked for next to the
/// request: the answer is never held up by it, and a lookup that has not finished when the answer
/// is in is dropped. Ends with the provider dropped, inside the runtime (see
/// `omsh_ai::runtime::block_on`).
async fn attempts<T>(
    provider: Box<dyn Provider>,
    first: &ChatRequest,
    check: &dyn Fn(&str) -> Result<T, PlanError>,
    received: &AtomicUsize,
    tries: &AtomicUsize,
    sent: &RefCell<Option<Sent>>,
    probe: bool,
) -> Answered<T> {
    let (result, limit) = {
        let window = async {
            if probe {
                provider.context_limit().await
            } else {
                None
            }
        };
        alongside(
            rounds(provider.as_ref(), first, check, received, tries, sent),
            window,
        )
        .await
    };
    // What the server counted for the last answer, before the provider goes.
    let usage = provider.take_usage();
    drop(provider);
    Answered {
        result,
        usage,
        limit,
    }
}

/// What [`ask`] gives back: the result, what was sent last, and the call, if the provider answered.
pub(crate) type Asked<T> = (Result<(T, String), Failure>, Option<Sent>, Option<Record>);

/// How the numbers of a call are shown after it, and where they are kept.
pub(crate) struct Reporter<'a> {
    pub calls: &'a Calls,
    pub model: &'a str,
    /// Set when the shell may draw panels with buttons; the info line then replaces the plain
    /// `3.2 s, 1,204 tokens in, 310 out`, on a terminal that takes keys.
    pub ui: Option<&'a MenuUi>,
    /// The plan panel carries the info line itself: nothing is printed after the call.
    pub in_panel: bool,
}

/// Runs [`attempts`] and shows the wait: on a terminal one line that is redrawn with the real
/// time and the number of characters that have arrived; otherwise one plain line. The call is
/// recorded when the provider answered.
pub(crate) fn ask<T>(
    provider: Box<dyn Provider>,
    first: &ChatRequest,
    check: &dyn Fn(&str) -> Result<T, PlanError>,
    label: &str,
    paint: &Paint,
    terminal: &mut dyn Terminal,
    reporter: &Reporter<'_>,
) -> Asked<T> {
    let received = AtomicUsize::new(0);
    let tries = AtomicUsize::new(1);
    let sent = RefCell::new(None);
    let kind = provider.kind();
    // The rich look (and the lookup of the window that goes with it) is only for a person at a
    // terminal that takes keys; every other flow keeps the plain line and sends no extra request.
    let rich = reporter.ui.filter(|_| terminal.menu_keys());
    let probe = rich.is_some() && reporter.calls.config_limit().is_none();
    let wait = WaitLine::start(label, paint, terminal);
    let terminal = RefCell::new(terminal);
    let watch = || {
        let mut terminal = terminal.borrow_mut();
        if terminal.interrupted() {
            return true;
        }
        let chars = received.load(Ordering::Relaxed);
        let mut extra = String::new();
        if chars > 0 {
            extra.push_str(&format!(", {chars} characters"));
        }
        if tries.load(Ordering::Relaxed) > 1 {
            extra.push_str(" (second try)");
        }
        wait.tick(&mut **terminal, &extra);
        false
    };
    let outcome = runtime::block_on(
        attempts(provider, first, check, &received, &tries, &sent, probe),
        &watch,
    );
    wait.clear(&mut **terminal.borrow_mut());
    let elapsed = wait.elapsed();
    let answered = match outcome {
        Ok(answered) => answered,
        Err(error) => Answered {
            result: Err(Failure::Provider(error)),
            usage: None,
            limit: None,
        },
    };
    let Answered {
        result,
        usage,
        limit,
    } = answered;
    // The provider answered (even if the plan was no good): the call counts.
    let record = (!matches!(result, Err(Failure::Provider(_)))).then(|| {
        reporter
            .calls
            .record(kind, reporter.model, usage, elapsed, limit)
    });
    let mut terminal = terminal.borrow_mut();
    match (rich, record.as_ref()) {
        (Some(ui), Some(record)) if !reporter.in_panel => {
            terminal.err(&reporter.calls.info_text(ui, record));
        }
        (Some(_), _) => {}
        (None, _) => wait.report(&mut **terminal, usage),
    }
    (result, sent.into_inner(), record)
}

// -- the panel ------------------------------------------------------------------------------

fn shorten(line: &str) -> String {
    if line.chars().count() <= SHOWN_WIDTH {
        return line.to_owned();
    }
    let mut cut: String = line.chars().take(SHOWN_WIDTH).collect();
    cut.push_str("...");
    cut
}

/// `text`, at most [`SHOWN_LINES`] lines, each indented by `indent` (the first by `first`).
fn block(out: &mut String, first: &str, indent: &str, text: &str) {
    let mut lines = text.trim_end().lines();
    for (index, line) in lines.by_ref().take(SHOWN_LINES).enumerate() {
        out.push_str(if index == 0 { first } else { indent });
        out.push_str(&shorten(line));
        out.push('\n');
    }
    if lines.next().is_some() {
        out.push_str(indent);
        out.push_str("...\n");
    }
}

/// Puts the bar of the panel where the first two columns of each indented line are. A look
/// that is not a terminal has blanks for a bar, so its text is left as it is.
fn with_bar(paint: &Paint, text: &str) -> String {
    if !paint.look().terminal {
        return text.to_owned();
    }
    let bar = paint.bar(Token::Ai);
    let mut out = String::with_capacity(text.len() + 16);
    for line in text.split_inclusive('\n') {
        match line.strip_prefix("  ") {
            Some(rest) => {
                out.push_str(&bar);
                out.push_str(rest);
            }
            None => out.push_str(line),
        }
    }
    out
}

fn panel(
    paint: &Paint,
    summary: &str,
    settings: &Settings,
    steps: &[Reviewed],
    blast: BlastLimits,
    journal: bool,
    turns: usize,
) -> String {
    with_bar(
        paint,
        &panel_text(paint, summary, settings, steps, blast, journal, turns),
    )
}

fn panel_text(
    paint: &Paint,
    summary: &str,
    settings: &Settings,
    steps: &[Reviewed],
    blast: BlastLimits,
    journal: bool,
    turns: usize,
) -> String {
    let after = match turns {
        0 => String::new(),
        1 => ", after 1 chat turn".to_owned(),
        n => format!(", after {n} chat turns"),
    };
    let mut out = format!(
        "oh-my-shell: {}:\n  {}\n",
        paint.token(
            Token::Ai,
            &format!(
                "plan from {} ({}{after})",
                settings.kind.label(),
                settings.model
            )
        ),
        shorten(summary)
    );
    if turns >= SOFT_TURN_LIMIT {
        out.push_str(&format!(
            "  (chat turn {turns}: if only a command needs changing, [e] edit may be quicker)\n"
        ));
    }
    for (index, step) in steps.iter().enumerate() {
        let rating = step.rating();
        let note = if step.edited {
            ", edited"
        } else if rating.from_model {
            " (the model's estimate)"
        } else {
            ""
        };
        out.push_str(&format!(
            "\n  {}. {}   {}\n",
            index + 1,
            shorten(&step.title),
            paint.token(
                confirm::level_token(rating.level),
                &format!("[{}{note}]", rating.level.name())
            )
        ));
        step_body(paint, index, step, blast, journal, &mut out);
    }
    out.push('\n');
    out
}

/// The lines under the title of one step: the command, the explanation, what it needs, the
/// reasons, the counts and what `undo` can do. Each line starts with five blanks.
fn step_body(
    paint: &Paint,
    index: usize,
    step: &Reviewed,
    blast: BlastLimits,
    journal: bool,
    out: &mut String,
) {
    block(out, "     $ ", "       ", &step.command);
    if step.edited {
        out.push_str("     (your edit; the model's explanation was for its own command)\n");
    } else if !step.explanation.trim().is_empty() {
        block(out, "     ", "     ", &step.explanation);
    }
    if step.needs_root {
        out.push_str(
            "     needs root: the plan does not add sudo; it may fail until you decide how \
                 to run it\n",
        );
    }
    for fix in &step.fixed {
        out.push_str(&format!(
            "     fixed: {} -> {}",
            shorten(&displayable(&fix.from)),
            shorten(&displayable(&fix.to)),
        ));
        if let Some(app) = &fix.app {
            out.push_str(&format!(" ({})", shorten(&displayable(app))));
        }
        out.push('\n');
    }
    for (name, near) in &step.missing {
        out.push_str(&format!("     not found: {}", shorten(&displayable(name))));
        if !near.is_empty() {
            out.push_str(&format!("; did you mean {}?", name_list(near)));
        }
        out.push('\n');
    }
    let mut counted = false;
    // Whether the plan's `rm`s go through the trash decides what a delete's reason says.
    let trash = if journal
        && !step.analysis.deletes.is_empty()
        && step
            .analysis
            .deletes
            .iter()
            .all(|site| site.trashable.is_ok())
    {
        Trash::Yes
    } else {
        Trash::No
    };
    for (at, finding) in step.analysis.findings.iter().enumerate() {
        out.push_str("     - ");
        out.push_str(&confirm::reason_text(finding, trash));
        out.push('\n');
        if let Some(target) = &finding.target {
            out.push_str("       on: ");
            out.push_str(&paint.token(Token::Path, &shorten(target)));
            out.push('\n');
        }
        if let Some(Some(found)) = step.radii.get(at) {
            counted = true;
            let cwd = finding.files.as_ref().and_then(|f| f.cwd.as_deref());
            for line in radius::lines(found, cwd, blast) {
                out.push_str("       ");
                if line.starts_with("for example:") {
                    out.push_str(&paint.token(Token::Muted, &line));
                } else {
                    out.push_str(&line);
                }
                out.push('\n');
            }
        }
    }
    if counted && index > 0 {
        out.push_str("       (counted now, before the steps above have run)\n");
    }
    undo_lines(paint, out, &step.analysis, journal);
}

/// What `undo` can and cannot do for a step: which of its `rm`s go through the trash, and
/// which `mkdir`, `mv` and redirections are followed.
fn undo_lines(paint: &Paint, out: &mut String, analysis: &Analysis, journal: bool) {
    let mut trashed = false;
    for site in &analysis.deletes {
        match (&site.trashable, journal) {
            (Ok(()), true) => trashed = true,
            (Ok(()), false) => {}
            (Err(why), _) => {
                out.push_str(&format!(
                    "     - {}: {}\n",
                    paint.token(Token::Warning, "deletes for good, not through the trash"),
                    why.text()
                ));
            }
        }
    }
    if !journal && analysis.deletes.iter().any(|site| site.trashable.is_ok()) {
        out.push_str(&format!(
            "     - {}: the trash is not available in this shell\n",
            paint.token(Token::Warning, "deletes for good")
        ));
    }
    if trashed {
        out.push_str(&format!(
            "     {}\n",
            paint.token(
                Token::Success,
                "rm goes to the trash; undo can bring it back"
            )
        ));
    }
    let mut followed = Vec::new();
    for change in &analysis.changes {
        match (&change.skip, journal) {
            (Some(why), _) => {
                out.push_str(&format!("     - not undoable: {} ({why})\n", change.what()))
            }
            (None, true) => match change.targets.iter().find(|target| target.unknown) {
                Some(target) => out.push_str(&format!(
                    "     - not undoable: {} ({} is only known when the step runs)\n",
                    change.what(),
                    target.src
                )),
                None => followed.push(change.what()),
            },
            (None, false) => {}
        }
    }
    followed.sort_unstable();
    followed.dedup();
    if !followed.is_empty() {
        out.push_str(&format!("     undo can reverse: {}\n", followed.join(", ")));
    }
}

// -- what was sent --------------------------------------------------------------------------

/// `text` as it may be shown: no control characters but line feeds.
fn displayable(text: &str) -> String {
    text.chars()
        .filter(|c| *c == '\n' || !c.is_control())
        .collect()
}

/// Exactly what went to the model (`[v]`, §13.5): the instructions with the context lines, and
/// every message of the conversation in order. The model's own earlier plans were sent back too;
/// their text is not printed again (it is the model's, and it may hold escape codes). After a
/// second try, the refused answer was sent back as well, with a message saying what was wrong.
fn sent_view(sent: &Sent, settings: &Settings) -> String {
    let request = &sent.request;
    let mut out = format!(
        "\noh-my-shell: this was sent to {} ({}):\n\ninstructions:\n",
        settings.kind.label(),
        settings.model
    );
    let quote = |out: &mut String, text: &str| {
        for line in displayable(text).lines() {
            out.push_str("  | ");
            out.push_str(line);
            out.push('\n');
        }
    };
    quote(&mut out, request.system.as_deref().unwrap_or_default());
    // On a second try the last two messages are the refused answer and the reason.
    let kept = request
        .messages
        .len()
        .saturating_sub(if sent.first_answer_chars.is_some() {
            2
        } else {
            0
        });
    for (index, message) in request.messages[..kept].iter().enumerate() {
        match (message.role, index) {
            (Role::User, 0) => {
                out.push_str("\nrequest:\n");
                quote(&mut out, &message.text);
            }
            (Role::User, _) => {
                out.push_str("\nyour message:\n");
                quote(&mut out, &message.text);
            }
            (Role::Assistant, _) => out.push_str(&format!(
                "\nthe model's earlier plan ({} characters) was sent back\n",
                message.text.chars().count()
            )),
        }
    }
    if let Some(chars) = sent.first_answer_chars {
        out.push_str(&format!(
            "\nsecond try: the model's first answer ({chars} characters) was sent back, with:\n"
        ));
        quote(
            &mut out,
            request.messages.last().map_or("", |m| m.text.as_str()),
        );
    }
    out.push_str("\nsecrets are replaced by [MASKED] before anything is sent; no file contents or command output are sent\n\n");
    out
}

// -- the keys -------------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
enum Choice {
    Run,
    /// Change the command of this step (0-based).
    Edit(usize),
    /// Send a follow-up message and get a new plan.
    Chat,
    /// Run the plan, asking before each step (only from the panel with buttons).
    StepByStep,
    Cancel(u8),
}

/// Waits for a key on the panel. Only Enter (or `y`) runs; `e` edits; `c` chats; `v` shows what
/// was sent and goes on waiting; Esc, `n` and `q` cancel; any other key is ignored.
fn choose(terminal: &mut dyn Terminal, steps: usize, view: &str, paint: &Paint) -> Choice {
    loop {
        match terminal.read_key(&keys_line(paint)) {
            Ok(Key::Enter | Key::Char('y' | 'Y')) => return Choice::Run,
            Ok(Key::Char('c' | 'C')) => return Choice::Chat,
            Ok(Key::Char('v' | 'V')) => terminal.err(view),
            Ok(Key::Char('e' | 'E')) => {
                if steps == 1 {
                    return Choice::Edit(0);
                }
                let prompt = format!("Edit which step? [1-{steps}] ");
                if let Ok(Key::Char(digit)) = terminal.read_key(&prompt)
                    && let Some(n) = digit.to_digit(10)
                    && (1..=steps as u32).contains(&n)
                {
                    return Choice::Edit(n as usize - 1);
                }
            }
            Ok(Key::Escape | Key::Char('n' | 'N' | 'q' | 'Q')) => return Choice::Cancel(FAILED),
            Ok(Key::Char(_)) => {}
            Err(ReadError::Interrupted) => return Choice::Cancel(INTERRUPTED),
            Err(ReadError::EndOfInput | ReadError::Failed) => return Choice::Cancel(FAILED),
        }
    }
}

// -- the flow -------------------------------------------------------------------------------

fn finish(shell: &mut Shell, status: u8) -> LineResult {
    shell.set_last_status(ExitStatus(status));
    LineResult::Ran(Outcome::Continue(ExitStatus(status)))
}

fn say(env: &mut PlanEnv<'_>, text: &str) {
    env.tty.open(env.shell, env.files).err(text);
}

/// A provider for the next request, or the one line that says why there is none.
pub(crate) fn provider_for(
    connect: &mut Connect,
    kind: omsh_config::Provider,
) -> Result<Box<dyn Provider>, String> {
    connect().map_err(|error| error.one_line(kind))
}

/// Why a request ended without a usable plan, as one line.
fn failure_line(failure: &Failure, kind: omsh_config::Provider) -> String {
    match failure {
        Failure::Provider(error) if error.kind == ErrorKind::Cancelled => "cancelled".to_owned(),
        Failure::Provider(error) => error.one_line(kind),
        Failure::Invalid(error) => format!("couldn't make a plan: {error}"),
    }
}

/// Handles a `? request` line. Returns what the REPL does next, like running a line.
pub fn run(setup: PlanSetup, request: &str, mut env: PlanEnv<'_>) -> LineResult {
    let PlanSetup {
        mut connect,
        settings,
        context: allowed,
        calls,
        ..
    } = setup;
    let kind = settings.kind;
    if request.trim().is_empty() {
        say(&mut env, USAGE);
        return finish(env.shell, USAGE_STATUS);
    }
    let provider = match provider_for(&mut connect, kind) {
        Ok(provider) => provider,
        Err(line) => {
            say(&mut env, &format!("oh-my-shell: ?: {line}\n"));
            return finish(env.shell, FAILED);
        }
    };

    // -- ask
    let context = PlanContext::build(gather(env.shell, &allowed, env.recent), &allowed);
    let plan_asked = PlanRequest {
        request,
        context: &context,
    };
    if hides_secret(&plan_asked) {
        say(
            &mut env,
            "oh-my-shell: a secret in the request or the context was hidden before sending\n",
        );
    }
    let first = plan_request(&plan_asked);
    let label = format!("Asking {} ({})", kind.label(), settings.model);
    let checker = env.shell.clone();
    let check = |answer: &str| check_answer(&checker, answer);
    // Panels with buttons: only with a menu **and** a person at a real terminal.
    let menu: Option<MenuUi> = {
        let terminal = env.tty.open(env.shell, env.files);
        view::can_show(env.menu, &*terminal)
    };
    let reporter = Reporter {
        calls: &calls,
        model: &settings.model,
        ui: env.menu,
        in_panel: menu.is_some(),
    };
    let (outcome, sent, mut record) = {
        let mut terminal = env.tty.open(env.shell, env.files);
        ask(
            provider,
            &first,
            &check,
            &label,
            env.paint,
            &mut *terminal,
            &reporter,
        )
    };
    // What `[v]` shows: the request that was really sent last (a second try, if there was one).
    let mut view = sent_view(
        &sent.unwrap_or(Sent {
            request: first.clone(),
            first_answer_chars: None,
        }),
        &settings,
    );
    let mut conversation = Conversation::new(&first);
    let Checked { summary, mut steps } = match outcome {
        Ok((checked, answer)) => {
            conversation.accept(&first, &answer);
            checked
        }
        Err(Failure::Provider(error)) if error.kind == ErrorKind::Cancelled => {
            say(&mut env, "\noh-my-shell: cancelled; nothing ran\n");
            return finish(env.shell, INTERRUPTED);
        }
        Err(failure) => {
            // The panel would have carried the numbers of the call; with no plan they go here.
            if let (Some(ui), Some(record)) = (&menu, &record) {
                say(&mut env, &calls.info_text(ui, record));
            }
            say(
                &mut env,
                &format!("oh-my-shell: ?: {}\n", failure_line(&failure, kind)),
            );
            return finish(env.shell, FAILED);
        }
    };
    let mut summary = summary;

    // -- count what the destructive steps would touch, then show it and wait for a key
    for step in &mut steps {
        count_step(step, env.confirm, env.blast);
    }
    let mut expanded = false;
    let stepwise = loop {
        let journal = env.undo.is_some() && env.shell.has_journal();
        let shown = menu.as_ref().and_then(|ui| {
            view::plan_choice(
                &mut env,
                ui,
                &calls,
                &settings,
                &summary,
                &steps,
                journal,
                conversation.turns(),
                record.as_ref(),
                &mut expanded,
                &view,
            )
        });
        let choice = match shown {
            Some(choice) => choice,
            None => {
                let text = panel(
                    env.paint,
                    &summary,
                    &settings,
                    &steps,
                    env.blast,
                    journal,
                    conversation.turns(),
                );
                let mut terminal = env.tty.open(env.shell, env.files);
                terminal.err(&text);
                choose(&mut *terminal, steps.len(), &view, env.paint)
            }
        };
        match choice {
            Choice::Run => break false,
            Choice::StepByStep => break true,
            Choice::Cancel(status) => {
                if status == INTERRUPTED {
                    say(&mut env, "\n");
                }
                say(&mut env, "oh-my-shell: cancelled; nothing ran\n");
                return finish(env.shell, status);
            }
            Choice::Edit(index) => edit_step(&mut env, &mut steps, index),
            Choice::Chat => {
                if let Some((new, new_record)) = chat_turn(
                    &mut env,
                    &mut connect,
                    &settings,
                    &calls,
                    &mut conversation,
                    &steps,
                    &mut view,
                ) {
                    summary = new.summary;
                    steps = new.steps;
                    record = new_record.or(record);
                }
            }
        }
    };

    // -- a critical step also needs the target typed
    let critical: Vec<String> = steps
        .iter()
        .flat_map(|step| step.analysis.at_least(Level::Critical))
        .filter_map(|finding| finding.target.clone())
        .collect();
    let top = steps
        .iter()
        .map(|step| step.rating().level)
        .max()
        .unwrap_or(Level::Safe);
    // Step by step asks at each step, with the step in front of the person; the one question for
    // the whole plan is for a plan that runs all at once.
    if stepwise {
        // Nothing to ask now.
    } else if steps
        .iter()
        .any(|step| step.analysis.level() == Level::Critical)
    {
        let targets: Vec<&str> = critical.iter().map(String::as_str).collect();
        let decision = {
            let mut terminal = env.tty.open(env.shell, env.files);
            confirm::ask_plan(
                Level::Critical,
                &targets,
                env.paint,
                menu.as_ref(),
                &mut *terminal,
            )
        };
        if let Decision::Cancel(code) = decision {
            return finish(env.shell, code);
        }
    } else if top >= confirm::threshold(env.confirm) {
        // Enter on the panel showed the plan; a step at the level that asks for a typed line
        // asks once more here, with the counts above, as `[safety] confirm` says.
        let decision = {
            let mut terminal = env.tty.open(env.shell, env.files);
            confirm::ask_plan(top, &[], env.paint, menu.as_ref(), &mut *terminal)
        };
        if let Decision::Cancel(code) = decision {
            return finish(env.shell, code);
        }
    }

    execute(
        &mut env,
        &steps,
        Mode {
            menu: menu.as_ref(),
            stepwise,
        },
    )
}

/// Reads a model's answer as a plan whose every command parses in this shell.
fn check_answer(shell: &Shell, answer: &str) -> Result<Checked, PlanError> {
    let plan = Plan::parse(answer)?;
    let steps = review_plan(shell, &plan)?;
    Ok(Checked {
        summary: plan.summary,
        steps,
    })
}

/// One round of `[c] chat`: reads a message, asks the model for a new plan with the whole
/// conversation, and returns the new plan (counted, ready to show). Anything that goes wrong
/// (nothing typed, Ctrl+C, no answer, a plan that is no good) says so in one line and returns
/// `None`: the plan on the screen stays as it was.
fn chat_turn(
    env: &mut PlanEnv<'_>,
    connect: &mut Connect,
    settings: &Settings,
    calls: &Calls,
    conversation: &mut Conversation,
    steps: &[Reviewed],
    view: &mut String,
) -> Option<(Checked, Option<Record>)> {
    let kind = settings.kind;
    let text = {
        let mut line = Vec::new();
        let mut terminal = env.tty.open(env.shell, env.files);
        match terminal.read_line(CHAT_PROMPT, false, &mut line) {
            Ok(()) => String::from_utf8_lossy(&line).into_owned(),
            Err(ReadError::Interrupted) => {
                terminal.err("\n");
                omsh_exec::clear_interrupt();
                return None;
            }
            Err(_) => return None,
        }
    };
    // Commands the person changed by hand: the model must not undo them.
    let edits: Vec<(usize, String)> = steps
        .iter()
        .enumerate()
        .filter(|(_, step)| step.edited)
        .map(|(index, step)| (index + 1, step.command.clone()))
        .collect();
    let follow_up = match conversation.follow_up(&text, &edits) {
        Ok(request) => request,
        Err(_) if text.trim().is_empty() => return None,
        Err(why) => {
            say(
                env,
                &format!("oh-my-shell: ?: chat: {why}; nothing was sent\n"),
            );
            return None;
        }
    };
    if follow_up_hides_secret(&text, &edits) {
        say(
            env,
            "oh-my-shell: a secret in your message was hidden before sending\n",
        );
    }
    let fail = |env: &mut PlanEnv<'_>, line: &str| {
        say(
            env,
            &format!("oh-my-shell: ?: chat: {line}; the previous plan is unchanged\n"),
        );
    };
    let provider = match provider_for(connect, kind) {
        Ok(provider) => provider,
        Err(line) => {
            fail(env, &line);
            return None;
        }
    };
    let label = format!(
        "Asking {} ({}), chat turn {}",
        kind.label(),
        settings.model,
        conversation.turns() + 1
    );
    let checker = env.shell.clone();
    let check = |answer: &str| check_answer(&checker, answer);
    let in_panel = {
        let terminal = env.tty.open(env.shell, env.files);
        view::can_show(env.menu, &*terminal).is_some()
    };
    let reporter = Reporter {
        calls,
        model: &settings.model,
        ui: env.menu,
        in_panel,
    };
    let (outcome, sent, record) = {
        let mut terminal = env.tty.open(env.shell, env.files);
        ask(
            provider,
            &follow_up,
            &check,
            &label,
            env.paint,
            &mut *terminal,
            &reporter,
        )
    };
    // `[v]` shows what was sent last, even if the answer was no good.
    if let Some(sent) = sent {
        *view = sent_view(&sent, settings);
    }
    match outcome {
        Ok((mut checked, answer)) => {
            conversation.accept(&follow_up, &answer);
            for step in &mut checked.steps {
                count_step(step, env.confirm, env.blast);
            }
            Some((checked, record))
        }
        Err(failure) => {
            if in_panel && let (Some(ui), Some(record)) = (env.menu, &record) {
                say(env, &calls.info_text(ui, record));
            }
            if matches!(&failure, Failure::Provider(error) if error.kind == ErrorKind::Cancelled) {
                omsh_exec::clear_interrupt();
                say(env, "\n");
            }
            fail(env, &failure_line(&failure, kind));
            None
        }
    }
}

/// Changes the command of one step, if the person gives a usable new one.
fn edit_step(env: &mut PlanEnv<'_>, steps: &mut [Reviewed], index: usize) {
    let current = steps[index].command.clone();
    let new = match (env.edit)(&current) {
        Edited::Text(text) => text,
        Edited::Cancelled => return,
        Edited::Unavailable => {
            let mut line = Vec::new();
            let mut terminal = env.tty.open(env.shell, env.files);
            terminal.err(&format!("  current: {current}\n"));
            match terminal.read_line("new command (Enter keeps it)> ", false, &mut line) {
                Ok(()) => String::from_utf8_lossy(&line).into_owned(),
                Err(_) => return,
            }
        }
    };
    let new = new.trim();
    if new.is_empty() || new == current.trim() {
        return;
    }
    let start = start_of(steps, index);
    match review(env.shell, new, &start) {
        Ok(analysis) => {
            let (command, analysis, fixed) =
                with_fixes(env.shell, new, analysis, &start, &mut None);
            let step = &mut steps[index];
            step.missing = missing_commands(env.shell, &command);
            step.command = command;
            step.analysis = analysis;
            step.fixed = fixed;
            step.edited = true;
            count_step(step, env.confirm, env.blast);
            // The edit may have changed where the steps after it start.
            rechain(env, steps, index + 1);
        }
        Err(why) => say(
            env,
            &format!("oh-my-shell: the command was not changed: {why}\n"),
        ),
    }
}

/// Analyzes the steps from `from` on again, each from where the one before it ends.
fn rechain(env: &mut PlanEnv<'_>, steps: &mut [Reviewed], from: usize) {
    for index in from..steps.len() {
        let start = start_of(steps, index);
        let command = steps[index].command.clone();
        if let Ok(analysis) = review(env.shell, &command, &start) {
            steps[index].analysis = analysis;
        }
        count_step(&mut steps[index], env.confirm, env.blast);
    }
}

/// How the steps run.
#[derive(Clone, Copy)]
struct Mode<'a> {
    /// The panels with buttons, when this terminal can show them.
    menu: Option<&'a MenuUi>,
    /// Ask before each step (Run step, Skip, Stop). Only with a menu.
    stepwise: bool,
}

/// Runs the steps in order and stops at the first one that fails. Step by step, each step is
/// asked about first; a skipped step is skipped, and every step is analyzed again at its turn
/// against the shell as it is then, so that a step that depended on a skipped one is judged for
/// what it really does.
fn execute(env: &mut PlanEnv<'_>, steps: &[Reviewed], mode: Mode<'_>) -> LineResult {
    let mut run = Run {
        began: Instant::now(),
        states: vec![RowState::Pending; steps.len()],
    };
    let result = execute_steps(env, steps, mode, &mut run);
    // With the panels, a plan of several steps ends with one overview of where each step is.
    if let Some(ui) = mode.menu
        && steps.len() > 1
    {
        show_summary(env, steps, &mut run, ui);
    }
    result
}

/// Where each step of a running plan is, and since when the plan runs: what the panels of
/// phase 6 show.
struct Run {
    began: Instant,
    states: Vec<RowState>,
}

impl Run {
    fn progress(&self, steps: &[Reviewed]) -> Progress {
        let rows = steps
            .iter()
            .zip(&self.states)
            .map(|(step, state)| RunRow {
                title: shorten(&step.title),
                state: *state,
            })
            .collect();
        Progress::new(rows, self.began.elapsed())
    }
}

/// The panel with every step and its state, after the plan ended. A step that was never
/// reached is "not run".
fn show_summary(env: &mut PlanEnv<'_>, steps: &[Reviewed], run: &mut Run, ui: &MenuUi) {
    for state in &mut run.states {
        if *state == RowState::Pending || *state == RowState::Ask {
            *state = RowState::NotRun;
        }
    }
    let caps = omsh_ui::panels::menu::current_caps(ui);
    let lines = omsh_ui::panels::plan_run::summary(
        &run.progress(steps),
        &caps,
        caps.panel_width(omsh_ui::panels::MAX_WIDTH),
    );
    let mut text = String::from("\n");
    for row in omsh_ui::text::render(&lines, &ui.theme, &caps) {
        text.push_str(&row);
        text.push('\n');
    }
    say(env, &text);
}

fn execute_steps(
    env: &mut PlanEnv<'_>,
    steps: &[Reviewed],
    mode: Mode<'_>,
    run: &mut Run,
) -> LineResult {
    let total = steps.len();
    let mut skipped = 0_usize;
    let glyphs = env.paint.glyphs();
    let ok = env.paint.token(Token::Success, glyphs.ok);
    let failed = env.paint.token(Token::Danger, glyphs.fail);
    let mut last = 0_u8;
    for (index, step) in steps.iter().enumerate() {
        let number = index + 1;
        if let (true, Some(ui)) = (mode.stepwise, mode.menu) {
            run.states[index] = RowState::Ask;
            let answer = view::step_question(ui, index, total, step, run.progress(steps));
            run.states[index] = RowState::Pending;
            match answer {
                view::StepAnswer::Run => {}
                view::StepAnswer::Skip => {
                    run.states[index] = RowState::Skipped;
                    skipped += 1;
                    let line = view::skipped_line(env.paint, number, &step.title);
                    say(env, &line);
                    continue;
                }
                view::StepAnswer::Stop(status) => {
                    omsh_exec::clear_interrupt();
                    return stopped_by_choice(env, steps, index, status);
                }
            }
        }
        let mut head = format!(
            "\n{} {}\n",
            env.paint
                .token(Token::Accent, &format!("[{number}/{total}]")),
            shorten(&step.title)
        );
        block(&mut head, "  $ ", "    ", &step.command);
        say(env, &head);

        let prepared = match env.shell.prepare_unaliased(&step.command, true) {
            Ok(prepared) => prepared,
            Err(result) => {
                let why = message_of(&result);
                say(env, &format!("  {failed}: {why}\n"));
                last = ExitStatus::USAGE.0;
                run.states[index] = RowState::Failed;
                return stopped(env, steps, number, last, false);
            }
        };
        // An earlier step may have changed what this one touches.
        let fresh = confirm::review(env.shell, &prepared);
        let riskier = fresh.level() > step.analysis.level();
        if riskier || mode.stepwise {
            let decision = {
                let mut terminal = env.tty.open(env.shell, env.files);
                if riskier {
                    terminal.err("oh-my-shell: this step is riskier than the plan showed\n");
                }
                confirm::decide_with(
                    &fresh,
                    prepared.text(),
                    env.confirm,
                    Some(env.blast),
                    false,
                    env.paint,
                    mode.menu,
                    &mut *terminal,
                )
            };
            if let Decision::Cancel(code) = decision {
                omsh_exec::clear_interrupt();
                return stopped(env, steps, number, code, false);
            }
        }

        // The trash and the journal (S18): `rm` becomes a move to the trash, and the files that
        // `mkdir`, `mv` and redirections touch are looked at before and after.
        let (prepared, watch) = match env.undo.filter(|_| env.shell.has_journal()) {
            Some(undo) => {
                undo.begin_line(&step.command, Origin::Plan);
                let cwd = std::path::PathBuf::from(crate::repl::directory(env.shell));
                let watch =
                    (!fresh.changes.is_empty()).then(|| Watch::before(undo, &fresh.changes, &cwd));
                let names = confirm::trashable_names(&fresh);
                let prepared = if names.is_empty() {
                    prepared
                } else {
                    match env.shell.rewrite(&prepared, &names, TRASH) {
                        Some(rewritten) => rewritten,
                        None => {
                            // Not expected. Never run the delete for good instead.
                            let _ = undo.end_line();
                            say(
                                env,
                                &format!("  {failed}: could not prepare the move to the trash\n"),
                            );
                            run.states[index] = RowState::Failed;
                            return stopped(env, steps, number, ExitStatus::USAGE.0, false);
                        }
                    }
                };
                (prepared, watch.map(|watch| (watch, cwd)))
            }
            None => (prepared, None),
        };

        let noted = env.history.and_then(|history| {
            let token = history.begin(&step.command, &crate::repl::directory(env.shell))?;
            Some((history, token))
        });
        // The jobs that exist before the step: a job that is new and stopped afterwards is this
        // step, stopped with Ctrl+Z.
        let known: Vec<u32> = env.shell.job_rows().iter().map(|row| row.number).collect();
        let started = Instant::now();
        let result = env.shell.run_prepared(prepared, env.files);
        if let Some((history, token)) = noted {
            let status = match &result {
                LineResult::Ran(outcome) => i32::from(outcome.status().0),
                _ => i32::from(env.shell.last_status().0),
            };
            history.store.finish(token, status, started.elapsed());
        }
        if let Some(undo) = env.undo.filter(|_| env.shell.has_journal()) {
            let mut lines = Vec::new();
            if let Some((watch, cwd)) = watch {
                lines.extend(watch.after(undo, &cwd));
            }
            if let Some(summary) = undo.end_line() {
                lines.push(
                    summary
                        .line()
                        .trim_start_matches("oh-my-shell: ")
                        .to_owned(),
                );
            }
            for line in lines {
                say(env, &format!("  {line}\n"));
            }
        }
        last = match result {
            LineResult::Ran(Outcome::Exit(status)) => {
                run.states[index] = RowState::Done;
                return LineResult::Ran(Outcome::Exit(status));
            }
            LineResult::Ran(Outcome::Continue(status)) => status.0,
            LineResult::Rejected(report) => {
                say(env, &format!("  {failed}: {}\n", report.message()));
                env.shell.last_status().0
            }
            LineResult::Incomplete => ExitStatus::USAGE.0,
        };
        env.recent.record(&step.command, i32::from(last));
        // Ctrl+Z: the step is stopped, not over. With the panels the person decides what
        // happens to it; without them (or if the stopped job cannot be found) the plan ends
        // here, as it always did.
        if is_stop_status(last)
            && let Some(ui) = mode.menu
            && let Some(job) = new_stopped_job(env, &known)
        {
            let mut ask = |progress: Progress| {
                view::pause_question(ui, index, total, &steps[index], progress)
            };
            match pause(env, steps, run, index, job, &mut ask) {
                Paused::Ended(status) => last = status,
                Paused::Stay => {}
                Paused::Skipped => {
                    run.states[index] = RowState::Skipped;
                    skipped += 1;
                    let line = view::skipped_line(env.paint, number, &step.title);
                    say(env, &line);
                    continue;
                }
                Paused::Background(job) => {
                    run.states[index] = RowState::Background;
                    return left_in_the_background(env, steps, number, job);
                }
                Paused::Stuck(job) => {
                    run.states[index] = RowState::Failed;
                    return stuck(env, steps, number, job);
                }
            }
        }
        // A step that only started a background job (`cmd &`) returns 0 at once. If that job
        // ends with a failure right away, the step did not succeed.
        if last == 0
            && let Some((job, word, status)) = failed_background_job(env, &known)
        {
            run.states[index] = RowState::Failed;
            say(env, &format!("  {failed} background job [{job}]: {word}\n"));
            return stopped(env, steps, number, status, true);
        }
        run.states[index] = match last {
            0 => RowState::Done,
            status if is_stop_status(status) => RowState::Paused,
            _ => RowState::Failed,
        };
        if last == 0 {
            say(env, &format!("  {ok}\n"));
        } else {
            say(env, &format!("  {failed} (status {last})\n"));
            // A step that ran and failed: offer the explanation (§13.6). Ctrl+C and a stop are
            // not failures to explain.
            let offer = last != INTERRUPTED && last != STOPPED;
            return stopped(env, steps, number, last, offer);
        }
    }
    let left_out = if skipped > 0 {
        format!(", {skipped} skipped")
    } else {
        String::new()
    };
    say(
        env,
        &format!(
            "\n{ok} {}\n",
            env.paint.token(
                Token::Success,
                &format!(
                    "plan done ({total} step{}{left_out})",
                    if total == 1 { "" } else { "s" }
                )
            )
        ),
    );
    finish(env.shell, last)
}

/// How long a step that started a background job waits to see whether the job fails at once.
const BACKGROUND_GRACE: std::time::Duration = std::time::Duration::from_millis(300);

/// A background job that the step just ended started and that is over with a failure (a nonzero
/// status or a signal): its number, the word `jobs` uses for it and its status. Waits up to
/// [`BACKGROUND_GRACE`] for new jobs to end; a step that started none returns at once, and a job
/// that is still running after the wait is left alone (it may be a server).
fn failed_background_job(env: &PlanEnv<'_>, known: &[u32]) -> Option<(u32, String, u8)> {
    let started = Instant::now();
    loop {
        let fresh: Vec<_> = env
            .shell
            .job_rows()
            .into_iter()
            .filter(|row| {
                !known.contains(&row.number) && !matches!(row.state, JobState::Stopped(_))
            })
            .collect();
        if fresh.is_empty() {
            return None;
        }
        let failed = fresh
            .iter()
            .find(|row| matches!(row.state, JobState::Exit(_) | JobState::Signaled(_)));
        if let Some(row) = failed
            && let Some(status) = env.shell.job_status(row.number)
        {
            return Some((row.number, row.state.word(), status));
        }
        if fresh.iter().all(|row| row.state.is_finished()) || started.elapsed() >= BACKGROUND_GRACE
        {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// The job of the step that Ctrl+Z has just stopped: a stopped job that was not in the table
/// before the step began.
fn new_stopped_job(env: &PlanEnv<'_>, known: &[u32]) -> Option<u32> {
    env.shell
        .job_rows()
        .into_iter()
        .find(|row| !known.contains(&row.number) && matches!(row.state, JobState::Stopped(_)))
        .map(|row| row.number)
}

/// True for the status of a program that was stopped (128 plus `SIGSTOP`, `SIGTSTP`, `SIGTTIN`
/// or `SIGTTOU`), which is what Ctrl+Z gives (148).
fn is_stop_status(status: u8) -> bool {
    (STOPPED - 1..=STOPPED + 2).contains(&status)
}

/// What became of a step that Ctrl+Z stopped.
enum Paused {
    /// It ran on in the foreground and ended with this status (`fg`).
    Ended(u8),
    /// It stays stopped and the plan ends here (also: no answer).
    Stay,
    /// It was ended (`SIGTERM`) and the plan goes on with the next step.
    Skipped,
    /// It goes on as this background job, and the plan ends here.
    Background(u32),
    /// It was asked to end and did not within a second; it is this job.
    Stuck(u32),
}

/// How long Skip waits for the stopped step to end after `SIGTERM`.
const SKIP_WAIT: std::time::Duration = std::time::Duration::from_secs(1);

/// Runs a builtin of the shell for the person who pressed a button, and gives its status. It
/// is not a step: no question, no history line, `command` so that a function of the same name
/// cannot take its place.
fn internal(env: &mut PlanEnv<'_>, text: &str) -> u8 {
    match env.shell.prepare_unaliased(text, true) {
        Ok(prepared) => match env.shell.run_prepared(prepared, env.files) {
            LineResult::Ran(Outcome::Continue(status) | Outcome::Exit(status)) => status.0,
            LineResult::Rejected(_) | LineResult::Incomplete => ExitStatus::USAGE.0,
        },
        Err(_) => ExitStatus::USAGE.0,
    }
}

/// Waits for the job to be over or gone, for at most [`SKIP_WAIT`].
fn job_is_over(env: &PlanEnv<'_>, job: u32) -> bool {
    let end = Instant::now() + SKIP_WAIT;
    loop {
        let over = env
            .shell
            .job_rows()
            .iter()
            .find(|row| row.number == job)
            .is_none_or(|row| row.state.is_finished());
        if over {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// Step `index` was stopped with Ctrl+Z and is job `%job`. Asks what to do, and does it.
/// **Continue** is `fg`, so the step may stop again (and the question comes back); **Skip** is
/// `kill %job` (which also continues a stopped job, so that the signal can act) and the plan
/// goes on; **To background** is `bg`; **Stop plan** (also Esc) leaves it stopped.
fn pause(
    env: &mut PlanEnv<'_>,
    steps: &[Reviewed],
    run: &mut Run,
    index: usize,
    job: u32,
    ask: &mut dyn FnMut(Progress) -> view::PauseAnswer,
) -> Paused {
    loop {
        run.states[index] = RowState::Paused;
        let answer = ask(run.progress(steps));
        match answer {
            view::PauseAnswer::Stop => return Paused::Stay,
            view::PauseAnswer::Continue => {
                let status = internal(env, &format!("command fg %{job}"));
                let again =
                    is_stop_status(status)
                        && env.shell.job_rows().iter().any(|row| {
                            row.number == job && matches!(row.state, JobState::Stopped(_))
                        });
                if !again {
                    return Paused::Ended(status);
                }
            }
            view::PauseAnswer::Skip => {
                internal(env, &format!("command kill %{job}"));
                return if job_is_over(env, job) {
                    Paused::Skipped
                } else {
                    Paused::Stuck(job)
                };
            }
            view::PauseAnswer::Background => {
                internal(env, &format!("command bg %{job}"));
                return Paused::Background(job);
            }
        }
    }
}

/// The plan ends at step `number`, which goes on as background job `%job`.
fn left_in_the_background(
    env: &mut PlanEnv<'_>,
    steps: &[Reviewed],
    number: usize,
    job: u32,
) -> LineResult {
    let left = steps.len() - number;
    say(env, &not_run_lines(env, steps, number));
    let rest = if left == 0 {
        String::new()
    } else {
        format!(
            "; the remaining {left} step{} did not run",
            if left == 1 { "" } else { "s" }
        )
    };
    say(
        env,
        &format!("oh-my-shell: step {number} goes on in the background as %{job}{rest}\n"),
    );
    finish(env.shell, 0)
}

/// Skip did not end the step: it is still job `%job`, and the plan ends.
fn stuck(env: &mut PlanEnv<'_>, steps: &[Reviewed], number: usize, job: u32) -> LineResult {
    say(env, &not_run_lines(env, steps, number));
    say(
        env,
        &format!(
            "oh-my-shell: step {number} did not end after SIGTERM; it is job %{job}; \
             the plan stops here\n"
        ),
    );
    finish(env.shell, FAILED)
}

/// One dim line for every step after step `number` (counting from 1), if there are any.
fn not_run_lines(env: &PlanEnv<'_>, steps: &[Reviewed], number: usize) -> String {
    if steps.len() <= number {
        return String::new();
    }
    let skip = env.paint.glyphs().skip;
    let mut lines = String::from("\n");
    for (index, step) in steps.iter().enumerate().skip(number) {
        let line = format!("{skip} {}. {} (not run)", index + 1, shorten(&step.title));
        lines.push_str("  ");
        lines.push_str(&env.paint.token(Token::Muted, &line));
        lines.push('\n');
    }
    lines
}

/// The person stopped a plan that runs step by step, before step `index` (counting from 0).
fn stopped_by_choice(
    env: &mut PlanEnv<'_>,
    steps: &[Reviewed],
    index: usize,
    status: u8,
) -> LineResult {
    let skip = env.paint.glyphs().skip;
    let mut lines = String::from("\n");
    for (at, step) in steps.iter().enumerate().skip(index) {
        let line = format!("{skip} {}. {} (not run)", at + 1, shorten(&step.title));
        lines.push_str("  ");
        lines.push_str(&env.paint.token(Token::Muted, &line));
        lines.push('\n');
    }
    say(env, &lines);
    let left = steps.len() - index;
    say(
        env,
        &format!(
            "oh-my-shell: stopped before step {}; the remaining {left} step{} did not run\n",
            index + 1,
            if left == 1 { "" } else { "s" }
        ),
    );
    finish(env.shell, status)
}

/// The plan ends at step `number` with `status`.
///
/// `offer` adds the line about `??` (the step ran and failed, so it is the last command).
fn stopped(
    env: &mut PlanEnv<'_>,
    steps: &[Reviewed],
    number: usize,
    status: u8,
    offer: bool,
) -> LineResult {
    let total = steps.len();
    let left = total - number;
    if left > 0 {
        // The steps that never ran, one dim line each, so the end of the plan is plain to see.
        let skip = env.paint.glyphs().skip;
        let mut lines = String::from("\n");
        for (index, step) in steps.iter().enumerate().skip(number) {
            let line = format!("{skip} {}. {} (not run)", index + 1, shorten(&step.title));
            lines.push_str("  ");
            lines.push_str(&env.paint.token(Token::Muted, &line));
            lines.push('\n');
        }
        say(env, &lines);
    }
    let text = if left == 0 {
        format!("\noh-my-shell: step {number} did not succeed (status {status})\n")
    } else {
        format!(
            "\noh-my-shell: step {number} did not succeed (status {status}); the remaining {left} \
             step{} did not run\n",
            if left == 1 { "" } else { "s" }
        )
    };
    say(env, &text);
    if offer {
        say(
            env,
            "oh-my-shell: type ?? (or press Alt+?) to ask why it failed\n",
        );
    }
    finish(env.shell, status)
}

mod desktop;
mod view;

#[cfg(test)]
mod tests;
