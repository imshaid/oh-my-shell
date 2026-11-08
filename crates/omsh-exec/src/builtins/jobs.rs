//! The builtins for background jobs: `jobs`, `wait` and `kill`.
//!
//! `kill` is a builtin so that it understands job specs (`kill %1`), which only the shell can
//! turn into a process group. `wait` is one because only the shell's own children can be waited
//! for. `fg` and `bg` come with S09b.

use std::time::Duration;

use nix::errno::Errno;
use nix::sys::signal::{Signal, kill, killpg};
use nix::unistd::Pid;

use crate::exec::{Flow, Unwind};
use crate::io::Io;
use crate::jobs::Phase;
use crate::jobtable::{View, parse_spec};
use crate::shell::Shell;
use crate::status::ExitStatus;

/// How often `wait` looks at the Ctrl+C flag while it sleeps. A job that changes wakes it
/// sooner.
const WAIT_STEP: Duration = Duration::from_millis(100);

/// Finds the job a word names, or says why not. `name` is the builtin, for the message.
fn find_job(shell: &Shell, name: &str, word: &[u8], io: &Io) -> Option<View> {
    let shown = String::from_utf8_lossy(word);
    let Some(spec) = parse_spec(word) else {
        io.say(format!("oh-my-shell: {name}: {shown}: no such job"));
        return None;
    };
    match shell.jobs.find(&spec) {
        Ok(view) => Some(view),
        Err(error) => {
            io.say(format!("oh-my-shell: {name}: {shown}: {}", error.message()));
            None
        }
    }
}

// ------------------------------------------------------------------------------------ jobs

/// `jobs [-l|-p] [spec...]`: lists the background jobs. A job that has finished is shown once
/// more here and then forgotten.
pub(super) fn jobs(shell: &mut Shell, args: &[Vec<u8>], io: &Io) -> Flow {
    let (mut long, mut pids_only) = (false, false);
    let mut rest = args;
    while let Some(arg) = rest.first() {
        if arg == b"--" {
            rest = &rest[1..];
            break;
        }
        if !arg.starts_with(b"-") || arg.len() == 1 {
            break;
        }
        for &letter in &arg[1..] {
            match letter {
                b'l' => long = true,
                b'p' => pids_only = true,
                other => {
                    io.say(format!(
                        "oh-my-shell: jobs: -{}: invalid option",
                        char::from(other)
                    ));
                    return Ok(ExitStatus::USAGE);
                }
            }
        }
        rest = &rest[1..];
    }

    let mut status = ExitStatus::SUCCESS;
    let views = if rest.is_empty() {
        shell.jobs.list()
    } else {
        let mut views = Vec::new();
        for word in rest {
            match find_job(shell, "jobs", word, io) {
                Some(view) => views.push(view),
                None => status = ExitStatus(1),
            }
        }
        views
    };

    let mut text = String::new();
    for view in &views {
        if pids_only {
            if let Some(pid) = view.job.leader() {
                text.push_str(&format!("{pid}\n"));
            }
        } else {
            text.push_str(&view.line(long));
            text.push('\n');
        }
        if matches!(view.phase, Phase::Finished(_)) {
            shell.jobs.remove(view.number);
        }
    }
    let written = shell.emit(io, 1, text.as_bytes())?;
    Ok(if status == ExitStatus::SUCCESS {
        written
    } else {
        status
    })
}

// ------------------------------------------------------------------------------------ wait

/// `wait [spec|pid...]`: waits for background jobs and gives the status of the last one named.
/// With nothing named it waits for all of them and gives 0. A job that is stopped is not waited
/// for: `wait` says so and goes on, as bash does.
pub(super) fn wait(shell: &mut Shell, args: &[Vec<u8>], io: &Io) -> Flow {
    if args.is_empty() {
        return wait_all(shell, io);
    }
    let mut status = ExitStatus::SUCCESS;
    for word in args {
        status = wait_for(shell, word, io)?;
    }
    Ok(status)
}

fn wait_for(shell: &mut Shell, word: &[u8], io: &Io) -> Flow {
    let shown = String::from_utf8_lossy(word).into_owned();
    let view = if let Some(spec) = parse_spec(word) {
        match shell.jobs.find(&spec) {
            Ok(view) => view,
            Err(error) => {
                io.say(format!("oh-my-shell: wait: {shown}: {}", error.message()));
                return Ok(ExitStatus::NOT_FOUND);
            }
        }
    } else if let Some(pid) = parse_pid(word) {
        match shell.jobs.find_pid(pid) {
            Some(view) => view,
            None => {
                io.say(format!(
                    "oh-my-shell: wait: pid {shown} is not a child of this shell"
                ));
                return Ok(ExitStatus::NOT_FOUND);
            }
        }
    } else {
        io.say(format!(
            "oh-my-shell: wait: {shown}: not a pid or valid job spec"
        ));
        return Ok(ExitStatus::USAGE);
    };

    let bell = shell.jobs.bell();
    loop {
        // Read before looking, so that a change in between is not missed.
        let seen = bell.count();
        if shell.interrupted() {
            return Err(Unwind::Interrupted);
        }
        match view.job.phase() {
            Phase::Finished(status) => {
                // The user has been told by `wait` itself: no notice later.
                shell.jobs.remove(view.number);
                return Ok(status);
            }
            Phase::Stopped(signal) => {
                io.say(format!("oh-my-shell: wait: job {} is stopped", view.number));
                return Ok(ExitStatus::from_signal(signal));
            }
            Phase::Running => bell.wait_after(seen, WAIT_STEP),
        }
    }
}

fn wait_all(shell: &mut Shell, io: &Io) -> Flow {
    let bell = shell.jobs.bell();
    let mut warned = Vec::new();
    loop {
        let seen = bell.count();
        if shell.interrupted() {
            return Err(Unwind::Interrupted);
        }
        let mut running = false;
        for view in shell.jobs.list() {
            match view.phase {
                Phase::Running => running = true,
                Phase::Stopped(_) if !warned.contains(&view.number) => {
                    warned.push(view.number);
                    io.say(format!("oh-my-shell: wait: job {} is stopped", view.number));
                }
                Phase::Stopped(_) | Phase::Finished(_) => {}
            }
        }
        if !running {
            break;
        }
        bell.wait_after(seen, WAIT_STEP);
    }
    // `wait` has seen them end: they need no notice.
    for view in shell.jobs.list() {
        if matches!(view.phase, Phase::Finished(_)) {
            shell.jobs.remove(view.number);
        }
    }
    Ok(ExitStatus::SUCCESS)
}

fn parse_pid(word: &[u8]) -> Option<Pid> {
    if word.is_empty() || !word.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(word)
        .ok()?
        .parse::<i32>()
        .ok()
        .filter(|pid| *pid > 0)
        .map(Pid::from_raw)
}

// ------------------------------------------------------------------------------------ kill

const KILL_USAGE: &str =
    "kill: usage: kill [-s sigspec | -n signum | -sigspec] pid | jobspec ... or kill -l [sigspec]";

/// A signal given by name (`TERM`, `SIGTERM`, any case) or number. `Some(None)` is signal 0,
/// which only checks that the process exists.
fn parse_signal(text: &[u8]) -> Option<Option<Signal>> {
    let text = std::str::from_utf8(text).ok()?;
    if let Ok(number) = text.parse::<i32>() {
        return if number == 0 {
            Some(None)
        } else {
            Signal::try_from(number).ok().map(Some)
        };
    }
    let upper = text.to_ascii_uppercase();
    let name = upper.strip_prefix("SIG").unwrap_or(&upper);
    Signal::iterator()
        .find(|signal| signal.as_str().strip_prefix("SIG") == Some(name))
        .map(Some)
}

/// `kill [-s sig | -n num | -sig] pid|%job...` and `kill -l [sig...]`.
pub(super) fn kill_(shell: &mut Shell, args: &[Vec<u8>], io: &Io) -> Flow {
    let mut signal = Some(Signal::SIGTERM);
    let mut rest = args;
    while let Some(arg) = rest.first() {
        let arg = arg.as_slice();
        if arg == b"--" {
            rest = &rest[1..];
            break;
        }
        if arg == b"-l" || arg == b"-L" {
            return list_signals(shell, &rest[1..], io);
        }
        if arg == b"-s" || arg == b"-n" {
            let Some(name) = rest.get(1) else {
                io.say(format!(
                    "oh-my-shell: kill: {}: option requires an argument",
                    String::from_utf8_lossy(arg)
                ));
                return Ok(ExitStatus::USAGE);
            };
            let Some(parsed) = parse_signal(name) else {
                io.say(format!(
                    "oh-my-shell: kill: {}: invalid signal specification",
                    String::from_utf8_lossy(name)
                ));
                return Ok(ExitStatus(1));
            };
            signal = parsed;
            rest = &rest[2..];
            continue;
        }
        // `-TERM`, `-15`: but `-123` after the signal is a process group, so only the first
        // such word counts, and only if it names a signal.
        if arg.len() > 1 && arg.starts_with(b"-") {
            let Some(parsed) = parse_signal(&arg[1..]) else {
                io.say(format!(
                    "oh-my-shell: kill: {}: invalid signal specification",
                    String::from_utf8_lossy(&arg[1..])
                ));
                return Ok(ExitStatus(1));
            };
            signal = parsed;
            rest = &rest[1..];
            continue;
        }
        break;
    }
    if rest.is_empty() {
        io.say(format!("oh-my-shell: {KILL_USAGE}"));
        return Ok(ExitStatus::USAGE);
    }

    let mut status = ExitStatus::SUCCESS;
    for word in rest {
        if !send(shell, word, signal, io) {
            status = ExitStatus(1);
        }
    }
    Ok(status)
}

/// Sends the signal to one process, group or job. Says why and returns false on failure.
fn send(shell: &Shell, word: &[u8], signal: Option<Signal>, io: &Io) -> bool {
    let shown = String::from_utf8_lossy(word).into_owned();
    if parse_spec(word).is_some() {
        let Some(view) = find_job(shell, "kill", word, io) else {
            return false;
        };
        if matches!(view.phase, Phase::Finished(_)) {
            io.say(format!("oh-my-shell: kill: {shown}: no such job"));
            return false;
        }
        let Some(signal) = signal else { return true };
        return match view.job.signal(signal) {
            Ok(()) => true,
            Err(errno) => {
                io.say(format!("oh-my-shell: kill: {shown}: {}", errno.desc()));
                false
            }
        };
    }
    let Some(number) = std::str::from_utf8(word)
        .ok()
        .and_then(|w| w.parse::<i32>().ok())
    else {
        io.say(format!(
            "oh-my-shell: kill: {shown}: arguments must be process or job IDs"
        ));
        return false;
    };
    // A negative number is a process group, as for the `kill` command.
    let result = if number < 0 {
        match number.checked_neg() {
            Some(group) => killpg(Pid::from_raw(group), signal),
            None => Err(Errno::ESRCH),
        }
    } else {
        kill(Pid::from_raw(number), signal)
    };
    match result {
        Ok(()) => true,
        Err(errno) => {
            io.say(format!("oh-my-shell: kill: ({shown}) - {}", errno.desc()));
            false
        }
    }
}

/// `kill -l`: the signal names, or the name of a number and the number of a name.
fn list_signals(shell: &Shell, words: &[Vec<u8>], io: &Io) -> Flow {
    let mut text = String::new();
    let mut status = ExitStatus::SUCCESS;
    if words.is_empty() {
        let mut all: Vec<Signal> = Signal::iterator().collect();
        all.sort_by_key(|s| *s as i32);
        let names: Vec<&str> = all
            .iter()
            .map(|s| s.as_str().strip_prefix("SIG").unwrap_or(s.as_str()))
            .collect();
        text = names.join(" ");
        text.push('\n');
    }
    for word in words {
        // A number is a signal number, or an exit status that a signal made (128 + n).
        let number = std::str::from_utf8(word)
            .ok()
            .and_then(|w| w.parse::<i32>().ok())
            .map(|n| if n > 128 { n - 128 } else { n });
        let found = match number {
            Some(n) => Signal::try_from(n).ok().map(|s| {
                s.as_str()
                    .strip_prefix("SIG")
                    .unwrap_or(s.as_str())
                    .to_owned()
            }),
            None => parse_signal(word).flatten().map(|s| (s as i32).to_string()),
        };
        if let Some(found) = found {
            text.push_str(&found);
            text.push('\n');
        } else {
            io.say(format!(
                "oh-my-shell: kill: {}: invalid signal specification",
                String::from_utf8_lossy(word)
            ));
            status = ExitStatus(1);
        }
    }
    let written = shell.emit(io, 1, text.as_bytes())?;
    Ok(if status == ExitStatus::SUCCESS {
        written
    } else {
        status
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signals_are_read_by_name_or_number() {
        assert_eq!(parse_signal(b"TERM"), Some(Some(Signal::SIGTERM)));
        assert_eq!(parse_signal(b"SIGTERM"), Some(Some(Signal::SIGTERM)));
        assert_eq!(parse_signal(b"term"), Some(Some(Signal::SIGTERM)));
        assert_eq!(parse_signal(b"9"), Some(Some(Signal::SIGKILL)));
        assert_eq!(parse_signal(b"0"), Some(None));
        assert_eq!(parse_signal(b"NOSUCH"), None);
        assert_eq!(parse_signal(b"999"), None);
        assert_eq!(parse_signal(b""), None);
    }

    #[test]
    fn a_pid_is_a_positive_number() {
        assert_eq!(parse_pid(b"42"), Some(Pid::from_raw(42)));
        assert_eq!(parse_pid(b"0"), None);
        assert_eq!(parse_pid(b"-4"), None);
        assert_eq!(parse_pid(b"%1"), None);
        assert_eq!(parse_pid(b""), None);
    }

    #[test]
    fn spec_alone_is_not_a_pid() {
        assert!(parse_spec(b"%1").is_some());
        assert_eq!(parse_spec(b"12"), None);
    }
}
