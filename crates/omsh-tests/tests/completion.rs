//! Tab completion (S15a), on a terminal that takes escape codes.
//!
//! The terminal does not draw: tests look for text in what the shell printed. A completion is
//! written into the line, so `expect_plain` (which ignores escape codes) finds it after the
//! typed text. A line is run only when the test wants to see its output.

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use omsh_tests::{Ctx, Launch, SHORT, Session, pty_test};

const BIN: &str = env!("CARGO_BIN_EXE_omsh-pty-shell");

const TAB: &[u8] = b"\t";
const CTRL_U: &[u8] = b"\x15";
const CTRL_C: &[u8] = b"\x03";
const ENTER: &[u8] = b"\r";

/// A program in `dir` that prints `output`.
fn program(dir: &Path, name: &str, output: &str) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\necho {output}\n"))?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
}

/// A shell whose `PATH` is `bin`, then the usual directories.
fn with_path(ctx: &Ctx, bin: &Path) -> Launch {
    let usual = std::env::var("PATH").unwrap_or_default();
    ctx.launch()
        .env("PATH", &format!("{}:{usual}", bin.display()))
}

fn start_in(launch: Launch, cwd: &Path) -> io::Result<Session> {
    launch.cwd(cwd).start()
}

fn write_config(ctx: &Ctx, text: &str) -> io::Result<()> {
    let dir = ctx.home().join(".config/oh-my-shell");
    fs::create_dir_all(&dir)?;
    fs::write(dir.join("config.toml"), text)
}

/// A name with one match is put into the line whole, with a space after it, and runs.
#[test]
fn tab_completes_the_only_command_that_fits() {
    pty_test(BIN, |ctx| {
        let bin = ctx.home().join("bin");
        program(&bin, "omshonly-tool", "ran-$((6*7))")?;
        let mut term = start_in(with_path(ctx, &bin), ctx.home())?;
        term.prompt();
        term.write_bytes(b"omshonly")?;
        term.write_bytes(TAB)?;
        term.expect_plain("omshonly-tool", SHORT);
        term.write_bytes(ENTER)?;
        term.expect("ran-42", SHORT);
        term.quit()
    });
}

/// Several matches: the start they share goes into the line and the choices are listed.
#[test]
fn tab_inserts_the_common_start_and_lists_the_choices() {
    pty_test(BIN, |ctx| {
        let bin = ctx.home().join("bin");
        program(&bin, "omshpair-one", "one-$((1+1))")?;
        program(&bin, "omshpair-two", "two-$((1+1))")?;
        let mut term = start_in(with_path(ctx, &bin), ctx.home())?;
        term.prompt();
        term.write_bytes(b"omshpair")?;
        term.write_bytes(TAB)?;
        term.expect_plain("omshpair-one", SHORT);
        term.expect_plain("omshpair-two", SHORT);
        // The shared start `omshpair-` is in the line: the choice is made by typing `t` and Tab.
        term.write_bytes(b"t")?;
        term.write_bytes(TAB)?;
        term.expect_plain("omshpair-two", SHORT);
        term.write_bytes(ENTER)?;
        term.expect("two-2", SHORT);
        term.quit()
    });
}

/// A choice in the menu is taken with the arrow keys and Enter.
#[test]
fn a_choice_is_made_with_tab_and_enter() {
    pty_test(BIN, |ctx| {
        let bin = ctx.home().join("bin");
        program(&bin, "omshpick-a", "got-a-$((1+1))")?;
        program(&bin, "omshpick-b", "got-b-$((1+1))")?;
        let mut term = start_in(with_path(ctx, &bin), ctx.home())?;
        term.prompt();
        term.write_bytes(b"omshpick")?;
        term.write_bytes(TAB)?;
        term.expect_plain("omshpick-b", SHORT);
        // The first Tab opened the menu with the first choice marked; the next one moves to the
        // second.
        term.write_bytes(TAB)?;
        // The menu moves its mark when it paints, so keys that arrive in the same instant as the
        // Tab would be read before the move. A person cannot type that fast.
        term.pump(Duration::from_millis(300));
        term.write_bytes(ENTER)?;
        // The choice is in the line now; the second Enter runs it.
        term.expect_plain("omshpick-b", SHORT);
        term.write_bytes(ENTER)?;
        term.expect("got-b-2", SHORT);
        term.quit()
    });
}

/// A file name with a space gets a backslash, and the command reads it back.
#[test]
fn a_file_name_with_a_space_is_escaped() {
    pty_test(BIN, |ctx| {
        let dir = ctx.home().join("files");
        fs::create_dir_all(&dir)?;
        fs::write(dir.join("my file.txt"), "")?;
        let mut term = start_in(ctx.launch(), &dir)?;
        term.prompt();
        term.write_bytes(b"ls my")?;
        term.write_bytes(TAB)?;
        term.expect_plain("ls my\\ file.txt", SHORT);
        term.write_bytes(ENTER)?;
        term.expect("my file.txt", SHORT);
        term.quit()
    });
}

/// A folder gets a slash and no space, so that the name can go on.
#[test]
fn a_folder_ends_with_a_slash_and_completion_goes_on_inside_it() {
    pty_test(BIN, |ctx| {
        let dir = ctx.home().join("work");
        fs::create_dir_all(dir.join("omshfolder/inner"))?;
        fs::write(dir.join("omshfolder/inner/leaf.txt"), "")?;
        let mut term = start_in(ctx.launch(), &dir)?;
        term.prompt();
        term.write_bytes(b"ls omshf")?;
        term.write_bytes(TAB)?;
        term.expect_plain("ls omshfolder/", SHORT);
        // One level at a time: the next Tab goes into the folder.
        term.write_bytes(TAB)?;
        term.expect_plain("ls omshfolder/inner/", SHORT);
        term.write_bytes(b"l")?;
        term.write_bytes(TAB)?;
        term.expect_plain("ls omshfolder/inner/leaf.txt", SHORT);
        term.write_bytes(CTRL_U)?;
        term.quit()
    });
}

/// `$NAME` is completed from the shell's variables.
#[test]
fn a_variable_name_is_completed() {
    pty_test(BIN, |ctx| {
        let mut term = ctx
            .launch()
            .env("OMSHVAR_UNIQUE_NAME", "vvalue-9")
            .cwd(ctx.home())
            .start()?;
        term.prompt();
        term.write_bytes(b"echo $OMSHVAR_U")?;
        term.write_bytes(TAB)?;
        term.expect_plain("echo $OMSHVAR_UNIQUE_NAME", SHORT);
        term.write_bytes(ENTER)?;
        term.expect("vvalue-9", SHORT);
        term.quit()
    });
}

/// A variable set by an earlier line is offered at the next prompt.
#[test]
fn a_variable_set_a_moment_ago_is_offered() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.command("omshnew_var=fresh-$((2+2))")?;
        term.prompt();
        term.write_bytes(b"echo $omshnew")?;
        term.write_bytes(TAB)?;
        term.expect_plain("echo $omshnew_var", SHORT);
        term.write_bytes(ENTER)?;
        term.expect("fresh-4", SHORT);
        term.quit()
    });
}

/// `git checkout` offers the branches of the repository, read from its files. A `git` on the
/// `PATH` that leaves a mark when it runs proves that the completion never starts it.
#[test]
fn git_branches_are_read_from_the_repository_files_and_git_is_not_run() {
    pty_test(BIN, |ctx| {
        let repo = ctx.home().join("repo");
        fs::create_dir_all(repo.join(".git/refs/heads/feature"))?;
        fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n")?;
        fs::write(repo.join(".git/refs/heads/main"), "")?;
        fs::write(repo.join(".git/refs/heads/feature/omshlogin"), "")?;
        let bin = ctx.home().join("bin");
        let mark = ctx.home().join("git-was-run");
        program(&bin, "git", &format!("touch {}", mark.display()))?;
        let mut term = start_in(with_path(ctx, &bin), &repo)?;
        term.prompt();
        term.write_bytes(b"git checkout feature/omshl")?;
        term.write_bytes(TAB)?;
        term.expect_plain("git checkout feature/omshlogin", SHORT);
        term.write_bytes(CTRL_U)?;
        // The git sub-command is completed too.
        term.write_bytes(b"git chec")?;
        term.write_bytes(TAB)?;
        term.expect_plain("git checkout", SHORT);
        term.write_bytes(CTRL_U)?;
        term.sync("done")?;
        assert!(!mark.exists(), "completion ran git");
        term.quit()
    });
}

/// Flags come from a fish completion file found through `XDG_DATA_DIRS`.
#[test]
fn flags_come_from_a_fish_completion_file() {
    pty_test(BIN, |ctx| {
        let share = ctx.home().join("share");
        let dir = share.join("fish/completions");
        fs::create_dir_all(&dir)?;
        fs::write(
            dir.join("omshcmd.fish"),
            "complete -c omshcmd -s a -l omshallthings -d 'Show all the things'\n",
        )?;
        let mut term = ctx
            .launch()
            .env("XDG_DATA_DIRS", share.to_str().unwrap_or(""))
            .cwd(ctx.home())
            .start()?;
        term.prompt();
        term.write_bytes(b"omshcmd --omshall")?;
        term.write_bytes(TAB)?;
        term.expect_plain("omshcmd --omshallthings", SHORT);
        term.write_bytes(CTRL_U)?;
        term.quit()
    });
}

/// The description of a flag is shown beside it in the menu.
#[test]
fn the_menu_shows_the_description_of_a_flag() {
    pty_test(BIN, |ctx| {
        let share = ctx.home().join("share");
        let dir = share.join("fish/completions");
        fs::create_dir_all(&dir)?;
        fs::write(
            dir.join("omshdesc.fish"),
            "complete -c omshdesc -l omsh-one -d 'First described flag'\n\
             complete -c omshdesc -l omsh-two -d 'Second described flag'\n",
        )?;
        let mut term = ctx
            .launch()
            .env("XDG_DATA_DIRS", share.to_str().unwrap_or(""))
            .cwd(ctx.home())
            .start()?;
        term.prompt();
        term.write_bytes(b"omshdesc --omsh")?;
        term.write_bytes(TAB)?;
        term.expect_plain("First described flag", SHORT);
        term.expect_plain("Second described flag", SHORT);
        term.write_bytes(CTRL_U)?;
        term.quit()
    });
}

/// Nothing is completed in an AI request.
#[test]
fn an_ai_request_is_not_completed() {
    pty_test(BIN, |ctx| {
        fs::write(ctx.home().join("zzomshfile"), "")?;
        let mut term = start_in(ctx.launch(), ctx.home())?;
        term.prompt();
        term.write_bytes(b"? zzomsh")?;
        term.expect_plain("? zzomsh", SHORT);
        term.write_bytes(TAB)?;
        // The editor needs a moment to react to the key; a completion would show the file.
        assert!(!term.try_expect("zzomshfile", Duration::from_millis(600)));
        term.write_bytes(CTRL_U)?;
        term.quit()
    });
}

/// Ctrl+C closes the menu and drops the line; the shell goes on.
#[test]
fn ctrl_c_with_the_menu_open_returns_to_the_prompt() {
    pty_test(BIN, |ctx| {
        let bin = ctx.home().join("bin");
        program(&bin, "omshint-a", "a")?;
        program(&bin, "omshint-b", "b")?;
        let mut term = start_in(with_path(ctx, &bin), ctx.home())?;
        term.prompt();
        term.write_bytes(b"omshint")?;
        term.write_bytes(TAB)?;
        term.expect_plain("omshint-b", SHORT);
        term.write_bytes(CTRL_C)?;
        term.sync("alive")?;
        term.quit()
    });
}

/// Tab works in vi mode too (in insert mode).
#[test]
fn tab_works_in_vi_mode() {
    pty_test(BIN, |ctx| {
        write_config(ctx, "[editor]\nmode = \"vi\"\n")?;
        let bin = ctx.home().join("bin");
        program(&bin, "omshvi-tool", "vi-ran-$((6*7))")?;
        let mut term = start_in(with_path(ctx, &bin), ctx.home())?;
        term.prompt();
        term.write_bytes(b"omshvi")?;
        term.write_bytes(TAB)?;
        term.expect_plain("omshvi-tool", SHORT);
        term.write_bytes(ENTER)?;
        term.expect("vi-ran-42", SHORT);
        term.quit()
    });
}
