//! Does the terminal understand synchronized output (DEC private mode 2026)?
//!
//! A redraw wrapped in `ESC [ ? 2026 h` ... `ESC [ ? 2026 l` is shown all at once, which removes
//! the flicker of a menu that erases and draws again. Terminals that do not know the mode ignore
//! the two sequences, so writing them is harmless; the question is only whether to bother.
//!
//! The terminal is asked with DECRQM (`ESC [ ? 2026 $ p`) followed by a cursor-position request
//! (`ESC [ 6 n`). Every terminal answers the second, in order, so a terminal that does not know
//! the first is found out as soon as its cursor report arrives, not after a time limit. A
//! terminal that answers neither costs [`LIMIT`] once and is taken as "no". The answer is kept
//! for the life of the process ([`supported`]). **We never allow-list a terminal by name**:
//! sources disagree about some of them, and only the answer counts.
//!
//! The reply is `ESC [ ? 2026 ; Ps $ y`. `Ps` 1 (set), 2 (reset) and 3 (permanently set) mean the
//! mode exists; 0 (not recognized) and 4 (permanently reset: it cannot be turned on) mean it
//! cannot be used.
//!
//! Like the cursor probe of `omsh-editor`, the query puts the terminal in raw mode for a moment
//! and consumes what the terminal sends back. Keys typed during that moment are lost, so nothing
//! is asked when input is already waiting. This must only be called lazily (when a menu is
//! about to open), never at start-up.

use std::io::{self, Write};
use std::os::fd::{AsFd, BorrowedFd};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use nix::poll::{PollFd, PollFlags, PollTimeout, poll};

/// How long a terminal that answers nothing is waited for.
pub const LIMIT: Duration = Duration::from_millis(150);

/// What is written: the mode question, then the cursor question as the sentinel.
pub const QUESTION: &[u8] = b"\x1b[?2026$p\x1b[6n";

/// Start and end of a synchronized update.
pub const BEGIN: &str = "\x1b[?2026h";
/// End of a synchronized update.
pub const END: &str = "\x1b[?2026l";

/// What the terminal said.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Support {
    /// The mode exists and can be set.
    Yes,
    /// The terminal answered, and the mode is unknown to it or cannot be set.
    No,
    /// The terminal answered nothing within the limit.
    Silent,
    /// Nothing was asked: not a terminal, or keys were waiting on the input.
    NotAsked,
}

/// What a question found out, with the evidence for a person to read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Probe {
    /// The verdict.
    pub support: Support,
    /// `Ps` of the reply, when there was one.
    pub mode: Option<u8>,
    /// Every byte the terminal sent back.
    pub reply: Vec<u8>,
    /// How long the question took.
    pub elapsed: Duration,
}

/// The `Ps` of a DECRQM reply for mode 2026 anywhere in `bytes`.
#[must_use]
pub fn decrqm_reply(bytes: &[u8]) -> Option<u8> {
    const HEAD: &[u8] = b"\x1b[?2026;";
    let at = bytes
        .windows(HEAD.len())
        .position(|window| window == HEAD)?;
    let rest = &bytes[at + HEAD.len()..];
    let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 || !rest[digits..].starts_with(b"$y") {
        return None;
    }
    std::str::from_utf8(&rest[..digits]).ok()?.parse().ok()
}

/// True if `bytes` hold a complete cursor report, `ESC [ row ; column R`.
#[must_use]
pub fn has_cursor_report(bytes: &[u8]) -> bool {
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

/// The verdict the bytes heard so far allow, or `None` while more may come. The mode reply
/// decides when there is one; a cursor report without it means the terminal does not know the
/// mode (it answered the later question, so it would have answered the earlier one).
#[must_use]
pub fn verdict(heard: &[u8]) -> Option<Support> {
    if let Some(mode) = decrqm_reply(heard) {
        return Some(if matches!(mode, 1..=3) {
            Support::Yes
        } else {
            Support::No
        });
    }
    has_cursor_report(heard).then_some(Support::No)
}

/// Asks: writes [`QUESTION`] to `output` and reads from `input` until there is a verdict or
/// `limit` has passed. `input` must be in raw mode (or a pipe). Nothing is asked when input is
/// already waiting.
///
/// # Errors
///
/// When writing the question fails.
pub fn exchange(
    input: BorrowedFd<'_>,
    output: &mut dyn Write,
    limit: Duration,
) -> io::Result<Probe> {
    let start = Instant::now();
    let finish = |support, heard: Vec<u8>| Probe {
        support,
        mode: decrqm_reply(&heard),
        reply: heard,
        elapsed: start.elapsed(),
    };
    if ready(input, Duration::ZERO) {
        return Ok(finish(Support::NotAsked, Vec::new()));
    }
    output.write_all(QUESTION)?;
    output.flush()?;
    let end = start + limit;
    let mut heard = Vec::new();
    loop {
        let left = end.saturating_duration_since(Instant::now());
        if !ready(input, left) {
            return Ok(finish(Support::Silent, heard));
        }
        let mut chunk = [0u8; 64];
        match nix::unistd::read(input, &mut chunk) {
            // The input ended, or a read failed: nobody is answering.
            Ok(0) | Err(_) => return Ok(finish(Support::Silent, heard)),
            Ok(n) => heard.extend_from_slice(&chunk[..n]),
        }
        if let Some(support) = verdict(&heard) {
            return Ok(finish(support, heard));
        }
        // Endless noise must not hold the shell.
        if heard.len() > 4096 {
            return Ok(finish(Support::Silent, heard));
        }
    }
}

fn ready(input: BorrowedFd<'_>, limit: Duration) -> bool {
    let mut fds = [PollFd::new(input, PollFlags::POLLIN)];
    // `poll` counts whole milliseconds and `try_from` cuts the rest: round up, so that a limit is
    // never cut short.
    let rounded = limit.saturating_add(Duration::from_micros(999));
    let timeout = PollTimeout::try_from(rounded).unwrap_or(PollTimeout::MAX);
    matches!(poll(&mut fds, timeout), Ok(n) if n > 0)
}

/// Raw mode for the length of a question, switched back only if this switched it on (a menu
/// that is already open has it on, and must keep it).
struct RawGuard {
    switched_on: bool,
}

impl RawGuard {
    fn new() -> Option<Self> {
        if crossterm::terminal::is_raw_mode_enabled().unwrap_or(false) {
            return Some(Self { switched_on: false });
        }
        crossterm::terminal::enable_raw_mode().ok()?;
        Some(Self { switched_on: true })
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        if self.switched_on {
            let _ = crossterm::terminal::disable_raw_mode();
        }
    }
}

/// Asks the real terminal (standard input and standard output). [`Support::NotAsked`] when
/// either is not a terminal, when raw mode cannot be switched on, or when keys are waiting.
#[must_use]
pub fn query(limit: Duration) -> Probe {
    use std::io::IsTerminal;
    let not_asked = || Probe {
        support: Support::NotAsked,
        mode: None,
        reply: Vec::new(),
        elapsed: Duration::ZERO,
    };
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    if !stdin.is_terminal() || !stdout.is_terminal() {
        return not_asked();
    }
    let Some(_raw) = RawGuard::new() else {
        return not_asked();
    };
    exchange(stdin.as_fd(), &mut stdout, limit).unwrap_or_else(|_| not_asked())
}

static ANSWER: OnceLock<bool> = OnceLock::new();

/// Whether to wrap redraws in synchronized-output sequences. The terminal is asked once ([`LIMIT`]
/// at most) and the answer is kept. A question that was not asked ([`Support::NotAsked`]) gives
/// false and is not kept, so a later call may ask.
#[must_use]
pub fn supported() -> bool {
    if let Some(answer) = ANSWER.get() {
        return *answer;
    }
    let probe = query(LIMIT);
    if probe.support == Support::NotAsked {
        return false;
    }
    let answer = probe.support == Support::Yes;
    let _ = ANSWER.set(answer);
    answer
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::os::fd::OwnedFd;
    use std::thread;

    #[test]
    fn the_reply_is_found_with_its_mode_number() {
        for (bytes, mode) in [
            (&b"\x1b[?2026;1$y"[..], 1),
            (b"\x1b[?2026;2$y", 2),
            (b"\x1b[?2026;0$y", 0),
            (b"\x1b[?2026;4$y\x1b[1;1R", 4),
            (b"noise\x1b[?2026;3$ytail", 3),
        ] {
            assert_eq!(decrqm_reply(bytes), Some(mode), "{bytes:?}");
        }
    }

    #[test]
    fn other_sequences_are_not_the_reply() {
        for bytes in [
            &b""[..],
            b"\x1b[?2026;",
            b"\x1b[?2026;$y",
            b"\x1b[?2026;1",
            b"\x1b[?2026;1$",
            b"\x1b[?2004;1$y",
            b"\x1b[2026;1$y",
            b"\x1b[12;40R",
            b"\x1b[?2026;999$y",
        ] {
            assert_eq!(decrqm_reply(bytes), None, "{bytes:?}");
        }
    }

    #[test]
    fn the_cursor_report_is_the_sentinel() {
        assert!(has_cursor_report(b"\x1b[12;40R"));
        assert!(has_cursor_report(b"x\x1b[?2026;2$y\x1b[1;1R"));
        for bytes in [
            &b"\x1b[12;40"[..],
            b"\x1b[;1R",
            b"\x1b[?12;40R",
            b"12;40R",
            b"",
        ] {
            assert!(!has_cursor_report(bytes), "{bytes:?}");
        }
    }

    #[test]
    fn the_verdict_follows_the_mode_number_and_a_bare_cursor_report_means_no() {
        assert_eq!(verdict(b"\x1b[?2026;1$y\x1b[1;1R"), Some(Support::Yes));
        assert_eq!(
            verdict(b"\x1b[?2026;2$y"),
            Some(Support::Yes),
            "no need to wait for the sentinel"
        );
        assert_eq!(verdict(b"\x1b[?2026;3$y"), Some(Support::Yes));
        assert_eq!(verdict(b"\x1b[?2026;0$y\x1b[1;1R"), Some(Support::No));
        assert_eq!(
            verdict(b"\x1b[?2026;4$y\x1b[1;1R"),
            Some(Support::No),
            "cannot be set"
        );
        assert_eq!(verdict(b"\x1b[5;9R"), Some(Support::No));
        assert_eq!(verdict(b"\x1b[?2026"), None, "more may come");
        assert_eq!(verdict(b""), None);
    }

    /// A fake terminal on two pipes: it waits for our question, then sends `reply`.
    struct FakeTerminal {
        /// What we read (the terminal's answers).
        input: OwnedFd,
        /// What we write (our questions).
        output: File,
        thread: thread::JoinHandle<Vec<u8>>,
    }

    fn fake_terminal(reply: &'static [u8], delay: Duration, answer_at_all: bool) -> FakeTerminal {
        let (answers_read, answers_write) = nix::unistd::pipe().unwrap();
        let (questions_read, questions_write) = nix::unistd::pipe().unwrap();
        let thread = thread::spawn(move || {
            use std::io::Read;
            let mut questions = File::from(questions_read);
            let mut asked = vec![0u8; QUESTION.len()];
            questions.read_exact(&mut asked).unwrap();
            if answer_at_all {
                thread::sleep(delay);
                let mut answers = File::from(answers_write);
                answers.write_all(reply).unwrap();
                // Keep the pipe open a little: a real terminal does not hang up.
                thread::sleep(Duration::from_millis(300));
            } else {
                thread::sleep(Duration::from_millis(300));
                drop(answers_write);
            }
            asked
        });
        FakeTerminal {
            input: answers_read,
            output: File::from(questions_write),
            thread,
        }
    }

    fn ask(reply: &'static [u8], delay: Duration, limit: Duration) -> (Probe, Vec<u8>) {
        let mut fake = fake_terminal(reply, delay, true);
        let probe = exchange(fake.input.as_fd(), &mut fake.output, limit).unwrap();
        (probe, fake.thread.join().unwrap())
    }

    #[test]
    fn a_terminal_that_knows_the_mode_is_found_out_and_was_asked_both_questions() {
        let (probe, asked) = ask(
            b"\x1b[?2026;2$y\x1b[1;1R",
            Duration::ZERO,
            Duration::from_secs(2),
        );
        assert_eq!(probe.support, Support::Yes);
        assert_eq!(probe.mode, Some(2));
        assert_eq!(asked, QUESTION);
    }

    #[test]
    fn a_terminal_that_does_not_know_it_is_found_out_by_the_cursor_report_not_by_the_limit() {
        let start = Instant::now();
        let (probe, _) = ask(
            b"\x1b[24;1R",
            Duration::from_millis(5),
            Duration::from_secs(5),
        );
        assert_eq!(probe.support, Support::No);
        assert_eq!(probe.mode, None);
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "no waiting for the limit"
        );
        let (zero, _) = ask(
            b"\x1b[?2026;0$y\x1b[24;1R",
            Duration::ZERO,
            Duration::from_secs(2),
        );
        assert_eq!((zero.support, zero.mode), (Support::No, Some(0)));
        let (fixed, _) = ask(
            b"\x1b[?2026;4$y\x1b[24;1R",
            Duration::ZERO,
            Duration::from_secs(2),
        );
        assert_eq!((fixed.support, fixed.mode), (Support::No, Some(4)));
    }

    #[test]
    fn half_a_reply_is_no_reply() {
        // Only the start of a reply arrives: that is not a verdict.
        let (probe, _) = ask(b"\x1b[?2026;", Duration::ZERO, Duration::from_millis(100));
        assert_eq!(probe.support, Support::Silent, "half a reply is no reply");
        assert_eq!(probe.reply, b"\x1b[?2026;");
    }

    #[test]
    fn a_terminal_that_says_nothing_costs_the_limit_once() {
        let mut fake = fake_terminal(b"", Duration::ZERO, false);
        let limit = Duration::from_millis(60);
        let probe = exchange(fake.input.as_fd(), &mut fake.output, limit).unwrap();
        assert_eq!(probe.support, Support::Silent);
        assert!(
            probe.elapsed >= limit && probe.elapsed < Duration::from_secs(2),
            "{:?}",
            probe.elapsed
        );
        fake.thread.join().unwrap();
    }

    #[test]
    fn nothing_is_asked_when_keys_are_already_waiting() {
        let (read, write) = nix::unistd::pipe().unwrap();
        File::from(write).write_all(b"ls\n").unwrap();
        let mut sink: Vec<u8> = Vec::new();
        let probe = exchange(read.as_fd(), &mut sink, Duration::from_secs(1)).unwrap();
        assert_eq!(probe.support, Support::NotAsked);
        assert!(
            sink.is_empty(),
            "the typed-ahead keys were not touched and nothing was written"
        );
        // The keys are still there.
        let mut left = [0u8; 8];
        assert_eq!(nix::unistd::read(read.as_fd(), &mut left).unwrap(), 3);
    }

    #[test]
    fn endless_noise_is_given_up_on() {
        let (read, write) = nix::unistd::pipe().unwrap();
        let writer = thread::spawn(move || {
            let mut write = File::from(write);
            for _ in 0..200 {
                if write.write_all(&[b'x'; 64]).is_err() {
                    break;
                }
            }
        });
        let mut sink: Vec<u8> = Vec::new();
        let probe = exchange(read.as_fd(), &mut sink, Duration::from_secs(5)).unwrap();
        assert_eq!(probe.support, Support::Silent);
        drop(read);
        writer.join().unwrap();
    }

    #[test]
    fn when_not_run_on_a_terminal_nothing_is_asked_and_the_answer_is_false() {
        // `cargo test` has no terminal on standard input.
        assert_eq!(query(Duration::from_millis(10)).support, Support::NotAsked);
        assert!(!supported());
    }
}
