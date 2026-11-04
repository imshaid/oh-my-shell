//! Runs the real `oh-my-shell` binary and checks what a user would see.

use std::fs::OpenOptions;
use std::io;
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

#[test]
fn running_without_arguments_says_the_shell_is_not_ready() -> io::Result<()> {
    let output = oh_my_shell(&[])?;

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("not implemented yet"));
    Ok(())
}

/// Regression test: `eprintln!` panics when stderr cannot be written, which made the
/// binary exit with 101 instead of 1.
#[test]
fn unwritable_stderr_does_not_cause_a_panic() -> io::Result<()> {
    // Every write to /dev/full fails with "no space left on device".
    let stderr = OpenOptions::new().write(true).open("/dev/full")?;

    let status = Command::new(BINARY)
        .stdout(Stdio::null())
        .stderr(stderr)
        .status()?;

    assert_eq!(status.code(), Some(1));
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
