//! The first start (UI upgrade, phase 5): the welcome, "Stay up to date?" and "Use eza for ls?",
//! on a terminal with keys; and the text of version 1.0 for everyone else.
//!
//! The harness asks for the text welcome by default (`OMSH_FIRST_RUN=plain`); a test about the
//! panels calls `first_run_panels()`. The shell under test gets a private `PATH` (a folder of its
//! own, with a fake `eza` only where the test wants one), so that a real `eza` on the machine that
//! runs the tests cannot change which questions are asked. The daily check is pointed at a closed
//! port on this computer, so no test can reach the real server.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use omsh_tests::fakerelease::FakeRelease;
use omsh_tests::session::strip_escapes;
use omsh_tests::watchdog::Ctx;
use omsh_tests::{Launch, SHORT, Session, pty_test};

const BIN: &str = env!("CARGO_BIN_EXE_omsh-pty-shell");

const ENTER: &[u8] = b"\r";
const ESC: &[u8] = b"\x1b";
const LEFT: &[u8] = b"\x1b[D";

/// Where the shell finds its first-run answers.
fn config_file(ctx: &Ctx) -> PathBuf {
    ctx.home().join(".config/oh-my-shell/config.toml")
}

/// A `bin` folder inside the test's home, holding a fake `eza` when asked.
fn bin(ctx: &Ctx, eza: bool) -> io::Result<PathBuf> {
    let dir = ctx.home().join("fakebin");
    fs::create_dir_all(&dir)?;
    if eza {
        let file = dir.join("eza");
        fs::write(&file, "#!/bin/sh\necho fake-eza \"$@\"\n")?;
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&file, fs::Permissions::from_mode(0o755))?;
    }
    Ok(dir)
}

/// The launch of a test of the panels: private `PATH`, no network.
fn launch(ctx: &Ctx, eza: bool) -> io::Result<Launch> {
    let path = bin(ctx, eza)?;
    Ok(ctx
        .launch()
        .first_run_panels()
        .env("PATH", &path.to_string_lossy())
        // Closed on this computer: a lookup, if one starts, is refused at once.
        .env("OMSH_RELEASE_SERVER", "http://127.0.0.1:9"))
}

/// Waits for the welcome.
fn welcome(term: &mut Session) {
    term.expect_plain("Welcome to Oh My Shell", SHORT);
    term.expect_plain("Take the tour", SHORT);
}

/// What `command` printed: the text between its echoed line and a marker that follows it. (The
/// text of a session since the cursor is empty after a wait, so the transcript is searched.)
fn output_of(term: &mut Session, command: &str) -> io::Result<String> {
    term.command(&format!("{command}; echo out-end-$((1+1))"))?;
    term.expect("out-end-2", SHORT);
    term.prompt();
    let (text, _) = strip_escapes(term.transcript());
    let echoed = "echo out-end-$((1+1))";
    let start = text.rfind(echoed).map_or(0, |at| at + echoed.len());
    let end = text.rfind("out-end-2").unwrap_or(text.len()).max(start);
    Ok(text[start..end].to_owned())
}

/// The lines of the file that say something (not blank, not a comment).
fn live_lines(file: &Path) -> io::Result<Vec<String>> {
    Ok(fs::read_to_string(file)?
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect())
}

// -- the defaults change nothing ----------------------------------------------------------------

/// Enter at every question is the safe answer to each: the file keeps its comments and says
/// nothing live, except the answer to the update question, which is written either way.
#[test]
fn enter_at_every_question_changes_no_setting_but_records_that_the_check_is_off() {
    pty_test(BIN, |ctx| {
        let mut term = launch(ctx, true)?.start_raw()?;
        welcome(&mut term);
        term.press(ENTER)?;
        term.expect_plain("Stay up to date?", SHORT);
        term.press(ENTER)?;
        term.expect_plain("Use eza for ls?", SHORT);
        term.press(ENTER)?;
        term.prompt();
        assert_eq!(
            live_lines(&config_file(ctx))?,
            ["[update]", "check = false"],
            "{}",
            term.transcript()
        );
        // `ls` was not turned into eza.
        let aliases = output_of(&mut term, "alias")?;
        assert!(!aliases.contains("ls="), "{aliases}");
        term.quit()
    });
}

/// Esc is the same answer as the default at every panel.
#[test]
fn esc_at_every_question_is_the_safe_answer() {
    pty_test(BIN, |ctx| {
        let mut term = launch(ctx, true)?.start_raw()?;
        welcome(&mut term);
        term.press(ESC)?;
        term.expect_plain("Stay up to date?", SHORT);
        term.press(ESC)?;
        term.expect_plain("Use eza for ls?", SHORT);
        term.press(ESC)?;
        term.prompt();
        assert_eq!(
            live_lines(&config_file(ctx))?,
            ["[update]", "check = false"],
            "{}",
            term.transcript()
        );
        term.quit()
    });
}

/// Without `eza` on `PATH` there is no third question.
#[test]
fn no_eza_no_question_about_ls() {
    pty_test(BIN, |ctx| {
        let mut term = launch(ctx, false)?.start_raw()?;
        welcome(&mut term);
        term.press(ENTER)?;
        term.expect_plain("Stay up to date?", SHORT);
        term.press(ENTER)?;
        term.prompt();
        assert!(
            !term.transcript().contains("Use eza"),
            "{}",
            term.transcript()
        );
        term.quit()
    });
}

/// The questions are asked once: the next start has the file and is quiet.
#[test]
fn the_second_start_asks_nothing() {
    pty_test(BIN, |ctx| {
        let mut term = launch(ctx, true)?.start_raw()?;
        welcome(&mut term);
        term.press(ENTER)?;
        term.expect_plain("Stay up to date?", SHORT);
        term.press(ENTER)?;
        term.expect_plain("Use eza for ls?", SHORT);
        term.press(ENTER)?;
        term.prompt();
        term.quit()?;

        let mut term = launch(ctx, true)?.start()?;
        let text = term.transcript().to_owned();
        for words in ["Welcome to Oh My Shell", "Stay up to date?", "Use eza"] {
            assert!(!text.contains(words), "{words}:\n{text}");
        }
        // The answer was "not now": `eza` is on `PATH`, but `ls` is not an alias of it.
        let aliases = output_of(&mut term, "alias")?;
        assert!(!aliases.contains("ls="), "{aliases}");
        term.quit()
    });
}

// -- the answers ----------------------------------------------------------------------------------

#[test]
fn yes_to_the_daily_check_writes_it_and_no_writes_the_opposite() {
    for (keys, expected) in [(&b"y"[..], "check = true"), (&b"n"[..], "check = false")] {
        pty_test(BIN, |ctx| {
            let mut term = launch(ctx, false)?.start_raw()?;
            welcome(&mut term);
            term.press(ENTER)?;
            term.expect_plain("Stay up to date?", SHORT);
            term.press(keys)?;
            term.prompt();
            term.quit()?;
            assert_eq!(live_lines(&config_file(ctx))?, ["[update]", expected]);
            Ok(())
        });
    }
}

/// Yes starts the daily check in this very session (a lookup arrives a moment after the prompt); No
/// does not. The server is a fake one on this computer.
#[test]
fn the_answer_to_the_daily_check_counts_in_the_same_session() {
    for (key, lookups) in [(&b"y"[..], 1), (&b"n"[..], 0)] {
        pty_test(BIN, move |ctx| {
            let server = FakeRelease::start();
            let mut term = launch(ctx, false)?
                .env("OMSH_RELEASE_SERVER", &server.origin())
                .start_raw()?;
            welcome(&mut term);
            term.press(ENTER)?;
            term.expect_plain("Stay up to date?", SHORT);
            term.press(key)?;
            term.prompt();
            // The check waits 2 s after the prompt.
            term.pump(Duration::from_millis(3500));
            assert_eq!(
                server.requests().len(),
                lookups,
                "{:?}\n{}",
                server.requests(),
                term.transcript()
            );
            term.quit()
        });
    }
}

/// The arrow keys move: Left from No lands on Yes, and Enter takes it.
#[test]
fn the_arrow_keys_move_between_the_buttons() {
    pty_test(BIN, |ctx| {
        let mut term = launch(ctx, false)?.start_raw()?;
        welcome(&mut term);
        term.press(ENTER)?;
        term.expect_plain("Stay up to date?", SHORT);
        term.press(LEFT)?;
        term.press(ENTER)?;
        term.prompt();
        term.quit()?;
        assert_eq!(live_lines(&config_file(ctx))?, ["[update]", "check = true"]);
        Ok(())
    });
}

#[test]
fn choosing_eza_writes_ui_ls_and_ls_runs_it_at_once_and_at_the_next_start() {
    pty_test(BIN, |ctx| {
        let mut term = launch(ctx, true)?.start_raw()?;
        welcome(&mut term);
        term.press(ENTER)?;
        term.expect_plain("Stay up to date?", SHORT);
        term.press(ENTER)?;
        term.expect_plain("Use eza for ls?", SHORT);
        term.press(b"u")?;
        term.prompt();
        term.command("ls -l here")?;
        term.expect("fake-eza -l here", SHORT);
        term.quit()?;
        let lines = live_lines(&config_file(ctx))?;
        assert!(lines.contains(&"ls = \"eza\"".to_owned()), "{lines:?}");
        assert!(lines.contains(&"check = false".to_owned()), "{lines:?}");
        let text = fs::read_to_string(config_file(ctx))?;
        assert!(text.contains("ls = \"eza\""), "{text}");

        // The next start does it from the file.
        let mut term = launch(ctx, true)?.start()?;
        term.command("ls again")?;
        term.expect("fake-eza again", SHORT);
        // And a person can take it away for the session.
        let before = output_of(&mut term, "alias")?;
        assert!(
            before.contains("ls='eza'") || before.contains("ls=eza"),
            "{before}"
        );
        output_of(&mut term, "unalias ls")?;
        let after = output_of(&mut term, "alias")?;
        assert!(!after.contains("ls="), "{after}");
        term.quit()
    });
}

/// The file's `ui.ls = "eza"` does nothing when eza is gone: `ls` stays what it was.
#[test]
fn ui_ls_eza_without_eza_on_the_path_defines_no_alias() {
    pty_test(BIN, |ctx| {
        let file = config_file(ctx);
        fs::create_dir_all(file.parent().unwrap())?;
        fs::write(&file, "[ui]\nls = \"eza\"\n")?;
        let mut term = launch(ctx, false)?.start()?;
        let aliases = output_of(&mut term, "alias")?;
        assert!(!aliases.contains("ls="), "{aliases}");
        term.quit()
    });
}

/// `init.omsh` runs after the alias, so it can take it away or replace it.
#[test]
fn init_omsh_can_replace_the_eza_alias() {
    pty_test(BIN, |ctx| {
        let file = config_file(ctx);
        fs::create_dir_all(file.parent().unwrap())?;
        fs::write(&file, "[ui]\nls = \"eza\"\n")?;
        fs::write(file.with_file_name("init.omsh"), "alias ls='echo mine'\n")?;
        let mut term = launch(ctx, true)?.start()?;
        term.command("ls x")?;
        term.expect("mine x", SHORT);
        term.quit()
    });
}

// -- the other ways to start ----------------------------------------------------------------------

/// `TERM=dumb`: the three lines of text, nothing is asked and nothing is written.
#[test]
fn a_dumb_terminal_gets_the_text_and_no_questions() {
    pty_test(BIN, |ctx| {
        let mut term = launch(ctx, true)?.dumb().start()?;
        let text = term.transcript().to_owned();
        assert!(text.contains("Welcome to Oh My Shell."), "{text}");
        assert!(!text.contains("Stay up to date?"), "{text}");
        term.quit()?;
        assert!(live_lines(&config_file(ctx))?.is_empty());
        Ok(())
    });
}

/// `OMSH_FIRST_RUN=plain` asks for the text on a terminal that could show panels.
#[test]
fn the_variable_asks_for_the_text_welcome() {
    pty_test(BIN, |ctx| {
        let mut term = launch(ctx, true)?.env("OMSH_FIRST_RUN", "plain").start()?;
        let text = term.transcript().to_owned();
        assert!(text.contains("Welcome to Oh My Shell."), "{text}");
        assert!(!text.contains("Take the tour"), "{text}");
        term.quit()?;
        assert!(live_lines(&config_file(ctx))?.is_empty());
        Ok(())
    });
}

/// CI is not a person: the text, no questions.
#[test]
fn ci_gets_the_text_and_no_questions() {
    pty_test(BIN, |ctx| {
        let mut term = launch(ctx, true)?.env("CI", "true").start()?;
        let text = term.transcript().to_owned();
        assert!(text.contains("Welcome to Oh My Shell."), "{text}");
        assert!(!text.contains("Stay up to date?"), "{text}");
        term.quit()
    });
}

/// With the AI switched off, the welcome does not offer to set it up.
#[test]
fn no_ai_leaves_out_the_ai_button() {
    pty_test(BIN, |ctx| {
        let mut term = launch(ctx, false)?.arg("--no-ai").start_raw()?;
        welcome(&mut term);
        // The shortcut letter of a button is underlined, so look at the text without escapes.
        let (plain, _) = strip_escapes(term.transcript());
        assert!(!plain.contains("Set up AI now"), "{plain}");
        assert!(plain.contains("Take the tour"), "{plain}");
        term.press(ESC)?;
        term.expect_plain("Stay up to date?", SHORT);
        term.press(ESC)?;
        term.prompt();
        term.quit()
    });
}

/// With the AI on, the button is there, and `a` begins the setup.
#[test]
fn a_starts_the_ai_setup_and_ctrl_c_leaves_it() {
    pty_test(BIN, |ctx| {
        let mut term = launch(ctx, false)?.start_raw()?;
        welcome(&mut term);
        term.expect_plain("Set up AI now", SHORT);
        term.press(b"a")?;
        term.expect_plain("Choose a provider", SHORT);
        term.press(&[3])?;
        // The questions after it are still asked.
        term.expect_plain("Stay up to date?", SHORT);
        term.press(ENTER)?;
        term.prompt();
        term.quit()
    });
}

/// The tour is the same one `omsh tour` shows, and the questions go on after it.
#[test]
fn t_shows_the_tour_and_the_questions_go_on() {
    pty_test(BIN, |ctx| {
        let mut term = launch(ctx, false)?.start_raw()?;
        welcome(&mut term);
        term.press(b"t")?;
        term.expect_plain("tour (1 of 6)", SHORT);
        term.press(b"q")?;
        term.expect_plain("Stay up to date?", SHORT);
        term.press(ENTER)?;
        term.prompt();
        term.quit()
    });
}

/// Ctrl+C at the welcome gives up the rest of the questions; the shell goes on with its prompt.
#[test]
fn ctrl_c_at_the_welcome_ends_the_questions_and_the_shell_goes_on() {
    pty_test(BIN, |ctx| {
        let mut term = launch(ctx, true)?.start_raw()?;
        welcome(&mut term);
        term.press(&[3])?;
        term.prompt();
        assert!(
            !term.transcript().contains("Stay up to date?"),
            "{}",
            term.transcript()
        );
        assert!(live_lines(&config_file(ctx))?.is_empty());
        term.quit()
    });
}

/// The settings file is shown under the box, as `~/...`.
#[test]
fn the_welcome_names_the_settings_file() {
    pty_test(BIN, |ctx| {
        let mut term = launch(ctx, false)?.start_raw()?;
        welcome(&mut term);
        term.expect_plain("Settings: ~/.config/oh-my-shell/config.toml", SHORT);
        term.press(ESC)?;
        term.expect_plain("Stay up to date?", SHORT);
        term.press(ESC)?;
        term.prompt();
        term.quit()
    });
}
