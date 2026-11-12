//! The desktop notification after a long command (S28), on a terminal that takes escape codes.
//!
//! The real `notify-send` is never reached: the test terminal has no `DISPLAY`, and the tests that
//! want a notification put a script called `notify-send` in a folder of their own at the front of
//! `PATH`, which writes down what it was given. "Long" is one second here (`min_seconds = 1`); a
//! test that expects *no* notification runs a long command afterwards and checks that this was
//! the only one, which does not depend on timing.

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use omsh_tests::{Ctx, Launch, SHORT, Session, pty_test};

const BIN: &str = env!("CARGO_BIN_EXE_omsh-pty-shell");

/// A long command for these tests: more than the one second of `min_seconds`.
const LONG: &str = "sleep 1.3";

fn write_config(ctx: &Ctx, text: &str) -> io::Result<()> {
    let file = ctx.home().join(".config/oh-my-shell/config.toml");
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(file, text)
}

const SHORT_LIMIT: &str = "[notify]\nmin_seconds = 1\n";

/// Puts an executable script `name` in `<home>/bin` and returns the folder.
fn program(ctx: &Ctx, name: &str, body: &str) -> io::Result<PathBuf> {
    let dir = ctx.home().join("bin");
    fs::create_dir_all(&dir)?;
    let file = dir.join(name);
    fs::write(&file, format!("#!/bin/sh\n{body}\n"))?;
    fs::set_permissions(&file, fs::Permissions::from_mode(0o755))?;
    Ok(dir)
}

fn log(ctx: &Ctx) -> PathBuf {
    ctx.home().join("notes.log")
}

/// A fake `notify-send` that appends its two last arguments (title, body) to the log, one line.
fn recording_notify_send(ctx: &Ctx) -> io::Result<PathBuf> {
    program(
        ctx,
        "notify-send",
        &format!(
            "printf '%s | %s\\n' \"$4\" \"$5\" >> {}",
            log(ctx).display()
        ),
    )
}

/// A launch with a desktop: a display, a session bus address and the fake program first in
/// `PATH`.
fn desktop(ctx: &Ctx, bin: &Path) -> Launch {
    ctx.launch()
        .env("PATH", &format!("{}:/usr/bin:/bin", bin.display()))
        .env("DISPLAY", ":0")
        .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent")
}

fn lines(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// Runs a long command and waits until the fake program has written its line.
fn run_long_and_wait(term: &mut Session, ctx: &Ctx, command: &str, count: usize) -> io::Result<()> {
    term.command(command)?;
    let until = Instant::now() + SHORT;
    while lines(&log(ctx)).len() < count {
        assert!(
            Instant::now() < until,
            "no notification for {command:?}: {:?}\n{}",
            lines(&log(ctx)),
            term.transcript()
        );
        term.pump(Duration::from_millis(20));
    }
    Ok(())
}

#[test]
fn a_command_that_ran_long_enough_notifies_with_its_line_and_its_time() {
    pty_test(BIN, |ctx| {
        write_config(ctx, SHORT_LIMIT)?;
        let bin = recording_notify_send(ctx)?;
        let mut term = desktop(ctx, &bin).start()?;
        run_long_and_wait(&mut term, ctx, LONG, 1)?;
        // The test shell runs in the C locale: ASCII title.
        assert_eq!(lines(&log(ctx)), ["Done in 1s | sleep 1.3"]);
        term.quit()
    });
}

#[test]
fn a_utf8_terminal_gets_the_check_mark_and_a_failure_says_its_status() {
    pty_test(BIN, |ctx| {
        write_config(ctx, SHORT_LIMIT)?;
        let bin = recording_notify_send(ctx)?;
        let mut term = desktop(ctx, &bin).env("LC_ALL", "C.UTF-8").start()?;
        run_long_and_wait(&mut term, ctx, "sleep 1.3; exit_status=$?; false", 1)?;
        assert_eq!(
            lines(&log(ctx)),
            ["✗ Failed (status 1) after 1s | sleep 1.3; exit_status=$?; false"]
        );
        term.quit()
    });
}

#[test]
fn a_quick_command_does_not_notify() {
    pty_test(BIN, |ctx| {
        write_config(ctx, SHORT_LIMIT)?;
        let bin = recording_notify_send(ctx)?;
        let mut term = desktop(ctx, &bin).start()?;
        term.sync("quick")?;
        run_long_and_wait(&mut term, ctx, LONG, 1)?;
        // The echo came first and left nothing: the only line is the long command's.
        assert_eq!(lines(&log(ctx)).len(), 1, "{:?}", lines(&log(ctx)));
        term.quit()
    });
}

#[test]
fn a_command_ended_with_ctrl_c_does_not_notify() {
    pty_test(BIN, |ctx| {
        write_config(ctx, SHORT_LIMIT)?;
        let bin = recording_notify_send(ctx)?;
        let mut term = desktop(ctx, &bin).start()?;
        term.command("sleep 30")?;
        term.pump(Duration::from_millis(1300));
        term.key(3)?;
        term.prompt();
        run_long_and_wait(&mut term, ctx, LONG, 1)?;
        assert_eq!(lines(&log(ctx)), ["Done in 1s | sleep 1.3"]);
        term.quit()
    });
}

#[test]
fn an_interactive_program_and_a_secret_in_the_line() {
    pty_test(BIN, |ctx| {
        write_config(ctx, SHORT_LIMIT)?;
        let bin = recording_notify_send(ctx)?;
        // A program called `vim` that takes a while, like a person editing.
        program(ctx, "vim", "sleep 1.3")?;
        let mut term = desktop(ctx, &bin).start()?;
        term.command("vim notes.txt")?;
        term.prompt();
        run_long_and_wait(&mut term, ctx, "sleep 1.3 # key=AKIAIOSFODNN7EXAMPLE", 1)?;
        let got = lines(&log(ctx));
        assert_eq!(got.len(), 1, "vim must not notify: {got:?}");
        assert!(got[0].contains("[MASKED]"), "{got:?}");
        assert!(!got[0].contains("AKIAIOSFODNN7EXAMPLE"), "{got:?}");
        term.quit()
    });
}

#[test]
fn notifications_can_be_turned_off_in_the_config() {
    pty_test(BIN, |ctx| {
        write_config(ctx, "[notify]\nenabled = false\nmin_seconds = 1\n")?;
        let bin = recording_notify_send(ctx)?;
        let mut term = desktop(ctx, &bin).start()?;
        term.command(LONG)?;
        term.sync("done")?;
        // A negative wait: nothing can be shown after the command has ended and the shell has
        // answered another command.
        term.pump(Duration::from_millis(300));
        assert!(lines(&log(ctx)).is_empty(), "{:?}", lines(&log(ctx)));
        term.quit()
    });
}

#[test]
fn without_a_desktop_the_person_is_told_once_with_a_toast() {
    pty_test(BIN, |ctx| {
        write_config(ctx, SHORT_LIMIT)?;
        // No DISPLAY: the harness starts with none.
        let mut term = ctx.start()?;
        term.command(LONG)?;
        term.expect_plain(
            "Desktop notification not shown: no desktop session (DISPLAY is not set)",
            SHORT,
        );
        term.quit()
    });
}

#[test]
fn over_ssh_a_forwarded_display_is_not_a_desktop() {
    pty_test(BIN, |ctx| {
        write_config(ctx, SHORT_LIMIT)?;
        let bin = recording_notify_send(ctx)?;
        let mut term = desktop(ctx, &bin)
            .env("SSH_CONNECTION", "1.2.3.4 5 6.7.8.9 22")
            .start()?;
        term.command(LONG)?;
        term.expect_plain(
            "Desktop notification not shown: this is an ssh session",
            SHORT,
        );
        assert!(lines(&log(ctx)).is_empty());
        term.quit()
    });
}

#[test]
fn the_plain_reader_prints_the_message_as_a_line_before_the_next_prompt() {
    pty_test(BIN, |ctx| {
        write_config(ctx, SHORT_LIMIT)?;
        let mut term = ctx.launch().dumb().start()?;
        term.command(LONG)?;
        term.expect(
            "oh-my-shell: Desktop notification not shown: no desktop session",
            SHORT,
        );
        term.quit()
    });
}

#[test]
fn a_program_that_hangs_does_not_hold_the_prompt_and_is_gone_when_the_shell_exits() {
    pty_test(BIN, |ctx| {
        write_config(ctx, SHORT_LIMIT)?;
        let marker = format!("omsh-pty-hang-{}", std::process::id());
        let bin = program(
            ctx,
            "notify-send",
            &format!("sh -c 'sleep 30; true' {marker} & wait"),
        )?;
        let mut term = desktop(ctx, &bin).start()?;
        term.command(LONG)?;
        // The prompt answers while the program hangs.
        term.sync("alive")?;
        term.quit()?;
        let until = Instant::now() + Duration::from_secs(3);
        while still_running(&marker) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !still_running(&marker),
            "the notification program outlived the shell"
        );
        Ok(())
    });
}

fn still_running(marker: &str) -> bool {
    fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .any(|entry| {
            fs::read(entry.path().join("cmdline"))
                .is_ok_and(|cmd| String::from_utf8_lossy(&cmd).contains(marker))
        })
}

#[test]
fn the_first_run_file_documents_the_section() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.quit()?;
        let text = fs::read_to_string(ctx.home().join(".config/oh-my-shell/config.toml"))?;
        assert!(text.contains("#[notify]\n"), "{text}");
        assert!(text.contains("#min_seconds = 10\n"), "{text}");
        Ok(())
    });
}
