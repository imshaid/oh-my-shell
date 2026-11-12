//! The command palette on `Ctrl+P` (S27b), on a terminal that takes escape codes.
//!
//! As with the history explorer, the harness keeps the bytes the shell wrote and does not draw a
//! screen, so these tests look at what the palette *does*: what ends up in the line, what runs,
//! which view opens next, how the terminal is left. How it looks is tested on a test backend in
//! `omsh-ui`, and was looked at in tmux.

use std::fs;
use std::time::Duration;

use nix::sys::signal::Signal;
use nix::sys::termios::LocalFlags;
use omsh_tests::procfs::hup_is_ignored;
use omsh_tests::skip::skip;
use omsh_tests::{SHORT, Session, pty_test};

const BIN: &str = env!("CARGO_BIN_EXE_omsh-pty-shell");

const CTRL_A: u8 = 1;
const CTRL_C: u8 = 3;
const CTRL_P: u8 = 0x10;
const ESC: u8 = 0x1b;

/// The terminal switches to and from the alternate screen.
const ENTER: &str = "\x1b[?1049h";
const LEAVE: &str = "\x1b[?1049l";

/// Presses `Ctrl+P` and waits until the palette is on the screen. The first drawing is written
/// top to bottom, and a blank in the terminal's default style is skipped when the screen is
/// drawn (so "Command palette" arrives as two pieces), while a blank inside dim text is written.
/// The last row of the first drawing is the keys line, whose end is `Up/Down move`.
fn open_palette(term: &mut Session) -> std::io::Result<()> {
    term.key(CTRL_P)?;
    term.expect(ENTER, SHORT);
    term.expect_plain("Up/Down move", SHORT);
    Ok(())
}

/// Waits until a view has given the screen back.
fn left_view(term: &mut Session) {
    term.expect(LEAVE, SHORT);
}

fn counts(term: &Session) -> (usize, usize) {
    let all = term.transcript();
    (all.matches(ENTER).count(), all.matches(LEAVE).count())
}

/// Defines the alias `zq`, which prints `zq-42`, and waits for the next prompt.
fn define_alias(term: &mut Session) -> std::io::Result<()> {
    term.command("alias zq='echo zq-$((6*7))'")?;
    term.prompt();
    Ok(())
}

/// A name chosen in the palette goes into the line (with a blank after it) and does not run;
/// Enter then runs it.
#[test]
fn an_alias_is_put_in_the_line_and_runs_only_after_enter() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        define_alias(&mut term)?;
        open_palette(&mut term)?;
        term.write_bytes(b"zq")?;
        term.write_bytes(b"\r")?;
        left_view(&mut term);
        assert!(
            !term.try_expect("zq-42", Duration::from_millis(400)),
            "the alias ran without Enter:\n{}",
            term.transcript()
        );
        term.write_bytes(b"\r")?;
        term.expect("zq-42", SHORT);
        term.prompt();
        term.quit()
    });
}

/// The text goes in at the cursor and does not replace the line: with the cursor at the start,
/// the alias is in command position and expands.
#[test]
fn a_name_goes_in_at_the_cursor_and_keeps_what_was_typed() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        define_alias(&mut term)?;
        term.write_bytes(b"echo end")?;
        term.key(CTRL_A)?;
        open_palette(&mut term)?;
        term.write_bytes(b"zq\r")?;
        left_view(&mut term);
        term.write_bytes(b"\r")?;
        // `zq echo end` is `echo zq-42 echo end`.
        term.expect("zq-42 echo end", SHORT);
        term.prompt();
        term.quit()
    });
}

/// A builtin is put in the line by name, at the cursor: `printf` in front of its arguments.
#[test]
fn a_builtin_is_put_in_the_line_by_name() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.write_bytes(b"'b-%s\\n' $((6*7))")?;
        term.key(CTRL_A)?;
        open_palette(&mut term)?;
        term.write_bytes(b"printf\r")?;
        left_view(&mut term);
        term.write_bytes(b"\r")?;
        term.expect("b-42", SHORT);
        term.prompt();
        term.quit()
    });
}

/// Escape and Ctrl+C leave the line as it was.
#[test]
fn escape_and_ctrl_c_keep_the_line() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.write_bytes(b"echo keep-$((2+3))")?;
        for (round, key) in [ESC, CTRL_C].into_iter().enumerate() {
            open_palette(&mut term)?;
            term.key(key)?;
            left_view(&mut term);
            assert_eq!(counts(&term), (round + 1, round + 1));
        }
        term.write_bytes(b"\r")?;
        term.expect("keep-5", SHORT);
        term.prompt();
        term.quit()
    });
}

/// "History explorer" opens the explorer; leaving it comes back to the line.
#[test]
fn the_history_explorer_entry_opens_the_explorer() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.wait_for_history()?;
        term.write_bytes(b"echo still-$((1+1))")?;
        open_palette(&mut term)?;
        term.write_bytes(b"History explorer\r")?;
        // The palette gave the screen back and the explorer took it.
        term.expect_plain("Looks at the newest 10,000 runs only", SHORT);
        assert_eq!(counts(&term), (2, 1), "{}", term.transcript());
        term.key(ESC)?;
        left_view(&mut term);
        assert_eq!(counts(&term), (2, 2));
        term.write_bytes(b"\r")?;
        term.expect("still-2", SHORT);
        term.prompt();
        term.quit()
    });
}

/// "Settings" opens the settings view.
#[test]
fn the_settings_entry_opens_the_settings_view() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        open_palette(&mut term)?;
        term.write_bytes(b"Settings\r")?;
        term.expect_plain("ui.theme", SHORT);
        assert_eq!(counts(&term), (2, 1), "{}", term.transcript());
        term.key(ESC)?;
        left_view(&mut term);
        term.sync("after")?;
        term.quit()
    });
}

/// "Explain the last failure" does what `Alt+?` does: with no AI set up it says so, and what was
/// typed stays in the editor.
#[test]
fn the_explain_entry_does_what_alt_question_does() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.command("false")?;
        term.prompt();
        term.write_bytes(b"echo typed-$((3+3))")?;
        open_palette(&mut term)?;
        term.write_bytes(b"Explain\r")?;
        left_view(&mut term);
        term.expect_plain("set up", SHORT);
        // Not a line: nothing ran, `$?` is still the failure, and the text is still there.
        std::thread::sleep(Duration::from_millis(500));
        term.write_bytes(b"\r")?;
        term.expect("typed-6", SHORT);
        term.prompt();
        term.quit()
    });
}

/// Entries are put in the line, never run: nothing the palette does adds to the history.
#[test]
fn the_palette_stores_nothing_in_the_history() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        define_alias(&mut term)?;
        term.wait_for_history()?;
        open_palette(&mut term)?;
        term.write_bytes(b"zq\r")?;
        left_view(&mut term);
        term.write_bytes(b"\x15")?;
        term.write_bytes(b"echo count=$(history --all | wc -l)\r")?;
        // The alias line and the line that counts.
        term.expect("count=2", SHORT);
        term.prompt();
        term.quit()
    });
}

/// "Edit config.toml" puts a line in the editor that runs `$VISUAL`, `$EDITOR` or `vi` on the
/// file.
#[test]
fn the_edit_entry_runs_the_editor_on_the_config_file() {
    pty_test(BIN, |ctx| {
        let mut term = ctx
            .launch()
            .env("VISUAL", "")
            .env("EDITOR", "/bin/true")
            .start()?;
        open_palette(&mut term)?;
        term.write_bytes(b"Edit config\r")?;
        left_view(&mut term);
        term.write_bytes(b"\r")?;
        term.prompt();
        term.command("echo status=$?")?;
        term.expect("status=0", SHORT);
        term.prompt();
        term.quit()
    });
}

/// With no configuration file (`--no-config`) there is nothing to edit, so the entry is not
/// listed.
#[test]
fn without_a_config_file_there_is_no_edit_entry() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.launch().arg("--no-config").start()?;
        open_palette(&mut term)?;
        term.write_bytes(b"Edit config")?;
        term.expect_plain("Nothing matches.", SHORT);
        term.key(ESC)?;
        left_view(&mut term);
        term.quit()
    });
}

/// `Ctrl+P` works in both vi modes.
#[test]
fn ctrl_p_works_in_both_vi_modes() {
    pty_test(BIN, |ctx| {
        let config = ctx.home().join(".config/oh-my-shell");
        fs::create_dir_all(&config)?;
        fs::write(config.join("config.toml"), "[editor]\nmode = \"vi\"\n")?;
        let mut term = ctx.start()?;
        define_alias(&mut term)?;
        // Insert mode.
        open_palette(&mut term)?;
        term.key(ESC)?;
        left_view(&mut term);
        // Normal mode: Escape in the editor (the prompt shows `: `), then the key.
        term.key(ESC)?;
        term.expect(": ", SHORT);
        term.key(CTRL_P)?;
        term.expect_plain("Up/Down move", SHORT);
        term.write_bytes(b"zq\r")?;
        assert_eq!(counts(&term), (2, 1));
        left_view(&mut term);
        term.write_bytes(b"\r")?;
        term.expect("zq-42", SHORT);
        term.quit()
    });
}

/// `Ctrl+P` used to be "previous history line"; the Up arrow still is.
#[test]
fn the_up_arrow_still_recalls_the_previous_line() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.sync("first")?;
        term.write_bytes(b"\x1b[A")?;
        term.write_bytes(b"\r")?;
        term.expect("first-2", SHORT);
        term.quit()
    });
}

/// A `SIGHUP` while the palette is open ends the shell (status 129), the screen is given back
/// and the terminal is in line mode when the shell is gone.
#[test]
fn sighup_while_the_palette_is_open_ends_the_shell_and_restores_the_terminal() {
    if hup_is_ignored() {
        skip(
            "sighup_while_the_palette_is_open_ends_the_shell_and_restores_the_terminal",
            "SIGHUP is ignored here (started under nohup?)",
        );
        return;
    }
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        open_palette(&mut term)?;
        term.signal_parent(Signal::SIGHUP)?;
        let status = term.wait_exit(SHORT);
        assert_eq!(status.code(), Some(129), "{}", term.transcript());
        assert!(term.transcript().contains(LEAVE), "{}", term.transcript());
        let flags = term.termios()?.local_flags;
        assert!(
            flags.contains(LocalFlags::ICANON | LocalFlags::ECHO),
            "{flags:?}"
        );
        Ok(())
    });
}

/// Closing the terminal while the palette is open ends the shell too.
#[test]
fn closing_the_terminal_while_the_palette_is_open_ends_the_shell() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        open_palette(&mut term)?;
        term.hang_up_terminal()?;
        let status = term.wait_exit(SHORT);
        assert_eq!(status.code(), Some(129));
        Ok(())
    });
}

/// The screen is given back once for each time it was taken, and the terminal mode is as before.
#[test]
fn leaving_the_palette_gives_the_screen_and_the_terminal_mode_back() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.sync("before")?;
        let before = term.termios()?.local_flags;
        for round in 1..=3 {
            open_palette(&mut term)?;
            term.key(ESC)?;
            left_view(&mut term);
            assert_eq!(counts(&term), (round, round));
        }
        term.sync("after")?;
        assert_eq!(term.termios()?.local_flags, before, "{}", term.transcript());
        term.quit()
    });
}
