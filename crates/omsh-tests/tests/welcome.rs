//! The first-run welcome and `omsh tour` (S28), on a terminal that takes escape codes.

use std::fs;

use omsh_tests::{SHORT, pty_test};

const BIN: &str = env!("CARGO_BIN_EXE_omsh-pty-shell");

const WELCOME: &str = "Welcome to Oh My Shell.";

/// The start that writes `config.toml` says hello, with the file's place; the next start is quiet.
#[test]
fn the_welcome_is_shown_once_on_the_start_that_wrote_the_config_file() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        let first = term.transcript().to_owned();
        assert!(first.contains(WELCOME), "{first}");
        assert!(
            first.contains("Settings: ~/.config/oh-my-shell/config.toml"),
            "{first}"
        );
        assert!(first.contains("omsh tour"), "{first}");
        term.quit()?;
        assert!(ctx.home().join(".config/oh-my-shell/config.toml").is_file());

        let mut term = ctx.start()?;
        assert!(
            !term.transcript().contains(WELCOME),
            "shown a second time:\n{}",
            term.transcript()
        );
        term.quit()
    });
}

#[test]
fn the_welcome_comes_before_the_first_prompt() {
    pty_test(BIN, |ctx| {
        let term = ctx.start()?;
        let text = term.transcript();
        let welcome = text.find(WELCOME).expect("a welcome");
        let prompt = text.find("$ ").expect("a prompt");
        assert!(welcome < prompt, "{text}");
        Ok(())
    });
}

/// `--no-config` reads and writes nothing, so there is no first run.
#[test]
fn no_welcome_without_a_config_file() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.launch().arg("--no-config").start()?;
        assert!(
            !term.transcript().contains(WELCOME),
            "{}",
            term.transcript()
        );
        assert!(!ctx.home().join(".config/oh-my-shell/config.toml").exists());
        term.quit()
    });
}

/// A config file that is already there is not a first run, even if it is empty.
#[test]
fn no_welcome_when_the_config_file_exists() {
    pty_test(BIN, |ctx| {
        let file = ctx.home().join(".config/oh-my-shell/config.toml");
        fs::create_dir_all(file.parent().unwrap())?;
        fs::write(&file, "")?;
        let mut term = ctx.start()?;
        assert!(
            !term.transcript().contains(WELCOME),
            "{}",
            term.transcript()
        );
        term.quit()
    });
}

/// A non-interactive shell (`-c`) is handed to /bin/sh and never says hello.
#[test]
fn no_welcome_for_a_script() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.launch().arg("-c").arg("echo ran").start_raw()?;
        term.expect("ran", SHORT);
        assert!(
            !term.transcript().contains(WELCOME),
            "{}",
            term.transcript()
        );
        assert!(!ctx.home().join(".config/oh-my-shell/config.toml").exists());
        Ok(())
    });
}

#[test]
fn the_tour_shows_a_page_for_each_enter_and_ends_back_at_the_prompt() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.command("omsh tour")?;
        for page in 1..=6 {
            term.expect_plain(&format!("tour ({page} of 6)"), SHORT);
            term.expect_plain("[q] quit", SHORT);
            term.key(b'\r')?;
        }
        term.command("echo status-$?-end")?;
        term.expect("status-0-end", SHORT);
        term.quit()
    });
}

#[test]
fn b_goes_back_and_q_leaves_early() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.command("omsh tour")?;
        term.expect_plain("tour (1 of 6)", SHORT);
        term.key(b'\r')?;
        term.expect_plain("tour (2 of 6)", SHORT);
        term.key(b'b')?;
        term.expect_plain("tour (1 of 6)", SHORT);
        term.key(b'q')?;
        term.sync("left")?;
        assert!(
            !term.transcript().contains("tour (3 of 6)"),
            "{}",
            term.transcript()
        );
        term.quit()
    });
}

#[test]
fn ctrl_c_in_the_tour_gives_130_and_the_shell_goes_on() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.command("omsh tour")?;
        term.expect_plain("tour (1 of 6)", SHORT);
        term.key(3)?;
        term.command("echo status-$?-end")?;
        term.expect("status-130-end", SHORT);
        term.quit()
    });
}

/// With the output in a file, all six pages are written and no key is waited for.
#[test]
fn the_tour_written_to_a_file_has_every_page() {
    pty_test(BIN, |ctx| {
        let out = ctx.home().join("tour.txt");
        let mut term = ctx.start()?;
        term.command(&format!("omsh tour < /dev/null > {}", out.display()))?;
        term.sync("done")?;
        let text = fs::read_to_string(&out)?;
        for page in 1..=6 {
            assert!(text.contains(&format!("tour ({page} of 6)")), "{text}");
        }
        term.quit()
    });
}

#[test]
fn the_tour_works_with_no_ai() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.launch().arg("--no-ai").start()?;
        term.command("omsh tour")?;
        term.expect_plain("tour (1 of 6)", SHORT);
        term.key(b'q')?;
        term.sync("ok")?;
        term.quit()
    });
}

#[test]
fn omsh_without_a_command_lists_the_tour() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.command("omsh")?;
        term.expect_plain("tour", SHORT);
        term.quit()
    });
}
