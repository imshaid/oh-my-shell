//! The read-run loop.
//!
//! This is a plain line reader for now: no editing, history or completion (S13 adds the
//! editor). It reads standard input and runs what it finds.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, ErrorKind, IsTerminal, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};

use omsh_config::{Config, Paths};
use omsh_exec::{
    ExitStatus, Io, LineResult, Outcome, Shell, hung_up, note_inherited_signals, release_terminal,
    start_interactive, wait_for_input,
};

use crate::report::{self, Style};

const PROMPT: &str = "$ ";
const CONTINUATION_PROMPT: &str = "> ";

/// How the shell was asked to run (see `choose` in `lib.rs`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    /// A person is typing the commands: prompts, notices, Ctrl+C survival. Set for a
    /// terminal and by `-i`.
    pub interactive: bool,
    /// A login shell: import the login environment before the first prompt.
    pub login: bool,
    /// `--no-ai`. The AI layer does not exist yet (S20), so nothing reads this.
    pub no_ai: bool,
    /// `--no-config`: read neither `config.toml` nor `init.omsh`, and write nothing.
    pub no_config: bool,
}

/// Runs the shell on standard input until it ends, and returns the exit status. `name` is the
/// name the shell was started under, when `$0` should show it (a login shell: `-oh-my-shell`).
pub fn run(options: Options, name: Option<&OsStr>) -> ExitStatus {
    // Before anything changes a signal setting: what was ignored at the start stays ignored.
    note_inherited_signals();
    let interactive = options.interactive;
    // Job control needs a real terminal; `-i` on a pipe still prompts but has none.
    let job_control = if interactive && io::stdin().is_terminal() {
        match start_interactive() {
            Ok(()) => true,
            Err(reason) => {
                say(&format!(
                    "oh-my-shell: no job control in this shell: {reason}"
                ));
                false
            }
        }
    } else {
        false
    };
    let input = match stdin_without_buffer() {
        Ok(file) => file,
        Err(error) => {
            say(&format!("oh-my-shell: cannot read standard input: {error}"));
            release_terminal();
            return ExitStatus(1);
        }
    };
    let status = run_on(input, options, job_control, name);
    // Give the terminal back to the program that started the shell.
    release_terminal();
    status
}

/// A second handle on file descriptor 0 that does not buffer.
///
/// `std::io::stdin()` reads ahead in blocks of several kilobytes. If a command such as
/// `head -n 1` then reads from the same input, the lines the shell has already taken are
/// gone. The shell reads one byte at a time instead and takes exactly one line. The new
/// handle shares its position with fd 0 (both name the same open file), so a child that
/// inherits fd 0 continues where the shell stopped.
fn stdin_without_buffer() -> io::Result<File> {
    Ok(File::from(io::stdin().as_fd().try_clone_to_owned()?))
}

fn run_on(
    mut input: impl Read,
    options: Options,
    job_control: bool,
    name: Option<&OsStr>,
) -> ExitStatus {
    let interactive = options.interactive;
    let mut shell = Shell::new();
    shell.set_interactive(interactive);
    shell.set_job_control(job_control);
    if let Some(name) = name {
        shell.set_name(name.as_bytes());
    }
    // The files that commands start with: this process's standard input, output and error.
    let mut files = Io::process();
    if options.login {
        // The profile files are in `sh` syntax, so `/bin/sh` reads them (see `omsh-exec`).
        shell.import_login_environment(&files);
    }
    // After the login environment, so that a profile may set `XDG_CONFIG_HOME`.
    let config = match start_up(&mut shell, &mut files, options) {
        Ok(config) => config,
        // `exit` in the startup file.
        Err(status) => return finish(&mut shell, status, &files),
    };
    let style = Style::detect(config.ui.icons);
    // The lines read so far for the command being typed: more than one when it is unfinished.
    let mut pending = String::new();
    loop {
        if interactive {
            if hung_up() {
                return finish(&mut shell, ExitStatus(HANGUP_STATUS), &files);
            }
            if pending.is_empty() {
                // What happened to the background jobs since the last prompt.
                shell.job_notices(&files);
            }
            prompt(if pending.is_empty() {
                PROMPT
            } else {
                CONTINUATION_PROMPT
            });
        }
        // Wait in steps, so that a `SIGHUP` sent to an idle shell is noticed: a plain read
        // would be restarted after the signal and never come back.
        if interactive && !wait_for_input(io::stdin().as_fd()) {
            return finish(&mut shell, ExitStatus(HANGUP_STATUS), &files);
        }
        let at_end = match read_line(&mut input) {
            Ok(Some(bytes)) => {
                if append_utf8(&mut pending, bytes).is_err() {
                    say("oh-my-shell: the input is not valid UTF-8; the line was skipped");
                    pending.clear();
                    shell.set_last_status(ExitStatus::USAGE);
                    continue;
                }
                false
            }
            Ok(None) => {
                if pending.is_empty() {
                    // End of input with stopped jobs: the first time only warns.
                    if !shell.confirm_exit(&files) {
                        continue;
                    }
                    let status = shell.last_status();
                    return finish(&mut shell, status, &files);
                }
                true
            }
            Err(error) => {
                say(&format!("oh-my-shell: cannot read standard input: {error}"));
                return ExitStatus(1);
            }
        };

        match shell.run_source(&pending, at_end, &mut files) {
            LineResult::Incomplete => {}
            LineResult::Ran(Outcome::Continue(_)) => pending.clear(),
            LineResult::Ran(Outcome::Exit(status)) => return finish(&mut shell, status, &files),
            LineResult::Rejected(report) => {
                report::show(&report, style);
                pending.clear();
            }
        }
        if at_end {
            let status = shell.last_status();
            return finish(&mut shell, status, &files);
        }
    }
}

/// How many lines of problems with `config.toml` are shown before "... and N more".
const MAX_CONFIG_LINES: usize = 10;

/// Reads `config.toml` (writing the commented first-run file if there is none) and runs
/// `init.omsh`, for a shell that a person types at. Nothing here stops the shell: whatever is
/// wrong is shown as a line each and the defaults stay. `Err` carries the status of an `exit`
/// in `init.omsh`.
///
/// `-c`, scripts and piped input never get here (they are handed to `/bin/sh` first), and
/// neither does `--interpret-stdin`, which is not interactive.
fn start_up(shell: &mut Shell, files: &mut Io, options: Options) -> Result<Config, ExitStatus> {
    if !options.interactive || options.no_config {
        return Ok(Config::default());
    }
    // The shell's own variables, which include what the login profile exported.
    let paths = Paths::from_lookup(|name| {
        shell
            .var(name)
            .map(|value| OsString::from_vec(value.to_vec()))
    });
    let loaded = omsh_config::load(&paths);
    for line in loaded.report(MAX_CONFIG_LINES) {
        say(&format!("oh-my-shell: {line}"));
    }
    if let Some(init) = paths.init_file()
        && let Outcome::Exit(status) = shell.run_startup_file(&init, files)
    {
        return Err(status);
    }
    Ok(loaded.config)
}

/// The status of a shell that was hung up: 128 + `SIGHUP`.
const HANGUP_STATUS: u8 = 129;

/// The shell ends: its `EXIT` trap runs, then its jobs are dealt with (stopped ones are told to
/// hang up, since no one could continue them any more).
fn finish(shell: &mut Shell, status: ExitStatus, files: &Io) -> ExitStatus {
    let status = shell.run_exit_trap(status, files);
    shell.end_session();
    status
}

/// Adds `bytes` to `text` if they are valid UTF-8. The parser works on `str`.
fn append_utf8(text: &mut String, bytes: Vec<u8>) -> Result<(), std::string::FromUtf8Error> {
    text.push_str(&String::from_utf8(bytes)?);
    Ok(())
}

/// Reads up to and including the next line ending. Returns nothing at the end of the input
/// if no byte was read.
fn read_line(input: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    let mut byte = [0_u8; 1];
    loop {
        match input.read(&mut byte) {
            Ok(0) => return Ok((!line.is_empty()).then_some(line)),
            Ok(_) => {
                line.push(byte[0]);
                if byte[0] == b'\n' {
                    return Ok(Some(line));
                }
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

/// Writes the prompt to stderr, so that it does not mix with a command's output.
fn prompt(text: &str) {
    let mut err = io::stderr();
    let _ = write!(err, "{text}");
    let _ = err.flush();
}

/// Writes one line to stderr. A failed write is ignored: there is nowhere left to say so.
fn say(text: &str) {
    let _ = writeln!(io::stderr(), "{text}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_ends_after_its_line_ending() {
        let mut input: &[u8] = b"one\ntwo\n";
        assert_eq!(read_line(&mut input).unwrap(), Some(b"one\n".to_vec()));
        assert_eq!(read_line(&mut input).unwrap(), Some(b"two\n".to_vec()));
        assert_eq!(read_line(&mut input).unwrap(), None);
    }

    #[test]
    fn the_last_line_may_have_no_line_ending() {
        let mut input: &[u8] = b"one\ntwo";
        read_line(&mut input).unwrap();
        assert_eq!(read_line(&mut input).unwrap(), Some(b"two".to_vec()));
        assert_eq!(read_line(&mut input).unwrap(), None);
    }

    #[test]
    fn nothing_beyond_the_line_is_consumed() {
        let mut input: &[u8] = b"one\ntwo\n";
        read_line(&mut input).unwrap();
        assert_eq!(input, b"two\n");
    }

    #[test]
    fn bytes_that_are_not_utf8_are_refused() {
        let mut text = String::new();
        assert!(append_utf8(&mut text, vec![0xff, b'\n']).is_err());
        assert!(text.is_empty());
    }
}
