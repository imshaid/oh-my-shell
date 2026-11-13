//! The `omsh` program (S29a): the installer makes `omsh` a symbolic link to `oh-my-shell`, and
//! `omsh ai status` then works from any shell. These tests make that link and start it on a
//! real terminal, as the installer's link would be started.
//!
//! A command line typed into a terminal is echoed, so a test never looks for text it typed.

use std::time::Duration;

use nix::sys::termios::LocalFlags;
use omsh_tests::{Ctx, Launch, SHORT, pty_test};

const BIN: &str = env!("CARGO_BIN_EXE_omsh-pty-shell");

/// A launch under the name `omsh`, with a keyring that does not exist.
fn omsh(ctx: &Ctx) -> std::io::Result<Launch> {
    ctx.launch()
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            "unix:path=/nonexistent/omsh-test-bus",
        )
        .named(ctx.home(), "omsh")
}

fn status_of(mut term: omsh_tests::Session) -> Option<i32> {
    term.wait_exit(SHORT).code()
}

#[test]
fn omsh_help_prints_the_usage_and_ends() {
    pty_test(BIN, |ctx| {
        let mut term = omsh(ctx)?.arg("help").start_raw()?;
        term.expect("usage: omsh <command>", SHORT);
        assert_eq!(status_of(term), Some(0));
        Ok(())
    });
}

#[test]
fn omsh_ai_status_runs_without_starting_a_shell() {
    pty_test(BIN, |ctx| {
        let mut term = omsh(ctx)?.arg("ai").arg("status").start_raw()?;
        term.expect("AI: not set up yet (run: omsh ai setup)", SHORT);
        let transcript = term.transcript().to_owned();
        assert!(!transcript.contains("$ "), "no prompt: {transcript}");
        assert_eq!(status_of(term), Some(0));
        Ok(())
    });
}

#[test]
fn the_switches_before_the_word_still_count() {
    pty_test(BIN, |ctx| {
        let mut term = omsh(ctx)?
            .arg("--no-ai")
            .arg("ai")
            .arg("status")
            .start_raw()?;
        term.expect("AI: off for this session (--no-ai)", SHORT);
        assert_eq!(status_of(term), Some(0));
        Ok(())
    });
}

#[test]
fn no_config_reads_and_writes_nothing() {
    pty_test(BIN, |ctx| {
        let mut term = omsh(ctx)?
            .arg("--no-config")
            .arg("ai")
            .arg("status")
            .start_raw()?;
        term.expect("AI: not set up yet", SHORT);
        assert_eq!(status_of(term), Some(0));
        assert!(
            !ctx.home().join(".config/oh-my-shell").exists(),
            "--no-config writes no file"
        );
        Ok(())
    });
}

#[test]
fn an_unknown_command_is_a_usage_error() {
    pty_test(BIN, |ctx| {
        let mut term = omsh(ctx)?.arg("ai").arg("bogus").start_raw()?;
        term.expect("omsh: ai: bogus: unknown command", SHORT);
        assert_eq!(status_of(term), Some(2));
        Ok(())
    });
}

/// Only the first word decides: `omsh script` is still a script run by `/bin/sh` (§8), and a
/// word that only starts like a tool is not one.
#[test]
fn a_word_that_is_not_a_command_goes_to_sh_as_a_script_name() {
    pty_test(BIN, |ctx| {
        let mut term = omsh(ctx)?.arg("ai.sh").start_raw()?;
        // `sh` says it cannot open the script; the tool code would have printed a usage.
        term.expect("ai.sh", SHORT);
        let code = status_of(term);
        assert!(code.is_some_and(|code| code != 0), "{code:?}");
        Ok(())
    });
}

/// Without the `omsh` name nothing changes: `oh-my-shell ai status` runs a script called `ai`.
#[test]
fn under_its_own_name_the_word_ai_is_a_script_name() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.launch().arg("ai").arg("status").start_raw()?;
        term.expect("ai", SHORT);
        let transcript = term.transcript().to_owned();
        assert!(!transcript.contains("AI: "), "{transcript}");
        let code = status_of(term);
        assert!(code.is_some_and(|code| code != 0), "{code:?}");
        Ok(())
    });
}

/// `omsh` alone is the interactive shell.
#[test]
fn omsh_alone_is_the_interactive_shell() {
    pty_test(BIN, |ctx| {
        let mut term = omsh(ctx)?.start()?;
        term.command("echo sum-$((6*7))")?;
        term.expect("sum-42", SHORT);
        term.quit()
    });
}

/// Ctrl+C at the hidden key prompt of `omsh ai setup`, started as its own program: the exit
/// status is 130, nothing is saved, and the terminal gets its echo back (the program catches
/// the signal so that it can).
#[test]
fn ctrl_c_at_the_key_prompt_ends_the_program_with_130_and_restores_the_terminal() {
    pty_test(BIN, |ctx| {
        let server = omsh_tests::fakehttp::Server::start(Duration::ZERO)?;
        let mut term = omsh(ctx)?.arg("ai").arg("setup").start_raw()?;
        term.expect("Number (1-5)", SHORT);
        term.line("5")?;
        term.expect("Server address", SHORT);
        term.line(&server.url("/v1"))?;
        term.expect("API key", SHORT);
        assert!(!term.termios()?.local_flags.contains(LocalFlags::ECHO));
        term.write_bytes(b"half-typed")?;
        term.key(3)?;
        term.expect("cancelled; nothing was saved", SHORT);
        let settings = term.termios()?;
        assert!(
            settings
                .local_flags
                .contains(LocalFlags::ECHO | LocalFlags::ICANON),
            "echo and line mode are back: {settings:?}"
        );
        assert_eq!(status_of(term), Some(130));
        assert!(!ctx.home().join(".config/oh-my-shell/credentials").exists());
        assert!(server.requests().is_empty(), "nothing was sent anywhere");
        Ok(())
    });
}
