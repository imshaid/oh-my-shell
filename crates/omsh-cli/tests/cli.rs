//! Runs the real `oh-my-shell` binary and checks what a user would see.

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::process::{Command, Output, Stdio};

const BINARY: &str = env!("CARGO_BIN_EXE_oh-my-shell");

fn oh_my_shell(args: &[&str]) -> io::Result<Output> {
    Command::new(BINARY).args(args).output()
}

#[test]
fn version_flag_prints_the_binary_name_and_version() -> io::Result<()> {
    let output = oh_my_shell(&["--version"])?;

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        concat!("oh-my-shell ", env!("CARGO_PKG_VERSION"), "\n"),
    );
    assert!(output.stderr.is_empty());
    Ok(())
}

#[test]
fn help_flag_prints_usage_and_options() -> io::Result<()> {
    let output = oh_my_shell(&["--help"])?;

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Usage: oh-my-shell"));
    assert!(stdout.contains("--help"));
    assert!(stdout.contains("--version"));
    assert!(output.stderr.is_empty());
    Ok(())
}

/// Runs the shell with `input` on standard input.
fn with_input(input: &[u8]) -> io::Result<Output> {
    let mut child = Command::new(BINARY)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(input)?;
    }
    child.wait_with_output()
}

#[test]
fn empty_input_ends_the_shell_with_status_zero() -> io::Result<()> {
    let output = with_input(b"")?;

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    Ok(())
}

#[test]
fn a_command_on_standard_input_runs_without_a_prompt() -> io::Result<()> {
    let output = with_input(b"echo hello\n")?;

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "hello\n");
    // No prompt is printed when input is not a terminal.
    assert!(output.stderr.is_empty());
    Ok(())
}

/// Regression test: `eprintln!` panics when stderr cannot be written, which made the
/// binary exit with 101 instead of with the status of the last command. The command here
/// fails, so the shell has a message to write to a stderr that rejects it.
#[test]
fn unwritable_stderr_does_not_cause_a_panic() -> io::Result<()> {
    // Every write to /dev/full fails with "no space left on device".
    let stderr = OpenOptions::new().write(true).open("/dev/full")?;

    let mut child = Command::new(BINARY)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(stderr)
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(b"nosuchcmd_omsh\n")?;
    }
    let status = child.wait()?;

    assert_eq!(status.code(), Some(127));
    Ok(())
}

#[test]
fn unknown_flag_is_rejected_with_a_usage_error() -> io::Result<()> {
    let output = oh_my_shell(&["--no-such-flag"])?;

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--no-such-flag"));
    assert!(stderr.contains("Usage: oh-my-shell"));
    Ok(())
}
