//! The settings view (S27b), opened from the command palette, on a terminal that takes escape
//! codes. A change is written to `config.toml` and applies when a new shell starts, so these
//! tests look at the file and at the next shell, which is where the view's work shows. How the
//! rows look is tested on a test backend in `omsh-ui`.

use std::fs;
use std::path::PathBuf;

use nix::sys::signal::Signal;
use omsh_tests::procfs::hup_is_ignored;
use omsh_tests::skip::skip;
use omsh_tests::{Ctx, SHORT, Session, pty_test};

const BIN: &str = env!("CARGO_BIN_EXE_omsh-pty-shell");

const CTRL_P: u8 = 0x10;
const ESC: u8 = 0x1b;

const ENTER: &str = "\x1b[?1049h";
const LEAVE: &str = "\x1b[?1049l";

fn config_file(ctx: &Ctx) -> PathBuf {
    ctx.home().join(".config/oh-my-shell/config.toml")
}

fn write_config(ctx: &Ctx, text: &str) -> std::io::Result<()> {
    let file = config_file(ctx);
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(file, text)
}

/// Opens the settings view through the palette and waits for its first drawing, which ends with
/// the keys line (a blank in the default style is not written, a blank inside dim text is, so
/// `moves` is the end of that line: `Up/Down move`, or `↑↓ move` with Unicode).
fn open_settings(term: &mut Session) -> std::io::Result<()> {
    open_settings_ending(term, "Up/Down move")
}

fn open_settings_ending(term: &mut Session, moves: &str) -> std::io::Result<()> {
    term.key(CTRL_P)?;
    term.expect(ENTER, SHORT);
    term.expect_plain(moves, SHORT);
    term.write_bytes(b"Settings\r")?;
    // The palette gave the screen back, and the settings view took it.
    term.expect_plain("ui.theme", SHORT);
    term.expect_plain(moves, SHORT);
    Ok(())
}

/// Waits until the settings view has given the screen back. (The palette gave it back before
/// the settings view drew, and `open_settings` has waited past that.)
fn left_view(term: &mut Session) {
    term.expect(LEAVE, SHORT);
}

/// Pressing Enter on a switch writes the new value to the file, keeping the rest of the file.
#[test]
fn a_switch_is_written_to_the_file_and_the_comments_stay() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        let before = fs::read_to_string(config_file(ctx))?;
        open_settings(&mut term)?;
        term.write_bytes(b"editor.hints\r")?;
        term.key(ESC)?;
        left_view(&mut term);
        let after = fs::read_to_string(config_file(ctx))?;
        assert!(after.starts_with(&before), "the old text stays:\n{after}");
        assert!(
            after.ends_with("[editor]\nhints = false\n"),
            "the change was appended:\n{after}"
        );
        term.sync("alive")?;
        term.quit()
    });
}

/// A choice moves to the next word with Enter and to the previous one with Shift+Tab, each time
/// from the value the file has, and the comment on the line stays.
#[test]
fn a_choice_moves_on_and_back() {
    pty_test(BIN, |ctx| {
        write_config(ctx, "[ui]\nicons = \"unicode\" # keep this\n")?;
        let mut term = ctx.start()?;
        open_settings_ending(&mut term, "\u{2191}\u{2193} move")?;
        // The words are auto, unicode, ascii: unicode -> ascii -> unicode -> auto.
        term.write_bytes(b"ui.icons\r")?;
        term.write_bytes(b"\x1b[Z\x1b[Z")?;
        term.key(ESC)?;
        left_view(&mut term);
        assert_eq!(
            fs::read_to_string(config_file(ctx))?,
            "[ui]\nicons = \"auto\" # keep this\n"
        );
        term.quit()
    });
}

/// "Automatic" motion is no key in the file.
#[test]
fn automatic_motion_takes_the_key_out_of_the_file() {
    pty_test(BIN, |ctx| {
        write_config(ctx, "[ui]\nmotion = \"off\"\nicons = \"ascii\"\n")?;
        let mut term = ctx.start()?;
        open_settings(&mut term)?;
        term.write_bytes(b"ui.motion\r")?;
        term.key(ESC)?;
        left_view(&mut term);
        assert_eq!(
            fs::read_to_string(config_file(ctx))?,
            "[ui]\nicons = \"ascii\"\n"
        );
        term.quit()
    });
}

/// The running shell keeps its values; a new shell reads the file. Here: hints off in the file,
/// the first shell still hints (the vi mode of the second shows the file was read).
#[test]
fn a_change_applies_when_a_new_shell_starts() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        open_settings(&mut term)?;
        term.write_bytes(b"editor.mode\r")?;
        term.key(ESC)?;
        left_view(&mut term);
        // This shell is still emacs: Ctrl+A goes to the start of the line, and `echo` runs.
        term.write_bytes(b"cho emacs-$((1+1))\x01e\r")?;
        term.expect("emacs-2", SHORT);
        term.prompt();
        term.quit()?;
        drop(term);

        let mut second = ctx.start()?;
        // The vi prompt: in insert mode Escape leaves it and the prompt shows `: `.
        second.key(ESC)?;
        second.expect(": ", SHORT);
        // Back to insert mode, so that `exit` is typed and not read as vi commands.
        second.write_bytes(b"i")?;
        second.quit()
    });
}

/// A setting the view does not change says where to change it, and the file is not touched.
#[test]
fn a_setting_that_is_not_changed_here_leaves_the_file_alone() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        let before = fs::read_to_string(config_file(ctx))?;
        open_settings(&mut term)?;
        term.write_bytes(b"safety.confirm\r")?;
        // The note is a dim span, so it is written whole.
        term.expect_plain("Edit config.toml to change it.", SHORT);
        term.key(ESC)?;
        left_view(&mut term);
        assert_eq!(fs::read_to_string(config_file(ctx))?, before);
        term.quit()
    });
}

/// A file with a syntax error is not touched; the view says so.
#[test]
fn a_broken_file_is_not_written_and_the_view_says_so() {
    pty_test(BIN, |ctx| {
        write_config(ctx, "[editor\nhints = true\n")?;
        let mut term = ctx.start()?;
        open_settings(&mut term)?;
        term.write_bytes(b"editor.hints\r")?;
        term.expect_plain("Not saved: the file has a syntax error", SHORT);
        term.key(ESC)?;
        left_view(&mut term);
        assert_eq!(
            fs::read_to_string(config_file(ctx))?,
            "[editor\nhints = true\n"
        );
        term.quit()
    });
}

/// `--no-config`: the view shows and refuses; no file is made.
#[test]
fn without_a_config_file_the_view_is_read_only() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.launch().arg("--no-config").start()?;
        term.key(CTRL_P)?;
        term.expect_plain("Up/Down move", SHORT);
        term.write_bytes(b"Settings\r")?;
        term.expect_plain(
            "No configuration file in this shell (--no-config): view only.",
            SHORT,
        );
        term.write_bytes(b"editor.hints\r")?;
        term.expect_plain("Not saved: This shell has no configuration file", SHORT);
        term.key(ESC)?;
        left_view(&mut term);
        assert!(!config_file(ctx).exists());
        term.quit()
    });
}

/// A `SIGHUP` while the view is open ends the shell (status 129).
#[test]
fn sighup_while_the_settings_view_is_open_ends_the_shell() {
    if hup_is_ignored() {
        skip(
            "sighup_while_the_settings_view_is_open_ends_the_shell",
            "SIGHUP is ignored here (started under nohup?)",
        );
        return;
    }
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        open_settings(&mut term)?;
        term.signal_parent(Signal::SIGHUP)?;
        let status = term.wait_exit(SHORT);
        assert_eq!(status.code(), Some(129), "{}", term.transcript());
        assert!(term.transcript().contains(LEAVE));
        Ok(())
    });
}

/// Closing the view with a change that waits for the next start puts a one-line toast above the
/// prompt, and the next line that is submitted takes it away.
#[test]
fn a_change_that_waits_for_the_next_start_is_said_in_a_toast() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        open_settings(&mut term)?;
        term.write_bytes(b"editor.hints\r")?;
        term.key(ESC)?;
        left_view(&mut term);
        term.expect_plain("1 setting changes at the next start", SHORT);
        // The next line submitted ends the toast: the prompt drawn after it has none.
        term.sync("gone")?;
        let tail = term
            .since_cursor()
            .rsplit("gone-2")
            .next()
            .unwrap_or("")
            .to_owned();
        assert!(!tail.contains("next start"), "the toast stayed:\n{tail}");
        term.quit()
    });
}

/// A view closed without a change says nothing.
#[test]
fn no_toast_when_nothing_waits_for_the_next_start() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        open_settings(&mut term)?;
        term.key(ESC)?;
        left_view(&mut term);
        term.sync("quiet")?;
        assert!(
            !term.since_cursor().contains("next start"),
            "{}",
            term.since_cursor()
        );
        term.quit()
    });
}
