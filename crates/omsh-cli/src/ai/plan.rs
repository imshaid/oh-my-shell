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

use std::cell::{Cell, RefCell};
use std::io::{self, IsTerminal};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use omsh_ai::{
    ChatRequest, ErrorKind, Gathered, GitState, ModelRisk, Plan, PlanContext, PlanError,
    PlanRequest, Policy, Provider, ProviderError, Role, Settings, collect_text_counting,
    hides_secret, plan_request, retry_request, runtime,
};
use omsh_config::{AiContext, AiLanguage, ConfirmLevel};
use omsh_editor::Head;
use omsh_exec::{ExitStatus, Io, Key, LineResult, Outcome, ReadError, Shell, Terminal};
use omsh_safety::blast::Radius;
use omsh_safety::{Analysis, Finding, Level};

use super::recent::Recent;
use crate::confirm::{self, Decision, StartDir};
use crate::history::History;
use crate::radius::{self, BlastLimits};

/// How many lines of a command, and of an explanation, the panel shows, and how wide.
const SHOWN_LINES: usize = 5;
const SHOWN_WIDTH: usize = 160;
/// How often the "waiting" line is redrawn.
const REDRAW: Duration = Duration::from_millis(150);
/// Exit status after a request that could not be answered or was given up.
const FAILED: u8 = 1;
const USAGE_STATUS: u8 = 2;
const INTERRUPTED: u8 = 130;

/// What `?` alone prints.
pub const USAGE: &str =
    "usage: ? <what you want done>   (example: ? show the 5 biggest files here)\n";

const KEYS: &str = "[Enter] run  [e] edit  [v] view context  [Esc] cancel ";

// -- what the REPL hands over ---------------------------------------------------------------

/// A provider that is ready to be asked, with the settings that decide what is sent.
pub struct PlanSetup {
    pub provider: Box<dyn Provider>,
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
    /// Draw ✓ and ✗ (otherwise `ok` and `failed`).
    pub unicode: bool,
    /// Changes the text of a command in the line editor.
    pub edit: &'a mut dyn FnMut(&str) -> Edited,
}

// -- masking and context --------------------------------------------------------------------

/// What is known about the shell right now, for the items `allowed` lets through. What is off
/// is not even collected; `PlanContext::build` looks at the switches again and masks.
fn gather(shell: &Shell, allowed: &AiContext, recent: &Recent) -> Gathered {
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

enum Failure {
    Provider(ProviderError),
    Invalid(PlanError),
}

/// The request that was sent last (the first one, or the second try), for `[v]`.
#[derive(Clone)]
struct Sent {
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
) -> Result<T, Failure> {
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
                outcome = Ok(value);
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
fn ask(
    provider: Box<dyn Provider>,
    first: &ChatRequest,
    check: &dyn Fn(&str) -> Result<Checked, PlanError>,
    label: &str,
    terminal: &mut dyn Terminal,
) -> (Result<Checked, Failure>, Option<Sent>) {
    let received = AtomicUsize::new(0);
    let tries = AtomicUsize::new(1);
    let sent = RefCell::new(None);
    let live = io::stderr().is_terminal();
    if !live {
        terminal.err(&format!("{label}...\n"));
    }
    let started = Instant::now();
    let last_draw = Cell::new(None::<Instant>);
    let terminal = RefCell::new(terminal);
    let watch = || {
        let mut terminal = terminal.borrow_mut();
        if terminal.interrupted() {
            return true;
        }
        if live && last_draw.get().is_none_or(|at| at.elapsed() >= REDRAW) {
            let chars = received.load(Ordering::Relaxed);
            let mut line = format!(
                "\r\x1b[K{label}... {:.1} s",
                started.elapsed().as_secs_f64()
            );
            if chars > 0 {
                line.push_str(&format!(", {chars} characters"));
            }
            if tries.load(Ordering::Relaxed) > 1 {
                line.push_str(" (second try)");
            }
            terminal.err(&line);
            last_draw.set(Some(Instant::now()));
        }
        false
    };
    let outcome = runtime::block_on(
        attempts(provider, first, check, &received, &tries, &sent),
        &watch,
    );
    if last_draw.get().is_some() {
        terminal.borrow_mut().err("\r\x1b[K");
    }
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

fn panel(summary: &str, settings: &Settings, steps: &[Reviewed], blast: BlastLimits) -> String {
    let mut out = format!(
        "oh-my-shell: plan from {} ({}):\n  {}\n",
        settings.kind.label(),
        settings.model,
        shorten(summary)
    );
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
            "\n  {}. {}   [{}{note}]\n",
            index + 1,
            shorten(&step.title),
            rating.level.name()
        ));
        block(&mut out, "     $ ", "       ", &step.command);
        if step.text.trim_end() != step.command.trim_end() {
            block(&mut out, "     runs as: ", "       ", &step.text);
        }
        if !step.explanation.trim().is_empty() {
            block(&mut out, "     ", "     ", &step.explanation);
        }
        if step.needs_root {
            out.push_str(
                "     needs root: the plan does not add sudo; it may fail until you decide how \
                 to run it\n",
            );
        }
        let mut counted = false;
        for (at, finding) in step.analysis.findings.iter().enumerate() {
            out.push_str("     - ");
            out.push_str(&finding.reason);
            out.push('\n');
            if let Some(target) = &finding.target {
                out.push_str("       on: ");
                out.push_str(&shorten(target));
                out.push('\n');
            }
            if let Some(Some(found)) = step.radii.get(at) {
                counted = true;
                let cwd = finding.files.as_ref().and_then(|f| f.cwd.as_deref());
                for line in radius::lines(found, cwd, blast) {
                    out.push_str("       ");
                    out.push_str(&line);
                    out.push('\n');
                }
            }
        }
        if counted && index > 0 {
            out.push_str("       (counted now, before the steps above have run)\n");
        }
    }
    out.push('\n');
    out
}

// -- what was sent --------------------------------------------------------------------------

/// `text` as it may be shown: no control characters but line feeds.
fn displayable(text: &str) -> String {
    text.chars()
        .filter(|c| *c == '\n' || !c.is_control())
        .collect()
}

/// Exactly what went to the model (`[v]`, §13.5): the instructions with the context lines, and
/// the request. After a second try, the refused first answer was sent back too; its own text is
/// not printed again (it is the model's, and it may hold escape codes).
fn sent_view(sent: &Sent, settings: &Settings) -> String {
    let request = &sent.request;
    let mut out = format!(
        "\noh-my-shell: this was sent to {} ({}):\n\ninstructions:\n",
        settings.kind.label(),
        settings.model
    );
    for line in displayable(request.system.as_deref().unwrap_or_default()).lines() {
        out.push_str("  | ");
        out.push_str(line);
        out.push('\n');
    }
    let mut user_messages = request.messages.iter().filter(|m| m.role == Role::User);
    out.push_str("\nrequest:\n");
    for line in displayable(user_messages.next().map_or("", |m| m.text.as_str())).lines() {
        out.push_str("  | ");
        out.push_str(line);
        out.push('\n');
    }
    if let Some(chars) = sent.first_answer_chars {
        out.push_str(&format!(
            "\nsecond try: the model's first answer ({chars} characters) was sent back, with:\n"
        ));
        for line in displayable(user_messages.next().map_or("", |m| m.text.as_str())).lines() {
            out.push_str("  | ");
            out.push_str(line);
            out.push('\n');
        }
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
    Cancel(u8),
}

/// Waits for a key on the panel. Only Enter (or `y`) runs; `e` edits; `v` shows what was sent
/// and goes on waiting; Esc, `n` and `q` cancel; any other key is ignored.
fn choose(terminal: &mut dyn Terminal, steps: usize, view: &str) -> Choice {
    loop {
        match terminal.read_key(KEYS) {
            Ok(Key::Enter | Key::Char('y' | 'Y')) => return Choice::Run,
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

/// Handles a `? request` line. Returns what the REPL does next, like running a line.
pub fn run(setup: PlanSetup, request: &str, mut env: PlanEnv<'_>) -> LineResult {
    if request.trim().is_empty() {
        say(&mut env, USAGE);
        return finish(env.shell, USAGE_STATUS);
    }

    // -- ask
    let context = PlanContext::build(
        gather(env.shell, &setup.context, env.recent),
        &setup.context,
    );
    let plan_asked = PlanRequest {
        request,
        context: &context,
        language: setup.language,
    };
    if hides_secret(&plan_asked) {
        say(
            &mut env,
            "oh-my-shell: a secret in the request or the context was hidden before sending\n",
        );
    }
    let first = plan_request(&plan_asked);
    let label = format!(
        "Asking {} ({})",
        setup.settings.kind.label(),
        setup.settings.model
    );
    let checker = env.shell.clone();
    let check = |answer: &str| -> Result<Checked, PlanError> {
        let plan = Plan::parse(answer)?;
        let steps = review_plan(&checker, &plan)?;
        Ok(Checked {
            summary: plan.summary,
            steps,
        })
    };
    let (outcome, sent) = {
        let mut terminal = env.tty.open(env.shell, env.files);
        ask(setup.provider, &first, &check, &label, &mut *terminal)
    };
    // What `[v]` shows: the request that was really sent last (a second try, if there was one).
    let view = sent_view(
        &sent.unwrap_or(Sent {
            request: first,
            first_answer_chars: None,
        }),
        &setup.settings,
    );
    let kind = setup.settings.kind;
    let Checked { summary, mut steps } = match outcome {
        Ok(checked) => checked,
        Err(Failure::Provider(error)) if error.kind == ErrorKind::Cancelled => {
            say(&mut env, "\noh-my-shell: cancelled; nothing ran\n");
            return finish(env.shell, INTERRUPTED);
        }
        Err(Failure::Provider(error)) => {
            say(
                &mut env,
                &format!("oh-my-shell: ?: {}\n", error.one_line(kind)),
            );
            return finish(env.shell, FAILED);
        }
        Err(Failure::Invalid(error)) => {
            say(
                &mut env,
                &format!("oh-my-shell: ?: couldn't make a plan: {error}\n"),
            );
            return finish(env.shell, FAILED);
        }
    };

    // -- count what the destructive steps would touch, then show it and wait for a key
    for step in &mut steps {
        count_step(step, env.confirm, env.blast);
    }
    loop {
        let text = panel(&summary, &setup.settings, &steps, env.blast);
        let choice = {
            let mut terminal = env.tty.open(env.shell, env.files);
            terminal.err(&text);
            choose(&mut *terminal, steps.len(), &view)
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
            confirm::ask(Level::Critical, &targets, &mut *terminal)
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
            confirm::ask(top, &[], &mut *terminal)
        };
        if let Decision::Cancel(code) = decision {
            return finish(env.shell, code);
        }
    }

    execute(&mut env, &steps)
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
    let (ok, failed) = if env.unicode {
        ("✓", "✗")
    } else {
        ("ok", "failed")
    };
    let mut last = 0_u8;
    for (index, step) in steps.iter().enumerate() {
        let number = index + 1;
        let mut head = format!("\n[{number}/{total}] {}\n", shorten(&step.title));
        block(&mut head, "  $ ", "    ", &step.command);
        say(env, &head);

        let prepared = match env.shell.prepare(&step.command, true) {
            Ok(prepared) => prepared,
            Err(result) => {
                let why = message_of(&result);
                say(env, &format!("  {failed}: {why}\n"));
                last = ExitStatus::USAGE.0;
                return stopped(env, number, total, last);
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
                    &mut *terminal,
                )
            };
            if let Decision::Cancel(code) = decision {
                omsh_exec::clear_interrupt();
                return stopped(env, number, total, code);
            }
        }

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
            return stopped(env, number, total, last);
        }
    }
    say(
        env,
        &format!(
            "\n{ok} plan done ({total} step{})\n",
            if total == 1 { "" } else { "s" }
        ),
    );
    finish(env.shell, last)
}

/// The plan ends at step `number` with `status`.
fn stopped(env: &mut PlanEnv<'_>, number: usize, total: usize, status: u8) -> LineResult {
    let left = total - number;
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
    finish(env.shell, status)
}

#[cfg(test)]
mod tests;
