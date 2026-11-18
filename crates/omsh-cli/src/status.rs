//! `omsh status` (UI upgrade, phase 5): one page that says what this installation is: the
//! version, how old the build is, whether the AI is set up, whether the daily check is on and
//! what it last found, the motion mode and why, and what the terminal can do.
//!
//! **It asks nothing and sends nothing.** The AI line comes from the configuration alone (no
//! keyring, no network), the update line from the stamp file, and the terminal line from the
//! environment and `isatty`; the synchronized-output answer is shown only if a panel already
//! asked the terminal in this shell. A fact that is not known is said to be unknown.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Local, TimeZone, Utc};
use omsh_config::{AiState, Config, Motion, Paths};
use omsh_exec::Terminal;
use omsh_ui::caps::{Caps, Out};
use omsh_ui::panels::MAX_WIDTH;
use omsh_ui::panels::status::{Row, plain, render};
use omsh_ui::text::{Tone, render_line};
use omsh_ui::theme::Theme;

use crate::update::daily;
use crate::update::notice::{self, OLD_AFTER_DAYS};
use crate::update::version::Version;

/// What the command needs, kept from the start of the shell.
#[derive(Clone)]
pub struct StatusEnv {
    pub config: Config,
    pub paths: Paths,
    /// `--no-ai`.
    pub no_ai: bool,
    /// `--no-config`: no file is read, so none is shown.
    pub no_config: bool,
    /// Where the stamp of the daily check is; `None` under `--no-config`.
    pub state_dir: Option<PathBuf>,
}

/// The things that come from the process and the clock, given so that the rows can be tested.
pub struct Facts<'a> {
    pub caps: Caps,
    /// Seconds since 1970.
    pub now: u64,
    pub current: Version,
    /// The build's commit time, if this build has one.
    pub build_epoch: Option<u64>,
    /// `OMSH_NO_UPDATE_CHECK`.
    pub opt_out: Option<&'a [u8]>,
    /// `OMSH_MOTION`, as written.
    pub env_motion: Option<&'a str>,
    pub term: Option<&'a str>,
    /// The window's columns and rows, when a terminal said so (never a default).
    pub size: Option<(u16, u16)>,
    /// What a panel found out about synchronized output in this shell, if one asked.
    pub sync_output: Option<bool>,
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

/// `1700000000` as `2023-11-14`.
fn date(epoch: u64) -> String {
    i64::try_from(epoch)
        .ok()
        .and_then(|secs| DateTime::<Utc>::from_timestamp(secs, 0))
        .map_or_else(
            || "unknown".to_owned(),
            |at| at.format("%Y-%m-%d").to_string(),
        )
}

/// `1700000000` as a local date and time.
fn local_time(epoch: u64) -> String {
    i64::try_from(epoch)
        .ok()
        .and_then(|secs| Local.timestamp_opt(secs, 0).single())
        .map_or_else(
            || "unknown".to_owned(),
            |at| at.format("%Y-%m-%d %H:%M").to_string(),
        )
}

fn shown_path(file: &Path) -> String {
    file.display().to_string()
}

/// The build row.
fn build_row(facts: &Facts<'_>) -> Row {
    match facts.build_epoch {
        None => Row::new("Build", "unknown (not a release build)", Tone::Muted),
        Some(epoch) => {
            let days = notice::age_days(epoch, facts.now);
            let old = days > OLD_AFTER_DAYS;
            Row::new(
                "Build",
                format!(
                    "{}, {days} days old{}",
                    date(epoch),
                    if old {
                        " (old: omsh update --check)"
                    } else {
                        ""
                    }
                ),
                if old { Tone::Warn } else { Tone::Plain },
            )
        }
    }
}

/// The AI row, from the configuration alone.
fn ai_row(env: &StatusEnv) -> Row {
    match AiState::new(&env.config.ai, env.no_ai) {
        AiState::OffByFlag => Row::new("AI", "off for this session (--no-ai)", Tone::Muted),
        AiState::OffByConfig => Row::new("AI", "off (ai.enabled = false)", Tone::Muted),
        AiState::NotSetUp => Row::new("AI", "not set up (omsh ai setup)", Tone::Muted),
        AiState::Ready {
            provider, model, ..
        } => Row::new(
            "AI",
            format!("ready: {}, {model}", provider.as_str()),
            Tone::Ai,
        ),
    }
}

/// The update rows: whether the check is on, and what it last found.
fn update_rows(env: &StatusEnv, facts: &Facts<'_>) -> Vec<Row> {
    let skip = daily::skip_reason(facts.opt_out, facts.caps.ci, facts.caps.stderr_tty);
    let first = if !env.config.update.check {
        Row::new(
            "Update check",
            "off (omsh update --check looks once)",
            Tone::Plain,
        )
    } else {
        match skip {
            None => Row::new("Update check", "on, once a day", Tone::Ok),
            Some(reason) => Row::new(
                "Update check",
                format!("on, but not running here: {}", reason.words()),
                Tone::Warn,
            ),
        }
    };
    let last = match env.state_dir.as_deref().and_then(daily::read) {
        None => Row::new("Last look", "never", Tone::Muted),
        Some(stamp) => {
            let found = stamp.latest.as_ref().map_or_else(
                || "no release found".to_owned(),
                |latest| {
                    if *latest > facts.current {
                        format!("newest is {latest}, newer than yours")
                    } else {
                        format!("newest is {latest}")
                    }
                },
            );
            Row::new(
                "Last look",
                format!("{}, {found}", local_time(stamp.checked)),
                Tone::Plain,
            )
        }
    };
    vec![first, last]
}

/// The motion row: the level and what decided it, in the order `Caps::decide` uses.
fn motion_row(env: &StatusEnv, facts: &Facts<'_>) -> Row {
    let caps = &facts.caps;
    let level = match caps.motion {
        Motion::Full => "full",
        Motion::Reduced => "reduced",
        Motion::Off => "off",
    };
    let why = if !caps.out_tty {
        "not a terminal"
    } else if caps.dumb {
        "TERM=dumb"
    } else if caps.ci {
        "CI"
    } else if facts
        .env_motion
        .and_then(omsh_ui::caps::parse_motion)
        .is_some()
    {
        "OMSH_MOTION"
    } else if env.config.ui.motion.is_some() {
        "ui.motion"
    } else if caps.ssh {
        "automatic: ssh"
    } else {
        "automatic"
    };
    Row::new("Motion", format!("{level} ({why})"), Tone::Plain)
}

/// The terminal rows.
fn terminal_rows(facts: &Facts<'_>) -> Vec<Row> {
    let caps = &facts.caps;
    let tty = |name: &str, on: bool| format!("{name} {}", if on { "tty" } else { "no" });
    let sync = match facts.sync_output {
        Some(true) => "yes",
        Some(false) => "no",
        None => "not asked yet",
    };
    vec![
        Row::new(
            "Terminal",
            format!(
                "{}, {}",
                facts
                    .term
                    .filter(|t| !t.is_empty())
                    .unwrap_or("TERM not set"),
                facts.size.map_or_else(
                    || "size unknown".to_owned(),
                    |(columns, rows)| format!("{columns}x{rows}")
                )
            ),
            Tone::Plain,
        ),
        Row::new(
            "Streams",
            format!(
                "{}, {}, {}",
                tty("stdin", caps.stdin_tty),
                tty("stdout", caps.stdout_tty),
                tty("stderr", caps.stderr_tty)
            ),
            Tone::Plain,
        ),
        Row::new(
            "Display",
            format!(
                "unicode {}, color {}{}",
                yes_no(caps.unicode),
                yes_no(caps.color),
                if caps.no_color { " (NO_COLOR)" } else { "" },
            ),
            Tone::Plain,
        ),
        Row::new("Sync output", sync, Tone::Plain),
        Row::new(
            "Session",
            format!(
                "ssh {}, tmux {}, CI {}",
                yes_no(caps.ssh),
                yes_no(caps.tmux),
                yes_no(caps.ci)
            ),
            Tone::Plain,
        ),
    ]
}

/// Every row, in the order they are shown.
#[must_use]
pub fn rows(env: &StatusEnv, facts: &Facts<'_>) -> Vec<Row> {
    let mut rows = vec![
        Row::new("Version", facts.current.to_string(), Tone::Plain),
        build_row(facts),
        ai_row(env),
    ];
    rows.extend(update_rows(env, facts));
    rows.push(motion_row(env, facts));
    rows.extend(terminal_rows(facts));
    rows.push(if env.no_config {
        Row::new("Config file", "none (--no-config)", Tone::Muted)
    } else {
        env.paths.config_file().map_or_else(
            || Row::new("Config file", "unknown (no home folder)", Tone::Muted),
            |file| Row::new("Config file", shown_path(&file), Tone::Path),
        )
    });
    rows.push(match env.state_dir.as_deref() {
        Some(dir) => Row::new("State folder", shown_path(dir), Tone::Path),
        None => Row::new("State folder", "none", Tone::Muted),
    });
    rows
}

/// Runs `omsh status`.
pub fn run(args: &[&str], terminal: &mut dyn Terminal, env: &StatusEnv) -> u8 {
    if let Some(other) = args.first() {
        terminal.err(&format!(
            "omsh: status: {other}: takes no arguments\nusage: omsh status\n"
        ));
        return 2;
    }
    let mut caps = Caps::detect(Out::Stdout, env.config.ui.icons, env.config.ui.motion);
    // The page goes to **this command's** standard output: `omsh status | cat` is a pipe even
    // when the shell's own output is a terminal.
    let to_terminal = terminal.output_is_terminal();
    caps.stdout_tty = to_terminal;
    caps.out_tty = to_terminal;
    caps.unicode = caps.unicode && to_terminal;
    caps.color = caps.color && to_terminal;
    if !to_terminal {
        // The same rule as `Caps::decide`: nothing moves for a pipe.
        caps.motion = Motion::Off;
    }
    // The update rules look at standard error (that is where the shell's own lines go).
    caps.stderr_tty =
        Caps::detect(Out::Stderr, env.config.ui.icons, env.config.ui.motion).stderr_tty;
    let opt_out = std::env::var_os(daily::OPT_OUT);
    let env_motion = std::env::var("OMSH_MOTION").ok();
    let term = std::env::var("TERM").ok();
    let facts = Facts {
        caps,
        now: daily::now(),
        current: Version::current(),
        build_epoch: notice::build_epoch(),
        opt_out: opt_out
            .as_deref()
            .map(std::os::unix::ffi::OsStrExt::as_bytes),
        env_motion: env_motion.as_deref(),
        term: term.as_deref(),
        // A window of 0 columns or rows is "not known" (a terminal that was never given a size).
        size: omsh_ui::caps::CapsFacts::from_environment()
            .size
            .filter(|(columns, rows)| *columns > 0 && *rows > 0),
        sync_output: omsh_ui::sync::known(),
    };
    let rows = rows(env, &facts);
    let text = if caps.out_tty {
        let width = caps.panel_width(MAX_WIDTH);
        let theme = Theme::default();
        let mut out = String::new();
        for line in render(&rows, &caps, width) {
            out.push_str(&render_line(&line, &theme, &caps));
            out.push('\n');
        }
        out
    } else {
        let mut out = plain(&rows).join("\n");
        out.push('\n');
        out
    };
    terminal.out(&text);
    0
}

#[cfg(test)]
mod tests;
