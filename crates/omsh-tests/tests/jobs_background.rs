//! Background jobs (S09a) on a real terminal.

use std::time::{Duration, Instant};

use omsh_tests::{SHORT, pty_test};

const BIN: &str = env!("CARGO_BIN_EXE_omsh-pty-shell");

/// `cat &` reads the terminal from the background and is stopped by `SIGTTIN`. The shell notes
/// it as stopped, and `kill %1` ends it although a stopped process would hold the `SIGTERM`.
#[test]
fn kill_ends_a_background_job_that_the_terminal_stopped() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.line("cat &")?;
        term.expect("[1] ", SHORT);

        // The notice before a prompt, and `jobs`, say that it stopped for the terminal.
        term.repeat_until("jobs", "Stopped (tty input)")?;
        assert_eq!(term.in_session("cat").len(), 1, "{}", term.transcript());

        term.line("kill %1")?;
        // The job's thread sees `cat` die and the next prompt tells the user.
        term.repeat_until("echo gone-$((1+1))", "Terminated")?;

        // Nothing is left: not the process, and not a zombie waiting to be collected.
        term.expect_none("cat");
        term.quit()
    });
}

/// Ctrl+C goes to the foreground job. A background program and the shell code of a background
/// job (a loop run on a thread of the shell) must not feel it.
#[test]
fn ctrl_c_stops_the_foreground_job_and_not_the_background_ones() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.line("sleep 30 &")?;
        term.expect("[1] ", SHORT);
        term.line("while :; do :; done &")?;
        term.expect("[2]", SHORT);

        term.line("sleep 20")?;
        // Give `sleep 20` time to start, then press Ctrl+C.
        term.pump(Duration::from_millis(400));
        term.key(3)?;
        // The shell is alive and ready for the next command... (The loop above checks the Ctrl+C
        // flag thousands of times a millisecond, so it would see it in the short moment it is
        // set.)
        term.sync("alive")?;
        // ... and both background jobs are still running, also after the loop had time to notice
        // a flag that it should not have seen.
        term.pump(Duration::from_millis(400));
        term.line("jobs")?;
        term.expect("sleep 30 &", SHORT);
        term.expect("while :; do :; done &", SHORT);
        assert!(
            !term.recent(300).contains("Done"),
            "a background job ended\n{}",
            term.transcript()
        );
        assert!(
            !term.transcript().contains("Exit 130"),
            "a background job was interrupted\n{}",
            term.transcript()
        );

        // A background job that dies of `SIGINT` sent by `kill` is not a Ctrl+C: the commands
        // after it on the same line still run. (If its death set the Ctrl+C flag, `echo` would
        // be skipped.)
        term.line("kill -INT %1; wait %1; echo after-$((1+1))")?;
        term.expect("after-2", SHORT);

        // The terminal is still owned by the shell: `kill` and `wait` work.
        term.line("kill %2")?;
        term.line("wait")?;
        term.sync("waited")?;
        term.quit()
    });
}

/// The notice for a finished job comes once, before a prompt, with the layout of bash.
#[test]
fn a_finished_job_is_announced_once() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.line("sh -c 'exit 3' &")?;
        term.expect("[1] ", SHORT);
        term.repeat_until("echo tick-$((1+1))", "Exit 3")?;
        let shown = term.transcript().matches("Exit 3").count();
        let end = Instant::now() + Duration::from_millis(300);
        while Instant::now() < end {
            term.sync("more")?;
        }
        assert_eq!(
            term.transcript().matches("Exit 3").count(),
            shown,
            "{}",
            term.transcript()
        );
        term.quit()
    });
}
