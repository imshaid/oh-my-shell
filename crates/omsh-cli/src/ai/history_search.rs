//! `? history <request>` (S25, §13.2): a natural-language search of the local history.
//!
//! The model never sees the history. It gets the request (masked) and today's date, and answers
//! with a small filter (`omsh_ai::HistoryFilter`) that is checked field by field. The shell turns
//! the filter into a [`Query`] and runs it on `history.db`. The person sees one line saying, in
//! the shell's own words, what was searched, then the same listing as the `history` builtin.
//!
//! Nothing here runs a command, and nothing from the history is sent anywhere.

use chrono::{DateTime, Datelike, Local, NaiveDate, TimeZone};
use omsh_ai::{
    Day, ErrorKind, HistoryFilter, Status, history_request_hides_secret, history_search_request,
};
use omsh_exec::{ExitStatus, Io, LineResult, Outcome, Shell, Trigger, trigger};
use omsh_history::{Entry, Query, SETTLE};
use omsh_ui::Paint;

use super::plan::{Failure, PlanSetup, TerminalSource, ask, provider_for};
use crate::history::{History, local_time, render};
use crate::repl::directory;

const FAILED: u8 = 1;
const USAGE_STATUS: u8 = 2;
const INTERRUPTED: u8 = 130;

/// What `? history` alone prints.
pub const USAGE: &str = "usage: ? history <what to look for>   (example: ? history docker \
                         commands that failed yesterday)\n";

/// The word after `?` that makes a request a history search.
const WORD: &str = "history";

/// The request of a `? history ...` line, or `None` if the request is something else
/// (`historyx`, `histories`, `show history`). A bare `history` gives an empty request.
#[must_use]
pub fn request_of(request: &str) -> Option<&str> {
    let request = request.trim_start();
    let rest = request.strip_prefix(WORD)?;
    match rest.chars().next() {
        None => Some(""),
        Some(c) if c.is_whitespace() => Some(rest.trim()),
        Some(_) => None,
    }
}

/// What the flow needs from the shell.
pub struct SearchEnv<'a> {
    pub shell: &'a mut Shell,
    pub files: &'a mut Io,
    pub tty: &'a mut dyn TerminalSource,
    pub history: Option<&'a History>,
    pub paint: &'a Paint,
}

fn say(env: &mut SearchEnv<'_>, text: &str) {
    env.tty.open(env.shell, env.files).err(text);
}

fn finish(shell: &mut Shell, status: u8) -> LineResult {
    shell.set_last_status(ExitStatus(status));
    LineResult::Ran(Outcome::Continue(ExitStatus(status)))
}

/// The first moment of `day` in local time. A day that has no midnight (a clock change at 00:00)
/// starts at its first hour that exists.
fn day_start_ms(day: Day) -> Option<i64> {
    let date = NaiveDate::from_ymd_opt(day.year, day.month, day.day)?;
    (0..6).find_map(|hour| {
        let time = date.and_hms_opt(hour, 0, 0)?;
        Local
            .from_local_datetime(&time)
            .earliest()
            .map(|moment| moment.timestamp_millis())
    })
}

/// The first moment of the day after `day`.
fn next_day_start_ms(day: Day) -> Option<i64> {
    let date = NaiveDate::from_ymd_opt(day.year, day.month, day.day)?.succ_opt()?;
    day_start_ms(Day {
        year: date.year(),
        month: date.month(),
        day: date.day(),
    })
}

/// The query for a checked filter. `cwd` is the folder the person is in. It asks for one entry more
/// than `newest` so that the line that started this search can be left out.
fn query_for(filter: &HistoryFilter, cwd: &str) -> Result<Query, String> {
    let since_ms = match filter.from {
        Some(day) => Some(day_start_ms(day).ok_or("cannot work out the first date")?),
        None => None,
    };
    let until_ms = match filter.to {
        Some(day) => Some(next_day_start_ms(day).ok_or("cannot work out the last date")?),
        None => None,
    };
    Ok(Query {
        cwd: filter.here.then(|| cwd.to_owned()),
        words_all: filter.all.clone(),
        words_any: filter.any.clone(),
        failed: filter.status == Status::Failed,
        succeeded: filter.status == Status::Succeeded,
        since_ms,
        until_ms,
        limit: Some(filter.newest + 1),
        newest_first: true,
        ..Query::default()
    })
}

/// True for the line of this shell that is running now: a `? history ...` search that has not
/// ended. It is in the history already and would match its own words.
fn is_running_search(entry: &Entry, session: i64) -> bool {
    entry.session == session
        && entry.exit_status.is_none()
        && matches!(
            trigger(&entry.command),
            Some(Trigger::Plan(request)) if request_of(request).is_some()
        )
}

/// The entries to show, newest first, without the search itself and at most `newest`.
fn shown(mut found: Vec<Entry>, session: i64, newest: u32) -> Vec<Entry> {
    found.retain(|entry| !is_running_search(entry, session));
    found.truncate(usize::try_from(newest).unwrap_or(usize::MAX));
    found
}

/// `today` as the model is told: `2026-11-11 (Wednesday)`.
fn today_text(now: DateTime<Local>) -> String {
    now.format("%Y-%m-%d (%A)").to_string()
}

/// Handles `? history <request>`. `setup` is the AI (or why it cannot be used). Nothing is sent
/// unless there is a history to search.
pub fn run(
    setup: Result<PlanSetup, String>,
    request: &str,
    now: DateTime<Local>,
    mut env: SearchEnv<'_>,
) -> LineResult {
    if request.trim().is_empty() {
        say(&mut env, USAGE);
        return finish(env.shell, USAGE_STATUS);
    }
    let Some(history) = env.history else {
        say(
            &mut env,
            "oh-my-shell: ? history: this shell has no history to search\n",
        );
        return finish(env.shell, FAILED);
    };
    let PlanSetup {
        mut connect,
        settings,
        ..
    } = match setup {
        Ok(setup) => setup,
        Err(why) => {
            say(
                &mut env,
                &format!(
                    "oh-my-shell: ? history: {why}\n\
                     oh-my-shell: without the AI, `history search TEXT` finds commands by their text\n"
                ),
            );
            return finish(env.shell, FAILED);
        }
    };
    let kind = settings.kind;
    let provider = match provider_for(&mut connect, kind) {
        Ok(provider) => provider,
        Err(line) => {
            say(&mut env, &format!("oh-my-shell: ? history: {line}\n"));
            return finish(env.shell, FAILED);
        }
    };

    // -- ask: only the request and today's date leave the machine
    say(
        &mut env,
        "oh-my-shell: ? history: sending your request and today's date; no command from your \
         history is sent\n",
    );
    if history_request_hides_secret(request) {
        say(
            &mut env,
            "oh-my-shell: a secret in the request was hidden before sending\n",
        );
    }
    let first = history_search_request(request, &today_text(now));
    let label = format!("Asking {} ({})", kind.label(), settings.model);
    let check = |answer: &str| HistoryFilter::parse(answer);
    let (outcome, _) = {
        let mut terminal = env.tty.open(env.shell, env.files);
        ask(provider, &first, &check, &label, env.paint, &mut *terminal)
    };
    let filter = match outcome {
        Ok((filter, _)) => filter,
        Err(Failure::Provider(error)) if error.kind == ErrorKind::Cancelled => {
            omsh_exec::clear_interrupt();
            say(&mut env, "\noh-my-shell: cancelled; nothing was searched\n");
            return finish(env.shell, INTERRUPTED);
        }
        Err(Failure::Provider(error)) => {
            say(
                &mut env,
                &format!("oh-my-shell: ? history: {}\n", error.one_line(kind)),
            );
            return finish(env.shell, FAILED);
        }
        Err(Failure::Invalid(error)) => {
            say(
                &mut env,
                &format!(
                    "oh-my-shell: ? history: couldn't read the search: {error}\n\
                     oh-my-shell: try again in other words, or use `history search TEXT`\n"
                ),
            );
            return finish(env.shell, FAILED);
        }
    };

    // -- search the local database and show what it holds
    let query = match query_for(&filter, &directory(env.shell)) {
        Ok(query) => query,
        Err(why) => {
            say(&mut env, &format!("oh-my-shell: ? history: {why}\n"));
            return finish(env.shell, FAILED);
        }
    };
    say(
        &mut env,
        &format!("oh-my-shell: ? history: {}\n", filter.describe()),
    );
    // So that the commands that just ran are there.
    history.store.settle(SETTLE);
    let found = shown(
        history.store.query(&query),
        history.store.session(),
        filter.newest,
    );
    if found.is_empty() {
        say(&mut env, "oh-my-shell: ? history: no command matches\n");
        return finish(env.shell, FAILED);
    }
    env.tty
        .open(env.shell, env.files)
        .out(&render(&found, local_time));
    let count = found.len();
    say(
        &mut env,
        &format!(
            "oh-my-shell: ? history: {count} command{} shown\n",
            if count == 1 { "" } else { "s" }
        ),
    );
    finish(env.shell, 0)
}

#[cfg(test)]
mod tests;
