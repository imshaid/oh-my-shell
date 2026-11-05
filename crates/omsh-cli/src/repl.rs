//! The read-run loop.
//!
//! This is a plain line reader for now: no editing, history or completion (S13 adds the
//! editor). It reads standard input and runs what it finds.

use std::fs::File;
use std::io::{self, ErrorKind, IsTerminal, Read, Write};
use std::os::fd::AsFd;

use omsh_exec::{ExitStatus, LineResult, Outcome, Shell, ignore_interrupts};

use crate::report::{self, Style};

const PROMPT: &str = "$ ";
const CONTINUATION_PROMPT: &str = "> ";

/// Runs the shell on standard input until it ends, and returns the exit status.
pub fn run() -> ExitStatus {
    let interactive = io::stdin().is_terminal();
    if interactive {
        ignore_interrupts();
    }
    let input = match stdin_without_buffer() {
        Ok(file) => file,
        Err(error) => {
            say(&format!("oh-my-shell: cannot read standard input: {error}"));
            return ExitStatus(1);
        }
    };
    run_on(input, interactive, Style::detect())
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

fn run_on(mut input: impl Read, interactive: bool, style: Style) -> ExitStatus {
    let mut shell = Shell::new();
    let mut err = io::stderr();
    // The lines read so far for the command being typed: more than one when it is unfinished.
    let mut pending = String::new();
    loop {
        if interactive {
            prompt(if pending.is_empty() {
                PROMPT
            } else {
                CONTINUATION_PROMPT
            });
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
                    return shell.last_status();
                }
                true
            }
            Err(error) => {
                say(&format!("oh-my-shell: cannot read standard input: {error}"));
                return ExitStatus(1);
            }
        };

        match shell.run_source(&pending, at_end, &mut err) {
            LineResult::Incomplete => {}
            LineResult::Ran(Outcome::Continue(_)) => pending.clear(),
            LineResult::Ran(Outcome::Exit(status)) => return status,
            LineResult::Rejected(report) => {
                report::show(&report, style);
                pending.clear();
            }
        }
        if at_end {
            return shell.last_status();
        }
    }
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
