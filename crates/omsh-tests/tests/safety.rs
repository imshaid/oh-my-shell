//! The safety analyzer and its question, in a real terminal (S16).
//!
//! Everything happens inside the test's own home directory. The one line that reaches a
//! Critical rule runs `shred` on a file that does not exist.

use std::fs;
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};

use omsh_tests::{Ctx, SHORT, Session, pty_test};

const BIN: &str = env!("CARGO_BIN_EXE_omsh-pty-shell");

const DANGEROUS_QUESTION: &str = "Run it? [y/N]";
const NOTHING_RAN: &str = "cancelled; nothing ran";
const NOWHERE: &str = "/nonexistent-omsh-safety-test";

/// A shell in the home directory, which holds `proj/build/f`.
fn shell_with_build(ctx: &Ctx) -> io::Result<Session> {
    let build = ctx.home().join("proj/build");
    fs::create_dir_all(&build)?;
    fs::write(build.join("f"), "x\n")?;
    ctx.launch().cwd(ctx.home()).start()
}

fn config_dir(ctx: &Ctx) -> io::Result<std::path::PathBuf> {
    let dir = ctx.home().join(".config/oh-my-shell");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Waits for the prompt, asks for `$?` and waits for its value.
fn status_is(term: &mut Session, value: u8) -> io::Result<()> {
    term.command("echo st=$?")?;
    term.expect_plain(&format!("st={value}"), SHORT);
    Ok(())
}

#[test]
fn a_dangerous_line_asks_and_n_keeps_the_files() {
    pty_test(BIN, |ctx| {
        let mut term = shell_with_build(ctx)?;
        term.command("rm -rf proj/build")?;
        term.expect_plain("this command is dangerous:", SHORT);
        term.expect_plain(DANGEROUS_QUESTION, SHORT);
        term.line("n")?;
        term.expect_plain(NOTHING_RAN, SHORT);
        status_is(&mut term, 1)?;
        assert!(ctx.home().join("proj/build/f").exists());
        term.quit()
    });
}

#[test]
fn the_same_line_runs_after_y() {
    pty_test(BIN, |ctx| {
        let mut term = shell_with_build(ctx)?;
        term.command("rm -rf proj/build")?;
        term.expect_plain(DANGEROUS_QUESTION, SHORT);
        term.line("y")?;
        status_is(&mut term, 0)?;
        assert!(!ctx.home().join("proj/build").exists());
        term.quit()
    });
}

#[test]
fn a_critical_line_needs_yes_or_the_target_typed() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.launch().cwd(ctx.home()).start()?;
        let command = format!("shred {NOWHERE}");
        // Not enough: the shell asks again for the exact words.
        term.command(&command)?;
        term.expect_plain("this command is critical:", SHORT);
        term.expect_plain(&format!("Type \"yes\" or \"{NOWHERE}\" to run it:"), SHORT);
        term.line("y")?;
        term.expect_plain(NOTHING_RAN, SHORT);
        status_is(&mut term, 1)?;
        // The target, as written, runs it: `shred` itself complains about the missing file.
        term.command(&command)?;
        term.expect_plain("to run it:", SHORT);
        term.line(NOWHERE)?;
        term.expect_plain(&format!("shred: {NOWHERE}"), SHORT);
        // So does "yes".
        term.prompt();
        let mark = term.transcript().len();
        term.command(&command)?;
        term.expect_plain("to run it:", SHORT);
        term.line("yes")?;
        term.prompt();
        assert!(
            term.transcript()[mark..].contains(&format!("shred: {NOWHERE}")),
            "{}",
            &term.transcript()[mark..]
        );
        term.quit()
    });
}

#[test]
fn ctrl_c_at_the_question_cancels_gives_130_and_the_shell_goes_on() {
    pty_test(BIN, |ctx| {
        let mut term = shell_with_build(ctx)?;
        term.command("rm -rf proj/build")?;
        term.expect_plain(DANGEROUS_QUESTION, SHORT);
        term.key(3)?;
        term.expect_plain(NOTHING_RAN, SHORT);
        status_is(&mut term, 130)?;
        assert!(ctx.home().join("proj/build/f").exists());
        term.command("echo still-here")?;
        term.expect_plain("still-here", SHORT);
        term.quit()
    });
}

#[test]
fn ctrl_c_at_the_question_leaves_no_interrupt_for_the_next_question() {
    pty_test(BIN, |ctx| {
        let mut term = shell_with_build(ctx)?;
        term.command("rm -rf proj/build")?;
        term.expect_plain(DANGEROUS_QUESTION, SHORT);
        term.key(3)?;
        term.expect_plain(NOTHING_RAN, SHORT);
        // The very next line asks again. Nothing ran in between that would clear a flag left
        // by Ctrl+C, so a flag left behind would cancel this question before it is answered.
        term.command("rm -rf proj/build")?;
        term.expect_plain(DANGEROUS_QUESTION, SHORT);
        term.line("y")?;
        status_is(&mut term, 0)?;
        assert!(!ctx.home().join("proj/build").exists());
        term.quit()
    });
}

#[test]
fn a_caution_prints_one_note_and_runs() {
    pty_test(BIN, |ctx| {
        fs::write(ctx.home().join("notes.txt"), "old\n")?;
        let mut term = ctx.launch().cwd(ctx.home()).start()?;
        term.command("echo new > notes.txt")?;
        term.expect_plain("oh-my-shell: caution: Replaces the content", SHORT);
        status_is(&mut term, 0)?;
        assert_eq!(fs::read_to_string(ctx.home().join("notes.txt"))?, "new\n");
        assert!(!term.transcript().contains(DANGEROUS_QUESTION));
        term.quit()
    });
}

#[test]
fn a_safe_line_shows_nothing_extra() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.launch().cwd(ctx.home()).start()?;
        term.command("echo hello-$((1+1))")?;
        term.expect_plain("hello-2", SHORT);
        term.prompt();
        let shown = term.transcript();
        assert!(!shown.contains("this command is"), "{shown}");
        assert!(!shown.contains("oh-my-shell: caution"), "{shown}");
        term.quit()
    });
}

#[test]
fn a_command_inside_a_substitution_or_an_alias_is_still_seen() {
    pty_test(BIN, |ctx| {
        let mut term = shell_with_build(ctx)?;
        term.command("alias del='rm -rf'")?;
        term.command("del proj/build")?;
        term.expect_plain(DANGEROUS_QUESTION, SHORT);
        term.line("n")?;
        term.expect_plain(NOTHING_RAN, SHORT);
        term.command("echo $(rm -rf proj/build)")?;
        term.expect_plain(DANGEROUS_QUESTION, SHORT);
        term.line("n")?;
        term.expect_plain(NOTHING_RAN, SHORT);
        term.prompt();
        assert!(ctx.home().join("proj/build/f").exists());
        term.quit()
    });
}

#[test]
fn the_confirm_setting_in_config_toml_is_read() {
    pty_test(BIN, |ctx| {
        fs::write(
            config_dir(ctx)?.join("config.toml"),
            "[safety]\nconfirm = \"critical\"\n",
        )?;
        let mut term = shell_with_build(ctx)?;
        // Dangerous, but below "critical": a note, no question, and it runs.
        term.command("rm -rf proj/build")?;
        term.expect_plain("oh-my-shell: dangerous: ", SHORT);
        status_is(&mut term, 0)?;
        assert!(!ctx.home().join("proj/build").exists());
        assert!(!term.transcript().contains(DANGEROUS_QUESTION));
        term.quit()
    });
}

#[test]
fn the_startup_file_is_not_asked_about() {
    pty_test(BIN, |ctx| {
        fs::create_dir_all(ctx.home().join("initdir"))?;
        fs::write(ctx.home().join("initdir/f"), "x\n")?;
        fs::write(
            config_dir(ctx)?.join("init.omsh"),
            "rm -rf \"$HOME/initdir\"\n",
        )?;
        let mut term = ctx.launch().cwd(ctx.home()).start()?;
        term.prompt();
        assert!(!ctx.home().join("initdir").exists());
        assert!(!term.transcript().contains("this command is"));
        term.quit()
    });
}

#[test]
fn input_that_is_not_typed_runs_without_a_question() {
    pty_test(BIN, |ctx| {
        let dir = ctx.home().join("piped/x");
        fs::create_dir_all(&dir)?;
        let output = run_piped(ctx.home(), "rm -rf piped; echo done-$((1+1))\n")?;
        assert!(output.contains("done-2"), "{output}");
        assert!(!output.contains("this command is"), "{output}");
        assert!(!ctx.home().join("piped").exists());
        Ok(())
    });
}

/// Feeds `script` to the shell's own interpreter through a pipe and returns what it printed.
fn run_piped(home: &Path, script: &str) -> io::Result<String> {
    use std::io::Write;
    let mut child = Command::new(BIN)
        .arg("--interpret-stdin")
        .arg("--no-config")
        .current_dir(home)
        .env("HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("no stdin"))?
        .write_all(script.as_bytes())?;
    let output = child.wait_with_output()?;
    Ok(format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    ))
}
