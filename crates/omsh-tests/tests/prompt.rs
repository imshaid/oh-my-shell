//! The prompt (S15b): the git segment with its dirty marker, the transient prompt, and the
//! "did you mean" line after `not found`.
//!
//! The git segment runs `git` as a process, so these tests put a `git` of their own first on
//! `PATH`: a script that prints what the test needs and leaves a trace of how it was called.

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use omsh_tests::{Ctx, Launch, SHORT, Session, pty_test};

const BIN: &str = env!("CARGO_BIN_EXE_omsh-pty-shell");

/// A program called `name` in `dir` that runs `body`.
fn script(dir: &Path, name: &str, body: &str) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n"))?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
}

/// A repository (only `HEAD` is needed: the branch is read from it) on branch `main`.
fn repo(ctx: &Ctx) -> io::Result<PathBuf> {
    let repo = ctx.home().join("repo");
    fs::create_dir_all(repo.join(".git"))?;
    fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n")?;
    Ok(repo)
}

fn write_config(ctx: &Ctx, text: &str) -> io::Result<()> {
    let dir = ctx.home().join(".config/oh-my-shell");
    fs::create_dir_all(&dir)?;
    fs::write(dir.join("config.toml"), text)
}

/// A directory that is the whole `PATH` of a shell: the harness starts the shell with `setsid`,
/// which has to be found there, and nothing else is.
fn only_path(dir: &Path) -> io::Result<String> {
    fs::create_dir_all(dir)?;
    let usual = std::env::var("PATH").unwrap_or_default();
    let setsid = usual
        .split(':')
        .map(|d| Path::new(d).join("setsid"))
        .find(|p| p.exists())
        .ok_or_else(|| io::Error::other("no setsid on PATH"))?;
    let link = dir.join("setsid");
    if !link.exists() {
        std::os::unix::fs::symlink(setsid, link)?;
    }
    Ok(dir.display().to_string())
}

/// A shell in `cwd` whose `PATH` is `bin`, then the usual directories.
fn launch(ctx: &Ctx, bin: &Path) -> Launch {
    let usual = std::env::var("PATH").unwrap_or_default();
    ctx.launch()
        .env("PATH", &format!("{}:{usual}", bin.display()))
}

/// Waits until the text the terminal has shown so far (escape sequences left out) holds `text`.
/// [`Launch::start`] has already read up to the first prompt, so the header drawn before it is
/// in the transcript and not after the cursor.
fn wait_shown(term: &mut Session, text: &str) {
    let end = Instant::now() + SHORT;
    loop {
        let (plain, _) = omsh_tests::session::strip_escapes(term.transcript());
        if plain.contains(text) {
            return;
        }
        assert!(
            Instant::now() < end,
            "never saw {text:?}; the terminal showed:\n{}",
            term.transcript()
        );
        term.pump(Duration::from_millis(20));
    }
}

/// The prompt shows the git segment next to the directory (this is what the defaults do too).
const WITH_GIT: &str = "[prompt]\nleft = [\"cwd\", \"git\"]\n";

#[test]
fn a_changed_repository_shows_a_star_after_the_branch_and_git_is_called_the_safe_way() {
    pty_test(BIN, |ctx| {
        let repo = repo(ctx)?;
        let bin = ctx.home().join("bin");
        let log = ctx.home().join("git-args");
        script(
            &bin,
            "git",
            &format!("echo \"$*\" >> {}\necho ' M file'", log.display()),
        )?;
        write_config(ctx, WITH_GIT)?;
        let mut term = launch(ctx, &bin).cwd(&repo).start()?;
        wait_shown(&mut term, "(main*)");
        term.sync("shown")?;
        term.quit()?;
        let args = fs::read_to_string(&log)?;
        for wanted in [
            "--no-optional-locks",
            "core.fsmonitor=false",
            "core.hooksPath=/dev/null",
            "status --porcelain=v1",
        ] {
            assert!(args.contains(wanted), "{wanted} missing from: {args}");
        }
        Ok(())
    });
}

#[test]
fn a_clean_repository_shows_the_branch_alone() {
    pty_test(BIN, |ctx| {
        let repo = repo(ctx)?;
        let bin = ctx.home().join("bin");
        script(&bin, "git", "exit 0")?;
        write_config(ctx, WITH_GIT)?;
        let mut term = launch(ctx, &bin).cwd(&repo).start()?;
        wait_shown(&mut term, "(main)");
        term.sync("shown")?;
        assert!(
            !term.transcript().contains("(main*)"),
            "a star on a clean repository\n{}",
            term.transcript()
        );
        term.quit()
    });
}

/// `git` that fails, or is not there at all, leaves the branch and nothing else.
#[test]
fn a_git_that_fails_or_is_missing_leaves_the_branch_alone() {
    pty_test(BIN, |ctx| {
        let repo = repo(ctx)?;
        let bin = ctx.home().join("bin");
        script(&bin, "git", "echo 'fatal: not today' >&2\nexit 128")?;
        write_config(ctx, WITH_GIT)?;
        let mut term = launch(ctx, &bin).cwd(&repo).start()?;
        wait_shown(&mut term, "(main)");
        term.sync("shown")?;
        assert!(!term.transcript().contains("(main*)"));
        assert!(
            !term.transcript().contains("not today"),
            "git's own messages never reach the screen"
        );
        term.quit()
    });
    pty_test(BIN, |ctx| {
        let repo = repo(ctx)?;
        write_config(ctx, WITH_GIT)?;
        // Only an empty directory on PATH: no git to find.
        let empty = only_path(&ctx.home().join("empty"))?;
        let mut term = ctx.launch().env("PATH", &empty).cwd(&repo).start()?;
        wait_shown(&mut term, "(main)");
        term.sync("shown")?;
        term.quit()
    });
}

/// The marker may come after the prompt: the editor repaints the prompt in place when `git`
/// answers late (here after 0.05 s, longer than the editor waits before the first paint and
/// shorter than the 150 ms limit).
#[test]
fn a_marker_that_comes_late_is_painted_after_the_prompt() {
    pty_test(BIN, |ctx| {
        let repo = repo(ctx)?;
        let bin = ctx.home().join("bin");
        script(&bin, "git", "sleep 0.05\necho ' M file'")?;
        write_config(ctx, WITH_GIT)?;
        let mut term = launch(ctx, &bin).cwd(&repo).start()?;
        wait_shown(&mut term, "(main*)");
        // The prompt was drawn first with the branch alone (`git` takes longer than the editor
        // waits), and the star came with a repaint.
        let (plain, _) = omsh_tests::session::strip_escapes(term.transcript());
        assert!(
            plain.contains("(main)"),
            "no first paint without the star\n{plain}"
        );
        term.sync("shown")?;
        term.quit()
    });
}

/// The point of the segment: a `git` that never answers does not hold the prompt, is ended at
/// the limit, and leaves no process behind.
#[test]
fn a_git_that_hangs_does_not_hold_the_prompt_and_is_ended() {
    pty_test(BIN, |ctx| {
        let repo = repo(ctx)?;
        let bin = ctx.home().join("bin");
        let pid_file = ctx.home().join("git-pid");
        script(
            &bin,
            "git",
            &format!("echo $$ > {}\nexec sleep 30", pid_file.display()),
        )?;
        write_config(ctx, WITH_GIT)?;
        let started = Instant::now();
        let mut term = launch(ctx, &bin).cwd(&repo).start()?;
        // The prompt (with its branch) is there long before the 30 seconds `git` would take.
        wait_shown(&mut term, "(main)");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the prompt waited for git: {:?}",
            started.elapsed()
        );
        // The shell is usable at once.
        term.line("echo alive-$((6*7))")?;
        term.expect("alive-42", SHORT);

        let pid = fs::read_to_string(&pid_file)?.trim().to_owned();
        let gone = Instant::now() + SHORT;
        while Path::new(&format!("/proc/{pid}")).exists() {
            assert!(Instant::now() < gone, "git {pid} is still there");
            term.pump(Duration::from_millis(20));
        }
        assert!(!term.transcript().contains("(main*)"));
        term.quit()
    });
}

/// The shell ends while the prompt's `git` is still running (well before its 150 ms limit). `git`
/// has a group of its own and the helper threads die with the process, so it used to be left
/// behind; found by a CI test that quits right after a prompt (S19b).
#[test]
fn a_git_that_runs_when_the_shell_exits_is_ended_with_it() {
    pty_test(BIN, |ctx| {
        let repo = repo(ctx)?;
        let bin = ctx.home().join("bin");
        let pid_file = ctx.home().join("git-pid");
        script(
            &bin,
            "git",
            &format!("echo $$ > {}\nexec sleep 30", pid_file.display()),
        )?;
        write_config(ctx, WITH_GIT)?;
        let mut term = launch(ctx, &bin).cwd(&repo).start()?;
        let end = Instant::now() + SHORT;
        while fs::read_to_string(&pid_file).map_or(true, |text| text.trim().is_empty()) {
            assert!(Instant::now() < end, "git was never started");
            term.pump(Duration::from_millis(5));
        }
        let pid = fs::read_to_string(&pid_file)?.trim().to_owned();
        // Out at once, long before the deadline of the prompt's `git`.
        term.quit()?;
        assert!(
            !Path::new(&format!("/proc/{pid}")).exists(),
            "git {pid} outlived the shell"
        );
        Ok(())
    });
}

#[test]
fn git_is_not_run_outside_a_repository_or_without_the_segment() {
    pty_test(BIN, |ctx| {
        let bin = ctx.home().join("bin");
        let mark = ctx.home().join("git-was-run");
        script(&bin, "git", &format!("touch {}", mark.display()))?;
        // Not in a repository.
        write_config(ctx, WITH_GIT)?;
        let plain = ctx.home().join("plain");
        fs::create_dir_all(&plain)?;
        let mut term = launch(ctx, &bin).cwd(&plain).start()?;
        term.line("cd ..")?;
        term.sync("moved")?;
        assert!(!mark.exists(), "git ran outside a repository");
        term.quit()?;

        // In a repository, but the prompt has no git segment.
        let repo = repo(ctx)?;
        write_config(ctx, "[prompt]\nleft = [\"cwd\"]\n")?;
        let mut term = launch(ctx, &bin).cwd(&repo).start()?;
        term.sync("shown")?;
        assert!(!mark.exists(), "git ran without a git segment");
        term.quit()
    });
}

/// A repository whose `HEAD` cannot be read has no branch, so nothing is shown and `git` is not
/// asked about it.
#[test]
fn an_unreadable_head_shows_nothing() {
    pty_test(BIN, |ctx| {
        let repo = repo(ctx)?;
        fs::write(repo.join(".git/HEAD"), "garbage\n")?;
        let bin = ctx.home().join("bin");
        let mark = ctx.home().join("git-was-run");
        script(
            &bin,
            "git",
            &format!("touch {}\necho ' M x'", mark.display()),
        )?;
        write_config(ctx, WITH_GIT)?;
        let mut term = launch(ctx, &bin).cwd(&repo).start()?;
        term.sync("shown")?;
        assert!(!mark.exists());
        assert!(!term.transcript().contains("(garbage"));
        term.quit()
    });
}

// --- The transient prompt ---------------------------------------------------------------------

/// With `transient = true` the line that was just entered is drawn again with the symbol alone;
/// without it the whole prompt (header included) is drawn again. The harness has no screen, so
/// this looks at what is written between pressing Enter and the command's output.
#[test]
fn the_transient_prompt_redraws_the_entered_line_without_its_header() {
    for transient in [false, true] {
        pty_test(BIN, move |ctx| {
            let dir = ctx.home().join("omshheader");
            fs::create_dir_all(&dir)?;
            write_config(
                ctx,
                &format!("[prompt]\nleft = [\"cwd\"]\ntransient = {transient}\n"),
            )?;
            let mut term = ctx.launch().cwd(&dir).start()?;
            term.write_bytes(b"echo one-$((1+1))")?;
            term.expect_plain("echo one-$((1+1))", SHORT);
            // Let the typing repaints end, so that only Enter's is counted.
            term.pump(Duration::from_millis(300));
            let from = term.transcript().len();
            term.write_bytes(b"\r")?;
            term.expect("one-2", SHORT);
            // The next prompt is drawn; nothing else is run, so that it is the only one.
            term.pump(Duration::from_millis(400));

            // `ESC [ ? 2004 l` turns bracketed paste off: the editor has finished the line and
            // the command starts. What came before it is the last drawing of the entered line.
            let after = &term.transcript()[from..];
            let (entered, _) = after.split_once("\x1b[?2004l").expect("the line was run");
            let (entered, _) = omsh_tests::session::strip_escapes(entered);
            assert!(
                entered.contains("$ echo one-$((1+1))"),
                "transient={transient}: {entered:?}"
            );
            assert_eq!(
                entered.contains("omshheader"),
                !transient,
                "transient={transient}: the header is redrawn only without it: {entered:?}"
            );
            // The next prompt is a full one either way.
            let (rest, _) = omsh_tests::session::strip_escapes(after);
            assert_eq!(
                rest.matches("omshheader").count(),
                if transient { 1 } else { 2 }
            );
            term.quit()
        });
    }
}

// --- "Did you mean" --------------------------------------------------------------------------

#[test]
fn a_mistyped_command_gets_a_suggestion_but_the_startup_file_does_not() {
    pty_test(BIN, |ctx| {
        let bin = ctx.home().join("bin");
        script(&bin, "git", "echo fake-git")?;
        // `PATH` is only `bin` (and `setsid`), so the one close name is `git`.
        let only = only_path(&bin)?;
        let config = ctx.home().join(".config/oh-my-shell");
        fs::create_dir_all(&config)?;
        fs::write(config.join("init.omsh"), "gti\n")?;
        let mut term = ctx.launch().env("PATH", &only).cwd(ctx.home()).start()?;
        // The startup file ran `gti`: not found, and no advice.
        term.sync("started")?;
        assert!(
            term.transcript().contains("gti: not found"),
            "{}",
            term.transcript()
        );
        assert!(
            !term.transcript().contains("did you mean"),
            "the startup file got a suggestion\n{}",
            term.transcript()
        );
        let typed = term.transcript().len();
        term.line("gti")?;
        term.expect_plain("oh-my-shell: gti: not found", SHORT);
        term.expect_plain("oh-my-shell: did you mean git?", SHORT);
        term.line("echo status-$?")?;
        term.expect("status-127", SHORT);
        assert!(term.transcript()[typed..].matches("did you mean").count() == 1);
        // Nothing close: no line.
        let before = term.transcript().len();
        term.line("zzzzzz")?;
        term.expect_plain("oh-my-shell: zzzzzz: not found", SHORT);
        term.sync("again")?;
        assert!(
            !term.transcript()[before..].contains("did you mean"),
            "{}",
            term.transcript()
        );
        term.quit()
    });
}
