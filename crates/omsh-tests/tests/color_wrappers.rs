//! The default colors of `ls`, `grep`, `egrep`, `fgrep` and `diff`, on a terminal with the
//! real programs.
//!
//! The editor also writes escape codes while a line is typed, so the tests look for codes the
//! editor never uses: GNU `grep` marks a match with `ESC[01;31m`, `ls` marks a directory with
//! `ESC[01;34m`, and `diff` writes its hunk header `1c1` in cyan. The harness sets
//! `NO_COLOR=1`; the tests that want colors set it empty, which means "not set".

use std::fs;
use std::io;

use omsh_tests::{Ctx, SHORT, Session, pty_test};

const BIN: &str = env!("CARGO_BIN_EXE_omsh-pty-shell");

const GREP_MATCH: &str = "\x1b[01;31m";
const LS_DIRECTORY: &str = "\x1b[01;34m";
const DIFF_HUNK: &str = "\x1b[36m1c1";

/// A shell with colors allowed, in a directory that holds `f` ("xyz"), `a` and `b`.
fn colored(ctx: &Ctx) -> io::Result<Session> {
    let dir = ctx.home();
    fs::write(dir.join("f"), "xyz\n")?;
    fs::write(dir.join("a"), "one\n")?;
    fs::write(dir.join("b"), "two\n")?;
    ctx.launch().env("NO_COLOR", "").cwd(dir).start()
}

/// Runs `command`, then a marker, and returns what the terminal showed in between.
fn output_of(term: &mut Session, command: &str) -> io::Result<String> {
    // Wait for the prompt first, so that the mark is taken after the previous command's output.
    term.prompt();
    let mark = term.transcript().len();
    term.line(&format!("{command}; echo end-$((1+1))"))?;
    term.expect("end-2", SHORT);
    Ok(term.transcript()[mark..].to_owned())
}

fn write_init(ctx: &Ctx, text: &str) -> io::Result<()> {
    let dir = ctx.home().join(".config/oh-my-shell");
    fs::create_dir_all(&dir)?;
    fs::write(dir.join("init.omsh"), text)
}

#[test]
fn the_programs_are_colored_on_a_terminal() {
    pty_test(BIN, |ctx| {
        let mut term = colored(ctx)?;
        let shown = output_of(&mut term, "grep y f")?;
        assert!(shown.contains(GREP_MATCH), "grep: {shown:?}");
        let shown = output_of(&mut term, "fgrep y f")?;
        assert!(shown.contains(GREP_MATCH), "fgrep: {shown:?}");
        let shown = output_of(&mut term, "egrep 'y|q' f")?;
        assert!(shown.contains(GREP_MATCH), "egrep: {shown:?}");
        let shown = output_of(&mut term, "ls -d /")?;
        assert!(shown.contains(LS_DIRECTORY), "ls: {shown:?}");
        let shown = output_of(&mut term, "diff a b")?;
        assert!(shown.contains(DIFF_HUNK), "diff: {shown:?}");
        term.quit()
    });
}

#[test]
fn a_pipe_gets_no_colors() {
    pty_test(BIN, |ctx| {
        let mut term = colored(ctx)?;
        let shown = output_of(&mut term, "grep y f | cat; ls -d / | cat; diff a b | cat")?;
        for code in [GREP_MATCH, LS_DIRECTORY, DIFF_HUNK] {
            assert!(!shown.contains(code), "{code:?} in a pipe: {shown:?}");
        }
        term.quit()
    });
}

#[test]
fn no_color_turns_the_wrappers_off_at_every_call() {
    pty_test(BIN, |ctx| {
        let mut term = colored(ctx)?;
        // As a prefix, for one command.
        let shown = output_of(&mut term, "NO_COLOR=1 grep y f")?;
        assert!(!shown.contains(GREP_MATCH), "{shown:?}");
        // Exported later in the session, and taken away again.
        term.command("export NO_COLOR=1")?;
        let shown = output_of(&mut term, "ls -d /")?;
        assert!(!shown.contains(LS_DIRECTORY), "{shown:?}");
        term.command("unset NO_COLOR")?;
        let shown = output_of(&mut term, "ls -d /")?;
        assert!(shown.contains(LS_DIRECTORY), "{shown:?}");
        term.quit()
    });
}

#[test]
fn the_harness_default_no_color_gives_plain_output() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        let shown = output_of(&mut term, "ls -d /; grep y /dev/null")?;
        assert!(!shown.contains(LS_DIRECTORY), "{shown:?}");
        term.quit()
    });
}

#[test]
fn the_shell_never_sets_the_color_variables_of_the_programs() {
    pty_test(BIN, |ctx| {
        let mut term = colored(ctx)?;
        let shown = output_of(
            &mut term,
            "echo v-${LS_COLORS-unset}-${GREP_COLOR-unset}-${GREP_COLORS-unset}",
        )?;
        assert!(shown.contains("v-unset-unset-unset"), "{shown:?}");
        term.quit()
    });
}

#[test]
fn command_skips_the_wrapper() {
    pty_test(BIN, |ctx| {
        let mut term = colored(ctx)?;
        let shown = output_of(&mut term, "command grep y f")?;
        assert!(!shown.contains(GREP_MATCH), "{shown:?}");
        term.quit()
    });
}

#[test]
fn an_alias_in_init_omsh_beats_the_wrapper_and_so_does_unset_f() {
    pty_test(BIN, |ctx| {
        write_init(ctx, "alias grep='grep --color=never'\nunset -f ls\n")?;
        let fresh = |ctx: &Ctx| ctx.launch().env("NO_COLOR", "").cwd(ctx.home()).start();
        fs::write(ctx.home().join("f"), "xyz\n")?;
        let mut term = fresh(ctx)?;
        let shown = output_of(&mut term, "grep y f")?;
        assert!(!shown.contains(GREP_MATCH), "alias lost: {shown:?}");
        let shown = output_of(&mut term, "ls -d /")?;
        assert!(!shown.contains(LS_DIRECTORY), "unset -f kept it: {shown:?}");
        // The ones init.omsh did not touch are still wrapped.
        let shown = output_of(&mut term, "type fgrep")?;
        assert!(shown.contains("fgrep is a shell function"), "{shown:?}");
        term.quit()
    });
}

#[test]
fn a_function_in_init_omsh_replaces_the_wrapper() {
    pty_test(BIN, |ctx| {
        write_init(ctx, "ls() { echo my-ls; }\n")?;
        let mut term = ctx.launch().env("NO_COLOR", "").start()?;
        term.line("ls")?;
        term.expect("my-ls", SHORT);
        term.quit()
    });
}

#[test]
fn no_config_still_has_the_wrappers() {
    pty_test(BIN, |ctx| {
        write_init(ctx, "unset -f ls\n")?;
        let mut term = ctx.launch().arg("--no-config").start()?;
        let shown = output_of(&mut term, "type ls")?;
        assert!(shown.contains("ls is a shell function"), "{shown:?}");
        term.quit()
    });
}
