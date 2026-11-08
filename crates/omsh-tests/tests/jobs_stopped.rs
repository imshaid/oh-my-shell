//! Stopped jobs (S09b) on a real terminal: Ctrl+Z, `fg`, `bg`, the warning at exit, `SIGHUP`.

use std::time::{Duration, Instant};

use nix::sys::signal::Signal;
use omsh_tests::procfs::{foreground_group, hup_is_ignored, process_group, state};
use omsh_tests::{SHORT, pty_test};

const BIN: &str = env!("CARGO_BIN_EXE_omsh-pty-shell");

const CTRL_Z: u8 = 0x1a;

/// Ctrl+Z stops the foreground job and the shell gets the terminal back; `fg` gives it to the
/// job again; `bg` lets the job run without it.
#[test]
fn ctrl_z_stops_the_job_and_fg_and_bg_continue_it() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        let shell = term.shell_pid();
        term.line("sleep 30")?;
        term.pump(Duration::from_millis(400));
        let sleep = term.in_session("sleep");
        assert_eq!(sleep.len(), 1, "{}", term.transcript());
        let sleep = sleep[0];
        // While it runs, the job owns the terminal.
        assert_eq!(foreground_group(shell), sleep.as_raw());

        term.key(CTRL_Z)?;
        term.expect("Stopped", SHORT);
        term.expect("sleep 30", SHORT);
        term.expect("$ ", SHORT);
        // The shell is back in front, and the program is still there, stopped.
        term.sync("alive")?;
        assert_eq!(foreground_group(shell), process_group(shell));
        assert_eq!(state(sleep), "T");
        // The notice came once, with the Ctrl+Z; the next prompt did not repeat it.
        assert_eq!(
            term.transcript().matches("Stopped").count(),
            1,
            "{}",
            term.transcript()
        );
        term.line("jobs")?;
        term.expect("Stopped", SHORT);

        // `fg`: the job runs, in front.
        term.line("fg")?;
        term.expect("sleep 30", SHORT);
        term.pump(Duration::from_millis(300));
        assert_ne!(state(sleep), "T");
        assert_eq!(foreground_group(shell), sleep.as_raw());

        // It can be stopped a second time, and `bg` makes it run in the background.
        term.key(CTRL_Z)?;
        term.expect("Stopped", SHORT);
        term.sync("again")?;
        term.line("bg")?;
        term.expect("[1]+ sleep 30 &", SHORT);
        term.sync("bg-done")?;
        assert_eq!(foreground_group(shell), process_group(shell));
        assert_ne!(state(sleep), "T");
        term.line("jobs")?;
        term.expect("Running", SHORT);

        term.line("kill %1")?;
        term.repeat_until("echo gone-$((1+1))", "Terminated")?;
        term.expect_none("sleep");
        term.quit()
    });
}

/// Ctrl+C reaches a job that was stopped and continued with `fg`, and only it.
#[test]
fn ctrl_c_ends_a_job_that_was_continued_with_fg() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.line("sleep 30")?;
        term.pump(Duration::from_millis(400));
        term.key(CTRL_Z)?;
        term.expect("Stopped", SHORT);
        term.sync("alive")?;
        // Ctrl+C also ends the rest of the line, as it does for a job that never stopped.
        term.line("fg; echo printed-$((1+1))")?;
        term.pump(Duration::from_millis(300));
        term.key(3)?;
        term.sync("after-int")?;
        assert!(
            !term.transcript().contains("printed-2"),
            "the line went on after Ctrl+C\n{}",
            term.transcript()
        );
        term.expect_none("sleep");
        term.line("jobs")?;
        term.sync("empty")?;
        assert!(!term.recent(40).contains("sleep"));
        term.quit()
    });
}

/// A pipeline and a subshell are stopped as one job, with every program in it.
#[test]
fn a_pipeline_and_a_subshell_stop_as_one_job() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.line("sleep 30 | cat")?;
        term.pump(Duration::from_millis(500));
        term.key(CTRL_Z)?;
        term.expect("Stopped", SHORT);
        term.sync("one")?;
        assert_eq!(term.in_session("sleep").len(), 1);
        assert_eq!(term.in_session("cat").len(), 1);
        let (sleep, cat) = (term.in_session("sleep")[0], term.in_session("cat")[0]);
        let end = Instant::now() + SHORT;
        while (state(sleep) != "T" || state(cat) != "T") && Instant::now() < end {
            term.pump(Duration::from_millis(50));
        }
        assert_eq!((state(sleep), state(cat)), ("T".to_owned(), "T".to_owned()));
        term.line("jobs")?;
        term.expect("sleep 30 | cat", SHORT);
        term.line("fg")?;
        term.pump(Duration::from_millis(300));
        term.key(3)?;
        term.sync("two")?;
        term.expect_none("sleep");
        term.expect_none("cat");

        // `( ... )` is one job too; `fg` goes on inside it.
        term.line("( sleep 30; echo subshell-finished )")?;
        term.pump(Duration::from_millis(500));
        term.key(CTRL_Z)?;
        term.expect("Stopped", SHORT);
        term.sync("three")?;
        term.line("jobs")?;
        term.expect("( sleep 30; echo subshell-finished )", SHORT);
        term.line("kill -CONT %1")?;
        term.line("kill %1")?;
        term.line("wait %1")?;
        term.sync("four")?;
        term.expect_none("sleep");
        term.quit()
    });
}

/// The terminal settings of a job are kept when it stops and given back by `fg`: the shell has
/// its own meanwhile.
#[test]
fn a_stopped_job_keeps_its_own_terminal_settings() {
    // Prints `flag=echo` or `flag=-echo`: the state of the echo flag of the terminal. (The marker
    // keeps it apart from the command lines, which the terminal shows too.)
    const ECHO_FLAG: &str =
        "echo flag=$(stty -a | tr -s \" ;\" \"\\n\" | grep -x -e echo -e -echo)";
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        // The job turns echo off, stops itself, and reports the flag when it is continued.
        term.line(&format!(
            "sh -c 'stty -echo; kill -STOP $$; {ECHO_FLAG}; sleep 0.2'"
        ))?;
        term.expect("Stopped", SHORT);
        term.expect("$ ", SHORT);
        // At the prompt the shell's own settings are back: echo is on.
        term.line(ECHO_FLAG)?;
        term.expect("flag=echo", SHORT);
        // `fg` gives the job its settings back: it sees echo off.
        term.line("fg")?;
        term.expect("flag=-echo", SHORT);
        term.sync("done")?;
        term.line("stty echo")?;
        term.quit()
    });
}

/// `exit` with a stopped job warns once; the second `exit` goes through and the stopped job is
/// told to hang up, so nothing is left behind.
#[test]
fn exit_warns_about_stopped_jobs_once_and_then_hangs_them_up() {
    if hup_is_ignored() {
        eprintln!("skipped: SIGHUP is ignored here (started under nohup?)");
        return;
    }
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.line("sleep 30")?;
        term.pump(Duration::from_millis(400));
        term.key(CTRL_Z)?;
        term.expect("Stopped", SHORT);
        term.sync("ready")?;

        term.line("exit")?;
        term.expect("There are stopped jobs.", SHORT);
        assert!(!term.has_exited()?, "the shell ended");
        assert_eq!(term.in_session("sleep").len(), 1);

        // Anything else in between brings the warning back.
        term.sync("between")?;
        term.line("exit")?;
        term.expect("There are stopped jobs.", SHORT);
        // End of input (Ctrl+D) at an empty prompt is the same attempt again.
        term.key(4)?;
        term.wait_exit(SHORT);
        term.expect_none("sleep");
        Ok(())
    });
}

/// `SIGHUP` sent to an idle shell ends it with status 129 and takes its jobs along, stopped or
/// not.
#[test]
fn sighup_ends_the_shell_and_its_jobs() {
    if hup_is_ignored() {
        eprintln!("skipped: SIGHUP is ignored here (started under nohup?)");
        return;
    }
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.line("sleep 100 &")?;
        term.expect("[1] ", SHORT);
        term.line("sleep 101")?;
        term.pump(Duration::from_millis(400));
        term.key(CTRL_Z)?;
        term.expect("Stopped", SHORT);
        term.sync("ready")?;
        assert_eq!(term.in_session("sleep").len(), 2);

        term.signal_parent(Signal::SIGHUP)?;
        let status = term.wait_exit(SHORT);
        assert_eq!(status.code(), Some(129), "{}", term.transcript());
        term.expect_none("sleep");
        Ok(())
    });
}

/// A job disowned with `-h` is left alone when the shell is hung up.
#[test]
fn disown_h_protects_a_job_from_the_hangup() {
    if hup_is_ignored() {
        eprintln!("skipped: SIGHUP is ignored here (started under nohup?)");
        return;
    }
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.line("sleep 100 &")?;
        term.expect("[1] ", SHORT);
        term.line("disown -h")?;
        term.sync("ready")?;
        term.signal_parent(Signal::SIGHUP)?;
        term.wait_exit(SHORT);
        term.pump(Duration::from_millis(300));
        assert_eq!(term.in_session("sleep").len(), 1, "{}", term.transcript());
        Ok(())
    });
}
