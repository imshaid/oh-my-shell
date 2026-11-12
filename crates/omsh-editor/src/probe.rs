//! Does the terminal answer "where is the cursor?" (`ESC [ 6 n`)?
//!
//! The editor asks that question at every prompt and waits two seconds for each answer it does
//! not get (that wait lives inside `crossterm`, and `reedline` has no switch to skip the
//! question). A terminal that never answers would make every prompt take 2 to 4 seconds. So the
//! shell asks once, before it builds the editor, with a short limit: no answer means the plain
//! line reader is used instead, and the person is told once.

use std::io::Write;
use std::os::fd::AsFd;
use std::time::{Duration, Instant};

use nix::poll::{PollFd, PollFlags, PollTimeout, poll};

/// True if the terminal reports its cursor within `limit`, or if that cannot be tested.
///
/// Raw mode is switched on for the question (the terminal would otherwise echo the answer as
/// text) and always switched off again. Bytes that are already waiting on the input mean the
/// person typed ahead: they must not be swallowed, and a terminal that answers is assumed. Keys
/// typed during the question itself are lost, which at most costs a few milliseconds of typing
/// on a terminal that answers at once.
#[must_use]
pub fn terminal_reports_cursor(limit: Duration) -> bool {
    let stdin = std::io::stdin();
    if ready(&stdin, Duration::ZERO) {
        return true;
    }
    if crossterm::terminal::enable_raw_mode().is_err() {
        return true;
    }
    let answered = ask(&stdin, limit);
    let _ = crossterm::terminal::disable_raw_mode();
    answered
}

fn ask(stdin: &std::io::Stdin, limit: Duration) -> bool {
    let mut out = std::io::stdout();
    if out
        .write_all(b"\x1b[6n")
        .and_then(|()| out.flush())
        .is_err()
    {
        // Cannot even write to the terminal: the plain reader has the same problem, so do not
        // call that a missing answer.
        return true;
    }
    let end = Instant::now() + limit;
    let mut heard = Vec::new();
    loop {
        let left = end.saturating_duration_since(Instant::now());
        if !ready(stdin, left) {
            return false;
        }
        let mut chunk = [0u8; 64];
        match nix::unistd::read(stdin.as_fd(), &mut chunk) {
            // End of input or a failed read is not a terminal that is silent.
            Ok(0) | Err(_) => return true,
            Ok(n) => heard.extend_from_slice(&chunk[..n]),
        }
        if has_cursor_reply(&heard) {
            return true;
        }
        // Not an answer. A terminal that sends endless noise must not hold the shell.
        if heard.len() > 4096 {
            return false;
        }
    }
}

/// Waits up to `limit` for input on `stdin`.
fn ready(stdin: &std::io::Stdin, limit: Duration) -> bool {
    let mut fds = [PollFd::new(stdin.as_fd(), PollFlags::POLLIN)];
    let timeout = PollTimeout::try_from(limit).unwrap_or(PollTimeout::MAX);
    matches!(poll(&mut fds, timeout), Ok(n) if n > 0)
}

/// True if `bytes` hold a complete cursor report: `ESC [ row ; column R`, with digits only.
pub(crate) fn has_cursor_reply(bytes: &[u8]) -> bool {
    bytes.windows(2).enumerate().any(|(at, pair)| {
        pair == b"\x1b[" && {
            let rest = &bytes[at + 2..];
            let rows = rest.iter().take_while(|b| b.is_ascii_digit()).count();
            let after = &rest[rows..];
            rows > 0 && after.first() == Some(&b';') && {
                let after = &after[1..];
                let columns = after.iter().take_while(|b| b.is_ascii_digit()).count();
                columns > 0 && after.get(columns) == Some(&b'R')
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::has_cursor_reply;

    #[test]
    fn a_complete_report_is_found_anywhere_in_the_bytes() {
        assert!(has_cursor_reply(b"\x1b[12;40R"));
        assert!(has_cursor_reply(b"\x1b[1;1R"));
        assert!(has_cursor_reply(b"typed\x1b[2;3Rmore"));
        // A report after another escape sequence (a focus event, a mouse report).
        assert!(has_cursor_reply(b"\x1b[I\x1b[24;80R"));
    }

    #[test]
    fn partial_or_other_sequences_are_not_a_report() {
        for bytes in [
            &b""[..],
            b"\x1b",
            b"\x1b[",
            b"\x1b[12",
            b"\x1b[12;",
            b"\x1b[12;40",
            b"\x1b[;40R",
            b"\x1b[12;R",
            b"\x1b[12R",
            b"\x1b[?12;40R",
            b"\x1b[12;4x0R",
            b"12;40R",
            b"\x1b[A",
            b"\x1b[1;5A",
        ] {
            assert!(!has_cursor_reply(bytes), "{bytes:?}");
        }
    }

    #[test]
    fn a_report_that_arrives_in_pieces_is_found_once_whole() {
        let mut heard = Vec::new();
        for piece in [&b"\x1b"[..], b"[1", b"2;4", b"0", b"R"] {
            assert!(!has_cursor_reply(&heard));
            heard.extend_from_slice(piece);
        }
        assert!(has_cursor_reply(&heard));
    }
}
