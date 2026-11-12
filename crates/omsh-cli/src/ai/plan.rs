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
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use omsh_ai::{
    ChatRequest, Conversation, ErrorKind, Gathered, GitState, ModelRisk, Plan, PlanContext,
    PlanError, PlanRequest, Policy, Provider, ProviderError, Role, SOFT_TURN_LIMIT, Settings,
    collect_text_counting, follow_up_hides_secret, hides_secret, plan_request, retry_request,
    runtime,
};
use omsh_config::{AiContext, AiLanguage, ConfirmLevel};
use omsh_editor::Head;
use omsh_exec::{ExitStatus, Io, Key, LineResult, Outcome, ReadError, Shell, TRASH, Terminal};
use omsh_safety::blast::Radius;
use omsh_safety::journal::Origin;
use omsh_safety::{Analysis, Finding, Level};
use omsh_ui::{Paint, Token};

use super::recent::Recent;
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
    pub language: AiLanguage,
    pub context: AiContext,
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
    /// The same after aliases are replaced: what would run.
    text: String,
    analysis: Analysis,
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
/// (its status, its source text) changes. `start` is where the command starts: for a step of a
/// plan, where the step before it ended. `Err` is a short reason why it cannot run here.
fn review(shell: &Shell, command: &str, start: &StartDir) -> Result<(String, Analysis), String> {
    let mut scratch = shell.clone();
    match scratch.prepare(command, true) {
        Ok(prepared) => {
            let analysis = confirm::review_at(&scratch, &prepared, start);
            Ok((prepared.text().to_owned(), analysis))
        }
        Err(result) => Err(message_of(&result)),
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
    for (index, step) in plan.steps.iter().enumerate() {
        let start = start_of(&out, index);
        let (text, analysis) = review(shell, &step.command, &start).map_err(|why| {
            PlanError::new(format!(
                "the command of step {} cannot be used here ({why}); use only the supported \
                 POSIX sh syntax",
                index + 1
            ))
        })?;
        out.push(Reviewed {
            title: step.title.clone(),
            explanation: step.explanation.clone(),
            needs_root: step.needs_root,
            model: step.risk,
            command: step.command.clone(),
            text,
            analysis,
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

/// Asks, checks the answer, and asks once more with the error if it was no good. Ends with the
/// provider dropped, inside the runtime (see `omsh_ai::runtime::block_on`).
async fn attempts<T>(
    provider: Box<dyn Provider>,
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
        let answer =
            match collect_text_counting(provider.as_ref(), &request, &policy, received).await {
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
    drop(provider);
    outcome
}

/// Runs [`attempts`] and shows the wait: on a terminal one line that is redrawn with the real
/// time and the number of characters that have arrived; otherwise one plain line.
pub(crate) fn ask<T>(
    provider: Box<dyn Provider>,
    first: &ChatRequest,
    check: &dyn Fn(&str) -> Result<T, PlanError>,
    label: &str,
    paint: &Paint,
    terminal: &mut dyn Terminal,
) -> (Result<(T, String), Failure>, Option<Sent>) {
    let received = AtomicUsize::new(0);
    let tries = AtomicUsize::new(1);
    let sent = RefCell::new(None);
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
        attempts(provider, first, check, &received, &tries, &sent),
        &watch,
    );
    wait.clear(&mut **terminal.borrow_mut());
    let result = match outcome {
        Ok(result) => result,
        Err(error) => Err(Failure::Provider(error)),
    };
    (result, sent.into_inner())
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
        block(&mut out, "     $ ", "       ", &step.command);
        if step.text.trim_end() != step.command.trim_end() {
            block(&mut out, "     runs as: ", "       ", &step.text);
        }
        if step.edited {
            out.push_str("     (your edit; the model's explanation was for its own command)\n");
        } else if !step.explanation.trim().is_empty() {
            block(&mut out, "     ", "     ", &step.explanation);
        }
        if step.needs_root {
            out.push_str(
                "     needs root: the plan does not add sudo; it may fail until you decide how \
                 to run it\n",
            );
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
                        out.push_str(&paint.token(Token::Dim, &line));
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
        undo_lines(paint, &mut out, &step.analysis, journal);
    }
    out.push('\n');
    out
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
        language,
        context: allowed,
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
        language,
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
    let (outcome, sent) = {
        let mut terminal = env.tty.open(env.shell, env.files);
        ask(provider, &first, &check, &label, env.paint, &mut *terminal)
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
    loop {
        let journal = env.undo.is_some() && env.shell.has_journal();
        let text = panel(
            env.paint,
            &summary,
            &settings,
            &steps,
            env.blast,
            journal,
            conversation.turns(),
        );
        let choice = {
            let mut terminal = env.tty.open(env.shell, env.files);
            terminal.err(&text);
            choose(&mut *terminal, steps.len(), &view, env.paint)
        };
        match choice {
            Choice::Run => break,
            Choice::Cancel(status) => {
                if status == INTERRUPTED {
                    say(&mut env, "\n");
                }
                say(&mut env, "oh-my-shell: cancelled; nothing ran\n");
                return finish(env.shell, status);
            }
            Choice::Edit(index) => edit_step(&mut env, &mut steps, index),
            Choice::Chat => {
                if let Some(new) = chat_turn(
                    &mut env,
                    &mut connect,
                    &settings,
                    &mut conversation,
                    &steps,
                    &mut view,
                ) {
                    summary = new.summary;
                    steps = new.steps;
                }
            }
        }
    }

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
    if steps
        .iter()
        .any(|step| step.analysis.level() == Level::Critical)
    {
        let targets: Vec<&str> = critical.iter().map(String::as_str).collect();
        let decision = {
            let mut terminal = env.tty.open(env.shell, env.files);
            terminal.err("oh-my-shell: this plan has a critical step\n");
            confirm::ask(Level::Critical, &targets, env.paint, &mut *terminal)
        };
        if let Decision::Cancel(code) = decision {
            return finish(env.shell, code);
        }
    } else if top >= confirm::threshold(env.confirm) {
        // Enter on the panel showed the plan; a step at the level that asks for a typed line
        // asks once more here, with the counts above, as `[safety] confirm` says.
        let decision = {
            let mut terminal = env.tty.open(env.shell, env.files);
            terminal.err(&format!(
                "oh-my-shell: this plan has a {} step\n",
                top.name()
            ));
            confirm::ask(top, &[], env.paint, &mut *terminal)
        };
        if let Decision::Cancel(code) = decision {
            return finish(env.shell, code);
        }
    }

    execute(&mut env, &steps)
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
    conversation: &mut Conversation,
    steps: &[Reviewed],
    view: &mut String,
) -> Option<Checked> {
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
    let (outcome, sent) = {
        let mut terminal = env.tty.open(env.shell, env.files);
        ask(
            provider,
            &follow_up,
            &check,
            &label,
            env.paint,
            &mut *terminal,
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
            Some(checked)
        }
        Err(failure) => {
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
        Ok((text, analysis)) => {
            let step = &mut steps[index];
            step.command = new.to_owned();
            step.text = text;
            step.analysis = analysis;
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
        if let Ok((text, analysis)) = review(env.shell, &command, &start) {
            steps[index].text = text;
            steps[index].analysis = analysis;
        }
        count_step(&mut steps[index], env.confirm, env.blast);
    }
}

/// Runs the steps in order and stops at the first one that fails.
fn execute(env: &mut PlanEnv<'_>, steps: &[Reviewed]) -> LineResult {
    let total = steps.len();
    let glyphs = env.paint.glyphs();
    let ok = env.paint.token(Token::Success, glyphs.ok);
    let failed = env.paint.token(Token::Danger, glyphs.fail);
    let mut last = 0_u8;
    for (index, step) in steps.iter().enumerate() {
        let number = index + 1;
        let mut head = format!(
            "\n{} {}\n",
            env.paint
                .token(Token::Accent, &format!("[{number}/{total}]")),
            shorten(&step.title)
        );
        block(&mut head, "  $ ", "    ", &step.command);
        say(env, &head);

        let prepared = match env.shell.prepare(&step.command, true) {
            Ok(prepared) => prepared,
            Err(result) => {
                let why = message_of(&result);
                say(env, &format!("  {failed}: {why}\n"));
                last = ExitStatus::USAGE.0;
                return stopped(env, steps, number, last, false);
            }
        };
        // An earlier step may have changed what this one touches.
        let fresh = confirm::review(env.shell, &prepared);
        if fresh.level() > step.analysis.level() {
            let decision = {
                let mut terminal = env.tty.open(env.shell, env.files);
                terminal.err("oh-my-shell: this step is riskier than the plan showed\n");
                confirm::decide(
                    &fresh,
                    prepared.text(),
                    env.confirm,
                    Some(env.blast),
                    env.paint,
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
    say(
        env,
        &format!(
            "\n{ok} {}\n",
            env.paint.token(
                Token::Success,
                &format!(
                    "plan done ({total} step{})",
                    if total == 1 { "" } else { "s" }
                )
            )
        ),
    );
    finish(env.shell, last)
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
            lines.push_str(&env.paint.token(Token::Dim, &line));
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

#[cfg(test)]
mod tests;
