//! `omsh status` (UI upgrade, phase 5): a panel on a terminal, `label: value` lines on a pipe, and
//! no network either way.

use omsh_tests::session::strip_escapes;
use omsh_tests::{SHORT, pty_test};

const BIN: &str = env!("CARGO_BIN_EXE_omsh-pty-shell");

#[test]
fn on_a_terminal_it_is_a_panel_with_the_facts_of_this_shell() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.command("omsh status")?;
        term.expect_plain("Oh My Shell status", SHORT);
        term.expect_plain("State folder", SHORT);
        let (text, _) = strip_escapes(term.transcript());
        // The labels are lined up with blanks: look at the words with single blanks.
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        for words in [
            "Version",
            "Build",
            "AI not set up (omsh ai setup)",
            "Update check off",
            "Last look never",
            "Motion",
            "Terminal xterm-256color",
            "Streams stdin tty, stdout tty, stderr tty",
            "Sync output not asked yet",
            "Config file",
            "State folder",
        ] {
            assert!(text.contains(words), "{words}:\n{text}");
        }
        term.quit()
    });
}

#[test]
fn through_a_pipe_it_is_plain_label_colon_value_lines() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.command("omsh status | cat")?;
        term.expect("Update check: off", SHORT);
        let (shown, _) = strip_escapes(term.transcript());
        let after = shown.rsplit("omsh status | cat").next().unwrap_or_default();
        assert!(after.contains("Version: "), "{shown}");
        assert!(after.contains("AI: not set up (omsh ai setup)"), "{shown}");
        assert!(!after.contains("Oh My Shell status"), "{shown}");
        // What `cat` printed has no escape of its own: look at the raw transcript.
        let raw = term.transcript();
        let at = raw.rfind("Version: ").expect("a Version line");
        let line = raw[at..].lines().next().unwrap();
        assert!(!line.contains('\x1b'), "{line:?}");
        term.quit()
    });
}

#[test]
fn it_takes_no_arguments() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.command("omsh status now; echo status=$?")?;
        term.expect("takes no arguments", SHORT);
        term.expect("status=2", SHORT);
        term.quit()
    });
}

#[test]
fn it_says_why_a_check_that_is_on_does_not_run() {
    pty_test(BIN, |ctx| {
        let file = ctx.home().join(".config/oh-my-shell/config.toml");
        std::fs::create_dir_all(file.parent().unwrap())?;
        std::fs::write(&file, "[update]\ncheck = true\n")?;
        let mut term = ctx.launch().env("OMSH_NO_UPDATE_CHECK", "1").start()?;
        term.command("omsh status | cat")?;
        term.expect(
            "Update check: on, but not running here: OMSH_NO_UPDATE_CHECK is set",
            SHORT,
        );
        term.quit()
    });
}
