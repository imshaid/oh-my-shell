//! The line editor and the prompt (S13a), on a terminal that takes escape codes.
//!
//! The harness starts the shell with `TERM=xterm-256color`, so the editor is the normal reader
//! here. `Launch::dumb()` starts the plain reader instead. The terminal does not draw anything:
//! it answers the editor's cursor question with a fixed position (see `Session`), and tests
//! look for text in what the shell printed.

use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use nix::sys::signal::{Signal, kill};
use nix::sys::termios::LocalFlags;
use nix::unistd::Pid;
use omsh_tests::procfs::hup_is_ignored;
use omsh_tests::skip::skip;
use omsh_tests::{Ctx, SHORT, Session, pty_test};

const BIN: &str = env!("CARGO_BIN_EXE_omsh-pty-shell");

const CTRL_C: u8 = 3;
const CTRL_D: u8 = 4;
const CTRL_Z: u8 = 0x1a;
/// Up arrow, as a terminal sends it.
const UP: &[u8] = b"\x1b[A";
const ESC: u8 = 0x1b;

/// The question "where is the cursor?": only the line editor asks it, before each prompt.
const EDITOR_ONLY: &str = "\x1b[6n";

fn write_config(ctx: &Ctx, text: &str) -> io::Result<PathBuf> {
    let dir = ctx.home().join(".config/oh-my-shell");
    fs::create_dir_all(&dir)?;
    let file = dir.join("config.toml");
    fs::write(&file, text)?;
    Ok(file)
}

/// The editor is the reader: it asks the terminal where the cursor is, before it draws.
#[test]
fn the_editor_is_used_on_a_terminal_that_takes_escape_codes() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        assert!(
            term.transcript().contains(EDITOR_ONLY),
            "{}",
            term.transcript()
        );
        term.quit()
    });
}

/// The plain reader is used when `TERM` is `dumb`, empty, or the shell runs inside Emacs.
#[test]
fn the_plain_reader_is_used_for_a_dumb_terminal_and_inside_emacs() {
    pty_test(BIN, |ctx| {
        for (label, launch) in [
            ("TERM=dumb", ctx.launch().dumb()),
            ("TERM empty", ctx.launch().env("TERM", "")),
            (
                "INSIDE_EMACS",
                ctx.launch().env("INSIDE_EMACS", "29.1,comint"),
            ),
        ] {
            let mut term = launch.start()?;
            term.line("echo plain-$((2*3))")?;
            term.expect("plain-6", SHORT);
            let shown = term.transcript();
            assert!(!shown.contains(EDITOR_ONLY), "{label}: {shown}");
            term.quit()?;
        }
        Ok(())
    });
}

/// Up arrow brings back the last command; Enter runs it again.
#[test]
fn up_arrow_recalls_the_previous_command() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        // The output `up-2` differs from the typed text `up-$((1+1))`.
        term.line("echo up-$((1+1))")?;
        term.expect("up-2", SHORT);
        term.write_bytes(UP)?;
        term.write_bytes(b"\r")?;
        term.expect("up-2", SHORT);
        term.quit()
    });
}

/// A line that starts with a space is not remembered (the default `[history] ignore`). As in
/// zsh, it can still be recalled once, right after it ran, to fix a typing mistake; after the
/// next command it is gone.
#[test]
fn a_line_with_a_leading_space_is_not_remembered() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.line("echo kept-$((1+1))")?;
        term.expect("kept-2", SHORT);
        term.line(" echo hidden-$((1+1))")?;
        term.expect("hidden-2", SHORT);
        term.line("echo other-$((1+1))")?;
        term.expect("other-2", SHORT);
        // Up once gives `other`, twice gives `kept`: `hidden` is not in between.
        term.write_bytes(UP)?;
        term.write_bytes(UP)?;
        term.write_bytes(b"\r")?;
        term.expect("kept-2", SHORT);
        let shown = term.transcript();
        assert_eq!(shown.matches("hidden-2").count(), 1, "{shown}");
        assert_eq!(shown.matches("kept-2").count(), 2, "{shown}");
        term.quit()
    });
}

/// Enter does not run a command that is not finished; the next line is part of it.
#[test]
fn enter_waits_for_an_unfinished_command() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.line("if true; then")?;
        term.expect("> ", SHORT);
        term.line("echo inner-$((1+1))")?;
        // Nothing runs before the `fi`.
        assert!(
            !term.try_expect("inner-2", Duration::from_millis(400)),
            "{}",
            term.transcript()
        );
        term.line("fi")?;
        term.expect("inner-2", SHORT);
        // The whole command is one history entry, so Up brings it all back (if each line were
        // its own entry, Up would give only `fi`, which is a syntax error).
        term.write_bytes(UP)?;
        term.write_bytes(b"\r")?;
        term.expect("inner-2", SHORT);
        term.quit()
    });
}

/// An open quote and a trailing backslash also wait; a here-document waits for its end word.
#[test]
fn quotes_backslashes_and_here_documents_wait_too() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.line("echo 'one")?;
        term.line("two'")?;
        term.expect("one", SHORT);
        term.expect("two", SHORT);

        term.line("echo joined-\\")?;
        term.line("$((1+1))")?;
        term.expect("joined-2", SHORT);

        term.line("cat <<EOF")?;
        term.line("body-$((1+1))")?;
        term.line("EOF")?;
        term.expect("body-2", SHORT);
        term.quit()
    });
}

/// A real mistake is not waited for: the shell reports it at once and the next line works.
#[test]
fn a_syntax_error_is_reported_and_not_waited_for() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.line("echo )")?;
        term.expect("syntax", SHORT);
        term.sync("after-error")?;
        term.quit()
    });
}

/// Ctrl+C drops a half-typed line and leaves `$?` alone.
#[test]
fn ctrl_c_drops_the_line_and_keeps_the_status() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.line("false")?;
        term.sync("one")?;
        // Typed, not run: `gone-2` would be the output.
        term.write_bytes(b"echo gone-$((1+1))")?;
        term.key(CTRL_C)?;
        term.pump(Duration::from_millis(300));
        term.line("echo st-$?")?;
        term.expect("st-0", SHORT);
        assert!(
            !term.transcript().contains("gone-2"),
            "{}",
            term.transcript()
        );
        term.quit()
    });
}

/// Ctrl+C at the prompt of a continuation line drops the whole unfinished command.
#[test]
fn ctrl_c_drops_an_unfinished_command() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.line("if true; then")?;
        term.expect("> ", SHORT);
        term.key(CTRL_C)?;
        term.pump(Duration::from_millis(300));
        // If `if true; then` were still pending, this would be part of it.
        term.line("echo fresh-$((1+1))")?;
        term.expect("fresh-2", SHORT);
        term.quit()
    });
}

/// Ctrl+D on an empty line ends the shell with the status of the last command.
#[test]
fn ctrl_d_ends_the_shell_with_the_last_status() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.line("false")?;
        term.pump(Duration::from_millis(300));
        term.key(CTRL_D)?;
        let status = term.wait_exit(SHORT);
        assert_eq!(status.code(), Some(1), "{}", term.transcript());
        Ok(())
    });
}

/// Ctrl+D with a stopped job warns once; the second one leaves.
#[test]
fn ctrl_d_warns_about_stopped_jobs_once() {
    if hup_is_ignored() {
        skip(
            "ctrl_d_warns_about_stopped_jobs_once",
            "SIGHUP is ignored here (started under nohup?)",
        );
        return;
    }
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.command("sleep 30")?;
        term.pump(Duration::from_millis(400));
        term.key(CTRL_Z)?;
        term.expect("Stopped", SHORT);
        term.sync("ready")?;
        term.key(CTRL_D)?;
        term.expect("There are stopped jobs.", SHORT);
        assert!(!term.has_exited()?);
        // The key must not come before the prompt: the editor would not be reading it.
        term.expect("$ ", SHORT);
        term.key(CTRL_D)?;
        term.wait_exit(SHORT);
        term.expect_none("sleep");
        Ok(())
    });
}

/// Ctrl+Z at a running program stops it; `fg` goes on; the prompt comes back each time.
#[test]
fn ctrl_z_and_fg_work_with_the_editor() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.line("sleep 30")?;
        term.pump(Duration::from_millis(400));
        term.key(CTRL_Z)?;
        term.expect("Stopped", SHORT);
        term.sync("stopped")?;
        term.line("fg")?;
        term.pump(Duration::from_millis(300));
        term.key(CTRL_C)?;
        term.sync("back")?;
        term.quit()
    });
}

/// A `SIGHUP` sent to a shell that waits in the editor ends it with status 129.
#[test]
fn sighup_ends_an_idle_editor() {
    if hup_is_ignored() {
        skip(
            "sighup_ends_an_idle_editor",
            "SIGHUP is ignored here (started under nohup?)",
        );
        return;
    }
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.sync("idle")?;
        term.signal_parent(Signal::SIGHUP)?;
        let status = term.wait_exit(SHORT);
        assert_eq!(status.code(), Some(129), "{}", term.transcript());
        Ok(())
    });
}

/// Closing the terminal (a window closed, an ssh session dropped) ends a shell that waits in the
/// editor. The editor's input library would spin at full speed on the dead terminal for ever;
/// this was found by closing a real terminal, not by the tests above, which only sent `SIGHUP`.
#[test]
fn closing_the_terminal_ends_an_idle_editor() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.sync("idle")?;
        term.hang_up_terminal()?;
        let status = term.wait_exit(SHORT);
        assert_eq!(status.code(), Some(129), "{}", term.transcript());
        Ok(())
    });
}

/// The same when `SIGHUP` is ignored (the shell was started with `nohup`): only the dead
/// terminal tells the shell to stop, and it must.
#[test]
fn closing_the_terminal_ends_the_editor_even_when_sighup_is_ignored() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.launch().with_hangup_ignored().start()?;
        term.sync("idle")?;
        term.hang_up_terminal()?;
        let status = term.wait_exit(SHORT);
        assert_eq!(status.code(), Some(129), "{}", term.transcript());
        Ok(())
    });
}

/// The same in the middle of a command that is not finished.
#[test]
fn closing_the_terminal_ends_the_editor_at_a_continuation_line() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.line("if true; then")?;
        term.expect("> ", SHORT);
        term.hang_up_terminal()?;
        let status = term.wait_exit(SHORT);
        assert_eq!(status.code(), Some(129), "{}", term.transcript());
        Ok(())
    });
}

/// A `SIGHUP` on a terminal that is still there ends the line cleanly: the terminal is back in
/// line mode when the shell is gone.
#[test]
fn sighup_leaves_the_terminal_in_line_mode() {
    if hup_is_ignored() {
        skip(
            "sighup_leaves_the_terminal_in_line_mode",
            "SIGHUP is ignored here (started under nohup?)",
        );
        return;
    }
    pty_test(BIN, |ctx| {
        let mut term = ctx.launch().under_sh().start()?;
        term.sync("idle")?;
        kill(Pid::from_raw(term.shell_pid() as i32), Signal::SIGHUP)?;
        term.expect("parent-saw-129", SHORT);
        let flags = term.termios()?.local_flags;
        assert!(
            flags.contains(LocalFlags::ICANON | LocalFlags::ECHO),
            "{flags:?}\n{}",
            term.transcript()
        );
        term.line("go")?;
        term.expect("parent-read-ok", SHORT);
        Ok(())
    });
}

/// When the shell is gone the terminal is in line mode again, for whoever reads next.
#[test]
fn the_terminal_is_in_line_mode_again_after_the_shell_ends() {
    pty_test(BIN, |ctx| {
        for ending in ["exit", "ctrl-d"] {
            let mut term = ctx.launch().under_sh().start()?;
            term.sync("one")?;
            if ending == "exit" {
                term.command("exit")?;
            } else {
                term.key(CTRL_D)?;
            }
            term.expect("parent-saw-", SHORT);
            let flags = term.termios()?.local_flags;
            assert!(
                flags.contains(LocalFlags::ICANON | LocalFlags::ECHO),
                "{ending}: {flags:?}\n{}",
                term.transcript()
            );
            term.line("go")?;
            term.expect("parent-read-ok", SHORT);
        }
        Ok(())
    });
}

/// `[editor] mode = "vi"`: Esc leaves insert mode (the indicator becomes `:`), and `dd`
/// deletes the line.
#[test]
fn vi_mode_comes_from_the_configuration() {
    pty_test(BIN, |ctx| {
        write_config(ctx, "[editor]\nmode = \"vi\"\n")?;
        let mut term = ctx.start()?;
        term.write_bytes(b"echo vi-$((1+1))")?;
        term.key(ESC)?;
        term.expect(": ", SHORT);
        term.write_bytes(b"dd")?;
        term.write_bytes(b"i")?;
        term.line("echo ok-$((1+1))")?;
        term.expect("ok-2", SHORT);
        assert!(!term.transcript().contains("vi-2"), "{}", term.transcript());
        term.quit()
    });
}

/// The right side shows the status of a failed command and the time of a slow one.
#[test]
fn the_right_prompt_shows_the_status_and_a_long_duration() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.line("false")?;
        // ASCII icons (the C locale of the harness): `x 1`.
        term.expect("x 1", SHORT);
        term.line("sleep 2.2")?;
        term.expect("2s", SHORT);
        term.quit()
    });
}

/// The prompt has two lines: the header (directory with `~` for the home directory, then the git
/// branch) and, below it, the symbol where the typing happens.
#[test]
fn the_prompt_is_a_header_line_and_a_symbol_line() {
    pty_test(BIN, |ctx| {
        let repo = ctx.home().join("proj");
        fs::create_dir_all(repo.join(".git"))?;
        fs::write(repo.join(".git/HEAD"), "ref: refs/heads/topic-x\n")?;
        let mut term = ctx.launch().cwd(&repo).start_raw()?;
        term.expect_plain("~/proj (topic-x)\r\n$ ", SHORT);
        term.line("cd /")?;
        term.expect_plain("/\r\n$ ", SHORT);
        term.quit()
    });
}

/// True if the output sets a foreground or background color (30-38, 40-48, 90-97, 100-107), as
/// opposed to bold, dim, reset or the terminal's default color (39, 49).
fn sets_a_color(shown: &str) -> bool {
    let mut rest = shown;
    while let Some(at) = rest.find("\x1b[") {
        rest = &rest[at + 2..];
        let end = rest
            .find(|c: char| !(c.is_ascii_digit() || c == ';'))
            .unwrap_or(0);
        let colored = rest[..end]
            .split(';')
            .filter_map(|code| code.parse::<u16>().ok())
            .any(|code| matches!(code, 30..=38 | 40..=48 | 90..=97 | 100..=107));
        if colored {
            return true;
        }
    }
    false
}

/// No color is forced. After a success the prompt sets no color at all, only bold (the
/// directory) and dim (the rest of the header). After a failure the symbol is ANSI red
/// (`ESC[31m`, the terminal's own red) and nothing else is colored.
#[test]
fn the_symbol_is_red_only_after_a_failure_and_nothing_else_is_colored() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.launch().env("NO_COLOR", "").start_raw()?;
        term.expect("$ ", SHORT);
        assert!(!sets_a_color(term.transcript()), "{:?}", term.transcript());
        assert!(
            term.transcript().contains("\x1b[1m"),
            "the directory is bold"
        );
        term.line("false")?;
        term.expect("\x1b[31m$ \x1b[0m", SHORT);
        // The red is on the symbol alone: the dim status (`x 1`) next to it has none.
        term.expect("\x1b[2mx 1\x1b[0m", SHORT);
        let shown = term.transcript().replace("\x1b[31m$ \x1b[0m", "");
        assert!(!sets_a_color(&shown), "{shown:?}");
        // A success takes the red away again.
        term.line("true")?;
        term.expect("$ ", SHORT);
        term.quit()
    });
}

/// `NO_COLOR` removes the red and keeps bold and dim, which are not colors.
#[test]
fn no_color_keeps_bold_and_dim() {
    pty_test(BIN, |ctx| {
        // The harness sets NO_COLOR=1 by default.
        let mut term = ctx.start()?;
        term.line("false")?;
        term.expect("\x1b[2mx 1\x1b[0m", SHORT);
        term.sync("again")?;
        assert!(term.transcript().contains("\x1b[1m"), "bold stays");
        assert!(!sets_a_color(term.transcript()), "{:?}", term.transcript());
        term.quit()
    });
}

/// A broken `config.toml` still starts the editor; the settings that are good apply.
#[test]
fn a_broken_config_starts_the_editor_too() {
    pty_test(BIN, |ctx| {
        write_config(
            ctx,
            "[ui]\nicons = \"unicode\"\nmotion = \"fast\"\n\n[editor]\nhints = 3\n",
        )?;
        let mut term = ctx.launch().start_raw()?;
        term.expect("ui.motion must be", SHORT);
        term.expect("editor.hints", SHORT);
        // `unicode` applies: the arrow of the prompt.
        term.expect("❯ ", SHORT);
        term.line("echo alive-$((6*7))")?;
        term.expect("alive-42", SHORT);
        term.line("echo )")?;
        term.expect("╭", SHORT);
        term.quit()
    });
}

/// `[prompt]` settings are read: segments, order, and `transient`.
#[test]
fn prompt_settings_come_from_the_configuration() {
    pty_test(BIN, |ctx| {
        write_config(
            ctx,
            "[prompt]\nleft = [\"status\"]\nright = []\ntransient = true\n",
        )?;
        let mut term = ctx.launch().cwd("/").start_raw()?;
        // No directory segment: the line starts with the indicator.
        term.expect_plain("$ ", SHORT);
        let (plain, _) = omsh_tests::session::strip_escapes(term.transcript());
        assert!(
            !plain.contains("/\r\n"),
            "the cwd segment was configured away, so there is no header line\n{plain}"
        );
        term.sync("configured")?;
        term.quit()
    });
}

/// A terminal that never answers the cursor question is slow (the editor waits two seconds for
/// each question) but not stuck: the prompt comes, commands run, and a program in front gets
/// its lines. Real terminals answer; this pins that a broken one does not hang the shell.
#[test]
fn a_terminal_that_never_answers_the_cursor_question_is_slow_but_works() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.launch().no_cursor_replies().start_raw()?;
        term.expect_plain("$ ", SHORT);
        term.line("echo plain-$((2*3))")?;
        term.expect("plain-6", SHORT);
        term.expect_plain("$ ", SHORT);
        term.quit()
    });
}

/// The editor must not leak descriptors to the programs it starts: a program sees as many under
/// the editor as under the plain reader. (Both see the harness's own terminal descriptor, so the
/// count is compared and not the numbers.)
#[test]
fn programs_see_as_many_descriptors_under_the_editor_as_under_the_plain_reader() {
    /// The number in `n=<digits>=` in the output. The typed text is `n=$(`, which has no digits.
    fn printed_count(shown: &str) -> Option<usize> {
        shown.split("n=").skip(1).find_map(|part| {
            let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
            let closed = part[digits.len()..].starts_with('=');
            (closed && !digits.is_empty()).then(|| digits.parse().ok())?
        })
    }
    fn count(term: &mut Session) -> io::Result<usize> {
        term.line("echo n=$(ls /proc/self/fd | wc -l)=")?;
        let end = std::time::Instant::now() + SHORT;
        loop {
            if let Some(n) = printed_count(term.transcript()) {
                return Ok(n);
            }
            assert!(
                std::time::Instant::now() < end,
                "no count shown\n{}",
                term.transcript()
            );
            term.pump(Duration::from_millis(50));
        }
    }
    pty_test(BIN, |ctx| {
        let mut edited = ctx.start()?;
        let with_editor = count(&mut edited)?;
        edited.quit()?;
        let mut plain = ctx.launch().dumb().start()?;
        let without = count(&mut plain)?;
        plain.quit()?;
        assert!(with_editor >= 4, "the count was not read: {with_editor}");
        assert_eq!(with_editor, without);
        Ok(())
    });
}
