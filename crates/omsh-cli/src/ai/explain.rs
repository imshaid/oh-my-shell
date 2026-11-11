//! Explain features (S24, §13.2): `?? <command>`, `??` / `Alt+?` after a failure, and
//! `cmd | ? <question>`.
//!
//! They ask for prose. **Nothing the model says is ever run**, and there is no panel and no
//! confirmation: the person asked a question and gets an answer. What is sent is decided in
//! `omsh_ai::explain` (masking, limits); this file decides *when* and what the person sees.
//!
//! - `??` text: the shell's own safety check is shown first (locally, never sent), then the
//!   explanation.
//! - `??` alone and `Alt+?`: the last command of this shell that failed, with its exit status.
//!   **Its error text is not sent**: programs write to the terminal directly, and capturing it
//!   would change what every program sees (`isatty(2)`). Decided with the owner on 2026-11-11.
//! - `cmd | ? question`: the command runs through the normal line path (analyzer, question,
//!   trash), its standard output goes into a pipe that the shell reads, and then the (masked,
//!   limited) output and the question go to the model.
//!
//! The answer is written to standard output as it arrives; everything else goes to standard error.

use std::cell::{Cell, RefCell};
use std::os::fd::OwnedFd;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

use omsh_ai::{
    ChatRequest, ErrorKind, Output, OutputAsk, PlanContext, Policy, Provider, Settings,
    answer_about_output, explain_command, explain_failure, hides_secret_in, runtime, stream_text,
};
use omsh_config::ConfirmLevel;
use omsh_exec::{ExitStatus, Io, LineResult, Outcome, Shell, Terminal};
use omsh_safety::Level;

use super::plan::{PlanSetup, TerminalSource, gather, provider_for};
use super::recent::{Last, Recent};
use super::wait::WaitLine;
use crate::confirm;
use crate::radius::BlastLimits;
use crate::undo::Undo;

const FAILED: u8 = 1;
const USAGE_STATUS: u8 = 2;
const INTERRUPTED: u8 = 130;
/// Exit status of a job that was stopped (Ctrl+Z): 128 + SIGTSTP.
const STOPPED: u8 = 148;

/// How much of the start and of the end of a command's output the shell keeps (the model gets
/// less; see `omsh_ai::Output`).
const KEEP_BYTES: usize = 32 * 1024;
/// How long the shell waits for output that arrives after the command has ended (a background
/// process that still holds the pipe).
const GRACE: Duration = Duration::from_millis(300);

/// What the explain flows need from the shell.
pub struct ExplainEnv<'a> {
    pub shell: &'a mut Shell,
    pub files: &'a mut Io,
    pub tty: &'a mut dyn TerminalSource,
    pub recent: &'a mut Recent,
}

fn say(env: &mut ExplainEnv<'_>, text: &str) {
    env.tty.open(env.shell, env.files).err(text);
}

fn finish(shell: &mut Shell, status: u8) -> LineResult {
    shell.set_last_status(ExitStatus(status));
    LineResult::Ran(Outcome::Continue(ExitStatus(status)))
}

/// What the person reads when the AI cannot be used.
pub fn unavailable(env: &mut ExplainEnv<'_>, label: &str, why: &str) -> LineResult {
    say(env, &format!("oh-my-shell: {label}: {why}\n"));
    finish(env.shell, FAILED)
}

// -- the answer ------------------------------------------------------------------------------

/// How a request ended.
#[derive(Debug, PartialEq, Eq)]
enum Ended {
    Answered,
    Cancelled,
    Failed(String),
}

/// `text` as it may be written to the terminal: no control characters but line feed and tab.
fn printable(text: &str) -> String {
    text.chars()
        .filter(|c| *c == '\n' || *c == '\t' || !c.is_control())
        .collect()
}

/// Asks, and writes the answer to standard output as it arrives.
fn stream_answer(
    provider: Box<dyn Provider>,
    request: &ChatRequest,
    label: &str,
    terminal: &mut dyn Terminal,
) -> Ended {
    let kind = provider.kind();
    let wait = WaitLine::start(label, terminal);
    let shown = Cell::new(false);
    let ended_with_newline = Cell::new(true);
    let terminal = RefCell::new(terminal);
    let watch = || {
        let mut terminal = terminal.borrow_mut();
        if terminal.interrupted() {
            return true;
        }
        if !shown.get() {
            wait.tick(&mut **terminal, "");
        }
        false
    };
    let mut on_piece = |piece: &str| {
        let text = printable(piece);
        if text.is_empty() {
            return;
        }
        let mut terminal = terminal.borrow_mut();
        if !shown.replace(true) {
            wait.clear(&mut **terminal);
        }
        ended_with_newline.set(text.ends_with('\n'));
        terminal.out(&text);
    };
    let outcome = runtime::block_on(
        async {
            let policy = Policy::standard();
            let result = stream_text(provider.as_ref(), request, &policy, &mut on_piece).await;
            // Inside the runtime, so that no connection outlives the request (S20).
            drop(provider);
            result
        },
        &watch,
    );
    wait.clear(&mut **terminal.borrow_mut());
    let mut terminal = terminal.borrow_mut();
    if shown.get() && !ended_with_newline.get() {
        terminal.out("\n");
    }
    match outcome {
        Ok(Ok(text)) if text.trim().is_empty() => Ended::Failed("the model gave no answer".into()),
        Ok(Ok(_)) => Ended::Answered,
        Ok(Err(error)) if error.kind == ErrorKind::Cancelled => Ended::Cancelled,
        Ok(Err(error)) => Ended::Failed(error.one_line(kind)),
        Err(error) if error.kind == ErrorKind::Cancelled => Ended::Cancelled,
        Err(error) => Ended::Failed(error.one_line(kind)),
    }
}

/// Makes a provider, sends `request`, shows the answer. Returns the exit status of the flow.
fn ask(
    connect: &mut super::plan::Connect,
    settings: &Settings,
    request: &ChatRequest,
    env: &mut ExplainEnv<'_>,
    what: &str,
) -> u8 {
    let provider = match provider_for(connect, settings.kind) {
        Ok(provider) => provider,
        Err(line) => {
            say(env, &format!("oh-my-shell: {what}: {line}\n"));
            return FAILED;
        }
    };
    let label = format!("Asking {} ({})", settings.kind.label(), settings.model);
    let ended = {
        let mut terminal = env.tty.open(env.shell, env.files);
        stream_answer(provider, request, &label, &mut *terminal)
    };
    match ended {
        Ended::Answered => 0,
        Ended::Cancelled => {
            omsh_exec::clear_interrupt();
            say(env, "\noh-my-shell: cancelled\n");
            INTERRUPTED
        }
        Ended::Failed(line) => {
            say(env, &format!("oh-my-shell: {what}: {line}\n"));
            FAILED
        }
    }
}

// -- ?? <command or topic> -------------------------------------------------------------------

/// `?? text`: explains without running. The shell's own check of the text is shown first.
pub fn command(setup: PlanSetup, text: &str, mut env: ExplainEnv<'_>) -> LineResult {
    let PlanSetup {
        mut connect,
        settings,
        language,
        context,
    } = setup;
    let ctx = PlanContext::build(gather(env.shell, &context, env.recent), &context);
    // What the safety analyzer says, locally. If the text is not a command it simply says
    // nothing.
    let mut scratch = env.shell.clone();
    if let Ok(prepared) = scratch.prepare(text, true) {
        let analysis = confirm::review(&scratch, &prepared);
        for finding in analysis.findings.iter().filter(|f| f.level > Level::Safe) {
            say(
                &mut env,
                &format!(
                    "oh-my-shell: the shell's own check: {}: {}\n",
                    finding.level.name(),
                    finding.reason
                ),
            );
        }
    }
    if hides_secret_in(&[text], &ctx, None) {
        say(
            &mut env,
            "oh-my-shell: a secret in the text or the context was hidden before sending\n",
        );
    }
    let request = explain_command(text, &ctx, language);
    let status = ask(&mut connect, &settings, &request, &mut env, "??");
    finish(env.shell, status)
}

// -- ?? alone, Alt+? --------------------------------------------------------------------------

/// Why there is nothing to send for the last command, or the command and status to explain.
#[derive(Debug, PartialEq, Eq)]
pub enum Target {
    Explain { command: String, status: i32 },
    Nothing(&'static str),
}

/// What `??` alone / `Alt+?` should do about the last command of this shell.
#[must_use]
pub fn target(last: Option<&Last>) -> Target {
    match last {
        None | Some(Last::Ran { status: 0, .. }) => {
            Target::Nothing("nothing to explain: the last command succeeded")
        }
        Some(Last::Ignored) => {
            Target::Nothing("the last command matches [history] ignore; it is not sent")
        }
        Some(Last::Ran { status, .. }) if *status == i32::from(INTERRUPTED) => {
            Target::Nothing("the last command was interrupted with Ctrl+C; nothing to explain")
        }
        Some(Last::Ran { status, .. }) if *status == i32::from(STOPPED) => {
            Target::Nothing("the last command was stopped (Ctrl+Z); nothing to explain")
        }
        Some(Last::Ran { command, status }) => Target::Explain {
            command: command.clone(),
            status: *status,
        },
    }
}

/// `??` alone (`typed`) or `Alt+?`: why did the last command fail? `setup` is the AI session's
/// answer; it is looked at only when there is something to send. Alt+? does not touch `$?`.
pub fn last(
    setup: impl FnOnce() -> Result<PlanSetup, String>,
    typed: bool,
    mut env: ExplainEnv<'_>,
) -> LineResult {
    let keep = env.shell.last_status();
    let done = |env: &mut ExplainEnv<'_>, status: u8| {
        if typed {
            finish(env.shell, status)
        } else {
            env.shell.set_last_status(keep);
            LineResult::Ran(Outcome::Continue(keep))
        }
    };
    let (command, status) = match target(env.recent.last()) {
        Target::Nothing(why) => {
            say(&mut env, &format!("oh-my-shell: {why}\n"));
            return done(&mut env, USAGE_STATUS);
        }
        Target::Explain { command, status } => (command, status),
    };
    let setup = match setup() {
        Ok(setup) => setup,
        Err(why) => {
            say(&mut env, &format!("oh-my-shell: ??: {why}\n"));
            return done(&mut env, FAILED);
        }
    };
    let PlanSetup {
        mut connect,
        settings,
        language,
        context,
    } = setup;
    let ctx = PlanContext::build(gather(env.shell, &context, env.recent), &context);
    if hides_secret_in(&[&command], &ctx, None) {
        say(
            &mut env,
            "oh-my-shell: a secret in the command or the context was hidden before sending\n",
        );
    }
    say(
        &mut env,
        &format!(
            "oh-my-shell: explaining `{}` (exit {status}); its error text is not sent. To include \
             it: <command> 2>&1 | ? why did this fail\n",
            one_line(&command)
        ),
    );
    let request = explain_failure(&command, status, &ctx, language);
    let code = ask(&mut connect, &settings, &request, &mut env, "??");
    done(&mut env, code)
}

/// A command on one line for a message, cut short.
fn one_line(command: &str) -> String {
    let first = command.lines().next().unwrap_or_default();
    let clean: String = first.chars().filter(|c| !c.is_control()).collect();
    if clean.chars().count() > 60 || command.lines().nth(1).is_some() {
        let mut cut: String = clean.chars().take(60).collect();
        cut.push_str("...");
        cut
    } else {
        clean
    }
}

// -- cmd | ? question -------------------------------------------------------------------------

/// What the drain thread has read from the command's standard output.
#[derive(Default)]
struct Captured {
    head: Vec<u8>,
    tail: Vec<u8>,
    total: u64,
    done: bool,
}

impl Captured {
    fn add(&mut self, chunk: &[u8]) {
        self.total += chunk.len() as u64;
        let room = KEEP_BYTES - self.head.len();
        let (first, rest) = chunk.split_at(room.min(chunk.len()));
        self.head.extend_from_slice(first);
        self.tail.extend_from_slice(rest);
        if self.tail.len() > KEEP_BYTES {
            let extra = self.tail.len() - KEEP_BYTES;
            self.tail.drain(..extra);
        }
    }
}

type Shared = Arc<(Mutex<Captured>, Condvar)>;

/// Reads `reader` until its writers are gone, keeping the start and the end. Runs on a thread
/// that the shell does not wait for beyond the grace time.
fn drain(mut reader: impl std::io::Read, shared: &Shared) {
    let mut chunk = [0_u8; 8192];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                shared
                    .0
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .add(&chunk[..n]);
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    shared.0.lock().unwrap_or_else(PoisonError::into_inner).done = true;
    shared.1.notify_all();
}

/// Waits until the drain has seen the end of the output, or `grace` has passed. True if the end
/// was seen.
fn finished(shared: &Shared, grace: Duration) -> bool {
    let guard = shared.0.lock().unwrap_or_else(PoisonError::into_inner);
    let (guard, _) = shared
        .1
        .wait_timeout_while(guard, grace, |captured| !captured.done)
        .unwrap_or_else(PoisonError::into_inner);
    guard.done
}

/// What the line editor and the shell need to run the command part like a typed line.
pub struct PipeEnv<'a> {
    pub confirm: ConfirmLevel,
    pub blast: BlastLimits,
    pub undo: Option<&'a Undo>,
}

/// `command | ? question`. The command runs like a typed line; its standard output is read by
/// the shell and, if it ran, sent with the question.
pub fn pipe(
    setup: Result<PlanSetup, String>,
    command: &str,
    question: &str,
    pipe_env: PipeEnv<'_>,
    mut env: ExplainEnv<'_>,
) -> LineResult {
    // Before anything runs: if the AI cannot answer, the command must not run either (the
    // person wrote `| ?` to ask a question about its output).
    let setup = match setup {
        Ok(setup) => setup,
        Err(why) => return unavailable(&mut env, "?", &why),
    };
    let PlanSetup {
        mut connect,
        settings,
        language,
        context,
    } = setup;

    let Ok((reader, writer)) = std::io::pipe() else {
        say(
            &mut env,
            "oh-my-shell: ?: cannot make a pipe; nothing ran\n",
        );
        return finish(env.shell, FAILED);
    };
    let shared: Shared = Arc::default();
    let drain_shared = Arc::clone(&shared);
    if std::thread::Builder::new()
        .name("pipe-drain".to_owned())
        .spawn(move || drain(reader, &drain_shared))
        .is_err()
    {
        say(
            &mut env,
            "oh-my-shell: ?: cannot read the output; nothing ran\n",
        );
        return finish(env.shell, FAILED);
    }

    let (result, ran) = {
        let mut table = env.files.clone();
        table.set(1, Arc::new(OwnedFd::from(writer)));
        let outcome = confirm::run_line_outcome(
            env.shell,
            command,
            true,
            &mut table,
            pipe_env.confirm,
            pipe_env.blast,
            pipe_env.undo,
        );
        // Let go of the writing end, or the reader never sees the end of the output.
        drop(table);
        outcome
    };
    if !ran {
        // Cancelled at the question, or the command could not be run: nothing to ask about.
        // (The drain ends by itself when the pipe is closed.)
        return result;
    }
    let status = match &result {
        LineResult::Ran(Outcome::Exit(_)) => return result,
        LineResult::Ran(Outcome::Continue(status)) => i32::from(status.0),
        _ => return result,
    };
    let complete = finished(&shared, GRACE);
    let (head, tail, total) = {
        let captured = shared.0.lock().unwrap_or_else(PoisonError::into_inner);
        (captured.head.clone(), captured.tail.clone(), captured.total)
    };

    if status == i32::from(INTERRUPTED) || status == i32::from(STOPPED) {
        say(
            &mut env,
            "oh-my-shell: the command was interrupted; nothing was sent\n",
        );
        return result;
    }
    env.recent.record(command, status);

    let ctx = PlanContext::build(gather(env.shell, &context, env.recent), &context);
    let output = Output::build(&head, &tail, total);
    let mut note = describe(&output);
    if !complete {
        note.push_str("; output written after the command ended was not read");
    }
    if hides_secret_in(&[command, question], &ctx, Some(&output)) {
        note.push_str("; a secret was hidden");
    }
    say(&mut env, &format!("oh-my-shell: {note}\n"));
    let request = answer_about_output(
        &OutputAsk {
            command,
            question,
            status: Some(status),
            output: &output,
        },
        &ctx,
        language,
    );
    let code = ask(&mut connect, &settings, &request, &mut env, "?");
    // The status of the line is the command's own unless the question could not be answered.
    finish(
        env.shell,
        if code == 0 {
            u8::try_from(status).unwrap_or(FAILED)
        } else {
            code
        },
    )
}

/// What is being sent, in one line, with real numbers.
fn describe(output: &Output) -> String {
    if output.binary {
        return format!(
            "the output is binary ({} bytes); none of it is sent, only your question",
            output.total_bytes
        );
    }
    if output.total_bytes == 0 {
        return "the command wrote nothing to standard output; sending your question".to_owned();
    }
    if output.cut {
        format!(
            "sending {} characters of the output (the start and the end of {} bytes)",
            output.sent_chars, output.total_bytes
        )
    } else {
        format!("sending {} characters of output", output.sent_chars)
    }
}

#[cfg(test)]
mod tests;
