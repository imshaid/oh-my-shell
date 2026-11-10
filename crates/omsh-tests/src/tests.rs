//! Tests of the harness itself. They run `cat` and `sh`, not the shell: if the harness could
//! hang or leave processes behind, nothing built on it could be trusted.

use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use nix::sys::termios::LocalFlags;

use crate::procfs::live_members;
use crate::session::SHORT;
use crate::watchdog::{pty_test, pty_test_within};

fn message(panic: &Box<dyn Any + Send>) -> String {
    panic
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap_or_default()
}

#[test]
fn expecting_text_that_never_comes_panics_at_the_limit() {
    pty_test("/bin/cat", |ctx| {
        let mut term = ctx.launch().start_raw()?;
        let start = Instant::now();
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            term.expect("never shown", Duration::from_millis(300));
        }));
        let panic = outcome.expect_err("expect should have panicked");
        assert!(message(&panic).contains("never saw \"never shown\""));
        assert!(start.elapsed() < Duration::from_secs(5));
        Ok(())
    });
}

#[test]
fn typed_text_is_echoed_and_cat_ends_at_end_of_input() -> std::io::Result<()> {
    pty_test("/bin/cat", |ctx| {
        let mut term = ctx.launch().start_raw()?;
        term.line("hello-from-the-test")?;
        // The terminal echoes the line, and cat prints it again.
        term.expect("hello-from-the-test", SHORT);
        term.expect("hello-from-the-test", SHORT);
        term.key(4)?; // Ctrl+D
        assert_eq!(term.wait_exit(SHORT).code(), Some(0));
        Ok(())
    });
    Ok(())
}

#[test]
fn the_exit_status_is_reported() {
    pty_test("/bin/sh", |ctx| {
        let mut term = ctx.launch().arg("-c").arg("exit 7").start_raw()?;
        assert_eq!(term.wait_exit(SHORT).code(), Some(7));
        Ok(())
    });
}

#[test]
fn a_body_that_never_ends_fails_at_the_limit_and_its_processes_are_killed() {
    let session = Arc::new(AtomicU32::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let (session_in, stop_in) = (Arc::clone(&session), Arc::clone(&stop));
    let start = Instant::now();
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        pty_test_within("/bin/sh", Duration::from_secs(1), move |ctx| {
            let term = ctx.launch().arg("-c").arg("sleep 100").start_raw()?;
            session_in.store(term.parent_pid(), Ordering::SeqCst);
            while !stop_in.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(10));
            }
            Ok(())
        });
    }));
    let panic = outcome.expect_err("the test should have failed");
    assert!(message(&panic).contains("did not finish in"), "{panic:?}");
    assert!(start.elapsed() < Duration::from_secs(20));
    let id = session.load(Ordering::SeqCst);
    assert_ne!(id, 0, "the body never started its program");
    assert!(live_members(id).is_empty(), "{:?}", live_members(id));
    // Let the leaked thread end.
    stop.store(true, Ordering::SeqCst);
}

#[test]
fn a_panicking_body_leaves_nothing_behind() {
    let session = Arc::new(AtomicU32::new(0));
    let session_in = Arc::clone(&session);
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        pty_test("/bin/sh", move |ctx| {
            let term = ctx.launch().arg("-c").arg("sleep 100").start_raw()?;
            session_in.store(term.parent_pid(), Ordering::SeqCst);
            panic!("boom");
        });
    }));
    let panic = outcome.expect_err("the test should have failed");
    assert_eq!(message(&panic), "boom");
    let id = session.load(Ordering::SeqCst);
    assert_ne!(id, 0);
    assert!(live_members(id).is_empty(), "{:?}", live_members(id));
}

#[test]
fn a_body_that_leaves_a_process_behind_is_caught() {
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        pty_test("/bin/sh", |ctx| {
            let term = ctx.launch().arg("-c").arg("sleep 100").start_raw()?;
            // Dropping would kill it; forgetting the session keeps the process alive.
            std::mem::forget(term);
            Ok(())
        });
    }));
    let panic = outcome.expect_err("the process should have been reported");
    assert!(
        message(&panic).contains("processes were left behind"),
        "{panic:?}"
    );
}

#[test]
fn typing_at_a_program_that_does_not_read_cannot_block_for_ever() {
    pty_test("/bin/sh", |ctx| {
        let mut term = ctx.launch().arg("-c").arg("sleep 100").start_raw()?;
        let start = Instant::now();
        // A megabyte is far more than the terminal buffers. Either the kernel drops what does
        // not fit or the write waits; in both cases the call ends, by itself or by its panic.
        let lines = "some text that nobody reads\n".repeat(40_000);
        let _ = catch_unwind(AssertUnwindSafe(|| term.write_bytes(lines.as_bytes())));
        assert!(start.elapsed() < SHORT * 2, "{:?}", start.elapsed());
        Ok(())
    });
}

#[test]
fn the_settings_of_the_terminal_can_be_read() {
    pty_test("/bin/sh", |ctx| {
        let mut term = ctx
            .launch()
            .arg("-c")
            .arg("stty -echo; sleep 100")
            .start_raw()?;
        let end = Instant::now() + SHORT;
        while term.termios()?.local_flags.contains(LocalFlags::ECHO) {
            assert!(Instant::now() < end, "echo was never turned off");
            term.pump(Duration::from_millis(20));
        }
        Ok(())
    });
}

#[test]
fn escape_sequences_are_stripped_and_positions_kept() {
    use crate::session::strip_escapes;
    let text = "a\x1b[31mb\x1b[0m \x1b7c\x1b[1;78H\x1b8é";
    let (plain, origin) = strip_escapes(text);
    assert_eq!(plain, "ab cé");
    assert_eq!(plain.len(), origin.len());
    // Each kept byte points back at itself in the original.
    for (i, &at) in origin.iter().enumerate() {
        assert_eq!(text.as_bytes()[at], plain.as_bytes()[i]);
    }
    assert_eq!(strip_escapes("no escapes").0, "no escapes");
    // A sequence cut off at the end of the output does not panic.
    assert_eq!(strip_escapes("x\x1b[3").0, "x");
    assert_eq!(strip_escapes("x\x1b").0, "x");
}

/// A program that prints `$ `, then for each line it reads waits 0.4 s, prints a result line and
/// prints `$ ` again, as a shell does.
const SLOW_PROMPTER: &str =
    "printf '$ '; while read l; do sleep 0.4; printf 'result-%s\\n$ ' \"$l\"; done";

#[test]
fn prompt_returns_at_once_at_the_first_prompt_and_waits_for_the_next_one() {
    pty_test("/bin/sh", |ctx| {
        let mut term = ctx.launch().arg("-c").arg(SLOW_PROMPTER).start_raw()?;
        term.prompt();
        term.line("one")?;
        let typed = Instant::now();
        term.prompt();
        assert!(
            typed.elapsed() >= Duration::from_millis(300),
            "prompt() came back before the command ended: {:?}",
            typed.elapsed()
        );
        assert!(term.transcript().ends_with("result-one\r\n$ "));
        // The line that is typed to a command waits for the prompt too.
        term.command("two")?;
        term.command("three")?;
        term.prompt();
        for line in ["result-one", "result-two", "result-three"] {
            assert!(term.transcript().contains(line), "{}", term.transcript());
        }
        term.key(4)?;
        Ok(())
    });
}

#[test]
fn prompt_panics_at_the_limit_when_no_prompt_comes() {
    pty_test("/bin/sh", |ctx| {
        let mut term = ctx.launch().arg("-c").arg("cat").start_raw()?;
        term.line("typed-to-cat")?;
        let outcome = catch_unwind(AssertUnwindSafe(|| term.prompt()));
        let panic = outcome.expect_err("prompt should have panicked");
        assert!(message(&panic).contains("never saw the prompt"));
        Ok(())
    });
}
