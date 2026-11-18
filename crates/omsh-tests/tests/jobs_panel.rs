//! The jobs panel (UI upgrade, phase 6) on a real terminal: `jobs -i` and the palette's "Jobs".
//!
//! A panel hides the cursor while it is open, so its keys are sent with `Session::press`, after
//! its text has been seen. Where nobody can press arrow keys (`TERM=dumb`, a pipe) `jobs -i` is
//! the plain list. The harness has no UTF-8 locale, so the panel is drawn in ASCII here; how it
//! looks in the other forms is tested in `omsh-ui`.

use std::time::Duration;

use omsh_tests::procfs::state;
use omsh_tests::{SHORT, Session, pty_test};

const BIN: &str = env!("CARGO_BIN_EXE_omsh-pty-shell");

const CTRL_P: u8 = 0x10;
const CTRL_Z: u8 = 0x1a;
const ESC: &[u8] = b"\x1b";

/// Starts `sleep 30` in the background and opens the panel on it.
fn open_on_a_sleep(term: &mut Session) -> std::io::Result<()> {
    term.line("sleep 30 &")?;
    term.prompt();
    term.line("jobs -i")?;
    term.expect_plain("Running", SHORT);
    term.expect_plain("sleep 30", SHORT);
    term.expect_plain("Foreground", SHORT);
    Ok(())
}

/// Ends what is left of the background job and the session.
fn clean_up(term: &mut Session) -> std::io::Result<()> {
    term.line("kill %1")?;
    term.repeat_until("echo gone-$((1+1))", "Terminated")?;
    term.quit()
}

#[test]
fn kill_asks_and_then_ends_the_job_and_the_panel_shows_how_it_ended() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        open_on_a_sleep(&mut term)?;
        term.press(b"k")?;
        term.expect_plain("Kill this job?", SHORT);
        term.expect_plain("Sends SIGTERM, as `kill %1`.", SHORT);
        term.press(b"k")?;
        // The panel is shown again, with the real word for how the job ended.
        term.expect_plain("Terminated", SHORT);
        term.press(ESC)?;
        term.prompt();
        term.expect_none("sleep");
        // The panel showed the finished job, so it is forgotten and no notice follows.
        term.line("jobs; echo left-$?-over")?;
        term.expect("left-0-over", SHORT);
        assert!(!term.transcript().contains("[1]+"), "{}", term.transcript());
        term.quit()
    });
}

#[test]
fn kill_has_cancel_chosen_so_enter_does_not_kill() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        open_on_a_sleep(&mut term)?;
        term.press(b"k")?;
        term.expect_plain("Kill this job?", SHORT);
        term.press(b"\r")?;
        // Back to the table, and the job is alive.
        term.expect_plain("Background", SHORT);
        assert_eq!(term.in_session("sleep").len(), 1);
        term.press(ESC)?;
        term.prompt();
        clean_up(&mut term)
    });
}

#[test]
fn stop_and_background_stop_and_continue_the_job() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        open_on_a_sleep(&mut term)?;
        let sleep = term.in_session("sleep")[0];
        // Background does not fit a running job: nothing happens.
        term.press(b"b")?;
        term.pump(Duration::from_millis(200));
        assert_ne!(state(sleep), "T");
        term.press(b"s")?;
        term.expect_plain("Stopped", SHORT);
        assert_eq!(state(sleep), "T");
        term.press(b"b")?;
        // The panel shows the job running again, not the state from before the key.
        term.expect_plain("Running", SHORT);
        term.pump(Duration::from_millis(200));
        assert_ne!(state(sleep), "T");
        term.press(ESC)?;
        term.prompt();
        clean_up(&mut term)
    });
}

#[test]
fn foreground_closes_the_panel_and_runs_the_job_in_front() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.line("sleep 2")?;
        term.pump(Duration::from_millis(400));
        term.key(CTRL_Z)?;
        term.expect("Stopped", SHORT);
        term.prompt();
        term.line("jobs -i")?;
        term.expect_plain("Stopped", SHORT);
        term.expect_plain("Foreground", SHORT);
        term.press(b"f")?;
        // `fg` names the job, then the shell waits for it and the prompt comes back.
        term.expect("sleep 2", SHORT);
        term.prompt();
        term.command("echo st-$?-end")?;
        term.expect("st-0-end", SHORT);
        term.expect_none("sleep");
        term.quit()
    });
}

#[test]
fn esc_closes_the_panel_and_changes_nothing() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        open_on_a_sleep(&mut term)?;
        term.press(ESC)?;
        term.prompt();
        assert_eq!(term.in_session("sleep").len(), 1);
        term.line("jobs")?;
        term.expect("Running", SHORT);
        clean_up(&mut term)
    });
}

#[test]
fn with_no_jobs_the_panel_says_so_and_esc_closes_it() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.line("jobs -i")?;
        term.expect_plain("No jobs", SHORT);
        term.press(ESC)?;
        term.prompt();
        term.quit()
    });
}

#[test]
fn the_palette_entry_opens_the_panel_and_keeps_what_was_typed() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.line("sleep 30 &")?;
        term.prompt();
        term.write_bytes(b"echo kee$((1+1))")?;
        term.key(CTRL_P)?;
        term.expect("\x1b[?1049h", SHORT);
        term.expect_plain("Up/Down move", SHORT);
        term.write_bytes(b"Jobs")?;
        term.write_bytes(b"\r")?;
        term.expect("\x1b[?1049l", SHORT);
        // The table, not a line put in the editor.
        term.expect_plain("Foreground", SHORT);
        term.press(ESC)?;
        // What was typed is still there, and Enter runs it. `$?` was not touched by the panel.
        term.write_bytes(b"\r")?;
        term.expect("kee2", SHORT);
        term.prompt();
        clean_up(&mut term)
    });
}

/// Where nobody can press arrow keys the same words give the plain list, as `jobs` does.
#[test]
fn without_the_panels_jobs_i_is_the_plain_list() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.launch().dumb().start()?;
        term.line("sleep 30 &")?;
        term.prompt();
        term.line("jobs -i")?;
        term.expect("Running", SHORT);
        term.expect("sleep 30 &", SHORT);
        term.prompt();
        assert!(!term.transcript().contains("Foreground"));
        clean_up(&mut term)
    });
}

#[test]
fn into_a_file_jobs_i_is_the_plain_list_too() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.line("sleep 30 &")?;
        term.prompt();
        // Keys and standard error are the terminal, standard output is not: no panel.
        term.line("jobs -i > $HOME/jobs.txt; echo st-$?-end")?;
        term.expect("st-0-end", SHORT);
        term.prompt();
        assert!(!term.transcript().contains("Foreground"));
        let listed = std::fs::read_to_string(ctx.home().join("jobs.txt"))?;
        assert!(
            listed.contains("Running") && listed.contains("sleep 30 &"),
            "{listed}"
        );
        clean_up(&mut term)
    });
}

#[test]
fn jobs_i_with_other_options_is_refused() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.line("jobs -il; echo st-$?-end")?;
        term.expect("-i cannot be used with other options", SHORT);
        term.expect("st-2-end", SHORT);
        term.quit()
    });
}
