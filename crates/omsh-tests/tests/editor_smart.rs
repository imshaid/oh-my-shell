//! Hints, abbreviations, bracketed paste and `j` (S14b), on a terminal that takes escape codes.
//!
//! The terminal does not draw: tests look for text in what the shell printed. A hint is drawn
//! after the typed text, so `expect_plain` (which ignores escape codes) finds the typed part and
//! the hint together.

use std::fs;
use std::io;
use std::time::Duration;

use omsh_tests::{Ctx, SHORT, pty_test};

const BIN: &str = env!("CARGO_BIN_EXE_omsh-pty-shell");

/// Right arrow, as a terminal sends it.
const RIGHT: &[u8] = b"\x1b[C";
/// Up arrow.
const UP: &[u8] = b"\x1b[A";
/// Ctrl+U: drop what was typed on the line.
const CTRL_U: &[u8] = b"\x15";

fn write_config(ctx: &Ctx, text: &str) -> io::Result<()> {
    let dir = ctx.home().join(".config/oh-my-shell");
    fs::create_dir_all(&dir)?;
    fs::write(dir.join("config.toml"), text)
}

/// A hint shows the rest of an earlier command, and Right takes it into the line.
#[test]
fn a_hint_from_history_is_shown_and_right_accepts_it() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.command("echo hint-$((1+1))")?;
        term.expect("hint-2", SHORT);
        term.prompt();
        term.write_bytes(b"echo hi")?;
        term.expect_plain("echo hint-$((1+1))", SHORT);
        // Not run yet: the hint is only a suggestion.
        assert!(!term.try_expect("hint-2", Duration::from_millis(300)));
        term.write_bytes(RIGHT)?;
        term.write_bytes(b"\r")?;
        term.expect("hint-2", SHORT);
        term.quit()
    });
}

/// A command run in this directory comes before a newer one run in another directory.
#[test]
fn a_command_from_this_directory_is_hinted_first() {
    pty_test(BIN, |ctx| {
        let a = ctx.home().join("a");
        let b = ctx.home().join("b");
        fs::create_dir_all(&a)?;
        fs::create_dir_all(&b)?;
        let mut term = ctx.start()?;
        term.command(&format!("cd {}", a.display()))?;
        term.command("echo pick-a-$((1+1))")?;
        term.expect("pick-a-2", SHORT);
        term.command(&format!("cd {}", b.display()))?;
        term.command("echo pick-b-$((1+1))")?;
        term.expect("pick-b-2", SHORT);
        // In `b` the newest is `pick-b`; back in `a` it is `pick-a`, though `pick-b` is newer.
        term.command(&format!("cd {}", a.display()))?;
        term.prompt();
        term.write_bytes(b"echo pick-")?;
        term.expect_plain("echo pick-a-$((1+1))", SHORT);
        term.write_bytes(CTRL_U)?;
        term.quit()
    });
}

/// With `hints = false` nothing is suggested.
#[test]
fn hints_can_be_turned_off() {
    pty_test(BIN, |ctx| {
        write_config(ctx, "[editor]\nhints = false\n")?;
        let mut term = ctx.start()?;
        term.command("echo zq-$((1+1))")?;
        term.expect("zq-2", SHORT);
        term.prompt();
        term.write_bytes(b"echo z")?;
        assert!(!term.try_expect("q-$((1+1))", Duration::from_millis(500)));
        term.write_bytes(CTRL_U)?;
        term.quit()
    });
}

/// An abbreviation is expanded in command position, on Space and on Enter; the history keeps
/// the long form; an argument that looks like one is left alone.
#[test]
fn an_abbreviation_expands_in_command_position_only() {
    pty_test(BIN, |ctx| {
        write_config(
            ctx,
            "[editor.abbreviations]\ngx = \"echo expanded-$((1+1))\"\n",
        )?;
        let mut term = ctx.start()?;
        term.command("echo gx end-$((1+1))")?;
        term.expect("gx end-2", SHORT);
        assert!(!term.transcript().contains("expanded-2"));
        // `Session::command` waits for the typed text to be shown, but the editor replaces it by
        // the long form, so the line is written by hand.
        term.prompt();
        term.write_bytes(b"gx\r")?;
        term.expect("expanded-2", SHORT);
        // Space expands too: the long form is in the line before Enter.
        term.prompt();
        // Keys that arrive together are read as one insertion (like a paste), so the space
        // comes alone, as it does from a keyboard.
        term.write_bytes(b"gx")?;
        std::thread::sleep(Duration::from_millis(200));
        term.write_bytes(b" ")?;
        term.expect_plain("echo expanded-$((1+1)) ", SHORT);
        term.write_bytes(CTRL_U)?;
        term.command("history search expanded-")?;
        term.expect("expanded-$((1+1))", SHORT);
        term.quit()
    });
}

/// A multi-line paste waits in the line for one Enter, and is one history entry.
#[test]
fn a_bracketed_paste_is_not_run_until_enter() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        // The editor asks the terminal for bracketed paste while it reads a line.
        assert!(
            term.transcript().contains("\x1b[?2004h"),
            "{}",
            term.transcript()
        );
        term.prompt();
        term.write_bytes(b"\x1b[200~echo pa-$((1+1))\necho pb-$((1+1))\n\x1b[201~")?;
        assert!(
            !term.try_expect("pa-2", Duration::from_millis(500)),
            "{}",
            term.transcript()
        );
        term.write_bytes(b"\r")?;
        term.expect("pa-2", SHORT);
        term.expect("pb-2", SHORT);
        // One entry: Up brings both lines back.
        term.prompt();
        term.write_bytes(UP)?;
        term.write_bytes(b"\r")?;
        term.expect("pa-2", SHORT);
        term.expect("pb-2", SHORT);
        term.quit()
    });
}

/// An escape sequence inside a paste does not reach the line.
#[test]
fn a_paste_has_its_control_characters_removed() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.prompt();
        term.write_bytes(b"\x1b[200~echo \x1b[31mcl-$((1+1))\x1b[201~\r")?;
        // Without the escape character the text is `[31mcl-2`.
        term.expect("[31mcl-2", SHORT);
        term.quit()
    });
}

/// `j words` goes to the best visited directory, `j -` goes back, and a miss or a missing word
/// has a status.
#[test]
fn j_jumps_to_a_visited_directory() {
    pty_test(BIN, |ctx| {
        let alpha = ctx.home().join("alpha-proj");
        let beta = ctx.home().join("beta");
        fs::create_dir_all(&alpha)?;
        fs::create_dir_all(&beta)?;
        let mut term = ctx.start()?;
        term.command(&format!("cd {}", alpha.display()))?;
        term.command(&format!("cd {}", beta.display()))?;
        term.command("cd /")?;
        term.command("j alph")?;
        term.command("echo at-$PWD")?;
        term.expect(&format!("at-{}", alpha.display()), SHORT);
        term.command("j -")?;
        term.command("echo back-$PWD")?;
        term.expect("back-/", SHORT);
        term.command("j nomatchzz; echo miss-$?")?;
        term.expect("miss-1", SHORT);
        term.command("j; echo usage-$?")?;
        term.expect("usage-2", SHORT);
        term.quit()
    });
}

/// Visited directories are remembered by the next shell.
#[test]
fn j_remembers_directories_between_shells() {
    pty_test(BIN, |ctx| {
        let place = ctx.home().join("remembered-place");
        fs::create_dir_all(&place)?;
        let mut term = ctx.start()?;
        term.command(&format!("cd {}", place.display()))?;
        term.sync("one")?;
        term.quit()?;
        let mut term = ctx.start()?;
        term.command("cd /")?;
        term.command("j remembered")?;
        term.command("echo now-$PWD")?;
        term.expect(&format!("now-{}", place.display()), SHORT);
        term.quit()
    });
}
