//! Colors, themes and the wait line, in a real terminal (S26).
//!
//! The test terminal is a plain `xterm-256color` with `NO_COLOR=1` and the C locale unless a test
//! says otherwise. A test that wants colors sets `NO_COLOR` to an empty string (which does not
//! count) and a UTF-8 locale. Panels are looked at as raw bytes (`expect`) when the escape codes
//! are the point, and as the text a person reads (`expect_plain`) when they are not.

use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use omsh_tests::fakehttp::Server;
use omsh_tests::question::{Answer, MODES, Mode, answer, plan_key, wait_plan, wait_question};
use omsh_tests::{Ctx, Launch, SHORT, Session, pty_test};

const BIN: &str = env!("CARGO_BIN_EXE_omsh-pty-shell");

const BRAILLE: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];

fn config_dir(ctx: &Ctx) -> io::Result<PathBuf> {
    let dir = ctx.home().join(".config/oh-my-shell");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// A shell in the home directory (which holds `proj/build/f`) with colors on and a UTF-8
/// locale, reading `config.toml` with `config` in it.
fn colored(ctx: &Ctx, config: &str) -> io::Result<Session> {
    colored_in(ctx, config, Mode::Panel)
}

/// Like [`colored`]; for `Mode::Dumb` the shell reads with the plain reader (`INSIDE_EMACS`, so
/// that the terminal still takes colors) and the question is the text one, drawn in color.
fn colored_in(ctx: &Ctx, config: &str, mode: Mode) -> io::Result<Session> {
    fs::create_dir_all(ctx.home().join("proj/build"))?;
    fs::write(ctx.home().join("proj/build/f"), "x\n")?;
    fs::write(config_dir(ctx)?.join("config.toml"), config)?;
    let launch = ctx
        .launch()
        .env("NO_COLOR", "")
        .env("LANG", "C.UTF-8")
        .cwd(ctx.home());
    match mode {
        Mode::Panel => launch.start(),
        Mode::Dumb => launch.env("INSIDE_EMACS", "1").start(),
    }
}

fn theme_file(ctx: &Ctx, name: &str, text: &str) -> io::Result<()> {
    let dir = config_dir(ctx)?.join("themes");
    fs::create_dir_all(&dir)?;
    fs::write(dir.join(format!("{name}.toml")), text)
}

// -- the safety question ------------------------------------------------------------------------

/// The text of the question from its first row to the end of what is on the screen: what the
/// panel (or the text panel) wrote, without the prompts before it.
fn question_text(term: &Session) -> &str {
    let text = term.transcript();
    let at = text
        .rfind("this command is")
        .unwrap_or_else(|| panic!("the question was not shown:\n{text}"));
    &text[at..]
}

#[test]
fn the_text_panel_has_a_red_bar_and_a_red_level_word() {
    // The plain reader keeps the text question, in color: the bar, the level word, the key.
    pty_test(BIN, |ctx| {
        let mut term = colored_in(ctx, "", Mode::Dumb)?;
        term.command("rm -rf proj/build")?;
        term.expect("this command is \x1b[31mdangerous\x1b[0m:", SHORT);
        // The bar replaces the two blanks in front of each line.
        term.expect("\x1b[31m│\x1b[0m rm -rf proj/build", SHORT);
        term.expect("\x1b[31m│\x1b[0m - Deletes files without asking", SHORT);
        // The key in the question is in the accent color.
        term.expect("Run it? \x1b[34m[y/N]\x1b[0m", SHORT);
        term.line("n")?;
        term.expect_plain("cancelled; nothing ran", SHORT);
        Ok(())
    });
}

#[test]
fn the_panel_has_a_red_rail_a_red_level_word_and_cyan_keys() {
    pty_test(BIN, |ctx| {
        let mut term = colored(ctx, "")?;
        term.command("rm -rf proj/build")?;
        term.expect("this command is \x1b[31mdangerous\x1b[0m:", SHORT);
        term.expect("\x1b[31m│\x1b[0m rm -rf proj/build", SHORT);
        term.expect("\x1b[31m│\x1b[0m - Deletes files without asking", SHORT);
        wait_question(&mut term, Mode::Panel);
        // The chosen button is reversed, and the keys in the bottom rule are in the key color.
        assert!(
            term.transcript().contains("\x1b[7m"),
            "{}",
            term.transcript()
        );
        assert!(
            question_text(&term).contains("\x1b[36mEnter\x1b[0m"),
            "{}",
            question_text(&term)
        );
        answer(&mut term, Mode::Panel, Answer::Cancel)?;
        term.expect_plain("cancelled; nothing ran", SHORT);
        Ok(())
    });
}

#[test]
fn no_color_keeps_the_bar_and_drops_the_color() {
    for mode in MODES {
        pty_test(BIN, move |ctx| {
            fs::create_dir_all(ctx.home().join("proj/build"))?;
            let launch = ctx
                .launch()
                .env("LANG", "C.UTF-8") // NO_COLOR=1 stays
                .cwd(ctx.home());
            let mut term = match mode {
                Mode::Panel => launch.start()?,
                Mode::Dumb => launch.env("INSIDE_EMACS", "1").start()?,
            };
            term.command("rm -rf proj/build")?;
            term.expect_plain("this command is dangerous:", SHORT);
            term.expect_plain("│ rm -rf proj/build", SHORT);
            term.expect_plain("│ - Deletes files without asking", SHORT);
            wait_question(&mut term, mode);
            // What the question wrote has no color; the editor's own paint is not looked at.
            let shown = question_text(&term).to_owned();
            answer(&mut term, mode, Answer::Cancel)?;
            term.expect_plain("cancelled; nothing ran", SHORT);
            for color in [
                "\x1b[31m",
                "\x1b[34m",
                "\x1b[36m",
                "\x1b[32m",
                "\x1b[90m",
                "\x1b[2;90m",
            ] {
                assert!(!shown.contains(color), "NO_COLOR: {color:?} in:\n{shown}");
            }
            if mode == Mode::Panel {
                assert!(
                    shown.contains("\x1b[7m"),
                    "the chosen button stays reversed:\n{shown}"
                );
            }
            Ok(())
        });
    }
}

#[test]
fn in_the_c_locale_the_bar_is_ascii() {
    for mode in MODES {
        pty_test(BIN, move |ctx| {
            fs::create_dir_all(ctx.home().join("proj/build"))?;
            let mut term = mode.launch(ctx.launch()).cwd(ctx.home()).start()?;
            term.command("rm -rf proj/build")?;
            term.expect_plain("| rm -rf proj/build", SHORT);
            term.expect_plain("| - Deletes files without asking", SHORT);
            wait_question(&mut term, mode);
            answer(&mut term, mode, Answer::Cancel)?;
            term.expect_plain("cancelled; nothing ran", SHORT);
            assert!(!term.transcript().contains('│'), "{}", term.transcript());
            Ok(())
        });
    }
}

#[test]
fn a_delete_that_may_go_to_the_trash_does_not_say_it_cannot_be_undone() {
    for mode in MODES {
        pty_test(BIN, move |ctx| {
            let mut term = colored_in(ctx, "", mode)?;
            term.command("rm -rf proj/build")?;
            match mode {
                Mode::Panel => term.expect_flat(
                    "This cannot be undone, unless you choose Move to trash.",
                    SHORT,
                ),
                Mode::Dumb => {
                    term.expect_flat("This cannot be undone, unless you answer t", SHORT);
                    term.expect_flat("(t = move to the trash instead)", SHORT);
                }
            }
            wait_question(&mut term, mode);
            answer(&mut term, mode, Answer::Cancel)?;
            term.expect_plain("cancelled; nothing ran", SHORT);
            Ok(())
        });
    }
}

#[test]
fn a_delete_for_good_says_so() {
    for mode in MODES {
        pty_test(BIN, move |ctx| {
            let mut term = colored_in(ctx, "", mode)?;
            // `find -delete` never goes to the trash.
            term.command("find proj/build -delete")?;
            term.expect_flat(
                "without asking about each one. This cannot be undone.",
                SHORT,
            );
            wait_question(&mut term, mode);
            answer(&mut term, mode, Answer::Cancel)?;
            term.expect_plain("cancelled; nothing ran", SHORT);
            Ok(())
        });
    }
}

// -- theme files ---------------------------------------------------------------------------------

#[test]
fn a_theme_file_changes_the_colors_of_the_text_question() {
    pty_test(BIN, |ctx| {
        theme_file(
            ctx,
            "mine",
            "[tokens]\ndanger = \"magenta\"\naccent = \"green\"\n",
        )?;
        let mut term = colored_in(ctx, "[ui]\ntheme = \"mine\"\n", Mode::Dumb)?;
        term.command("rm -rf proj/build")?;
        term.expect("this command is \x1b[35mdangerous\x1b[0m:", SHORT);
        term.expect("\x1b[35m│\x1b[0m rm -rf proj/build", SHORT);
        term.expect("Run it? \x1b[32m[y/N]\x1b[0m", SHORT);
        term.line("n")?;
        term.expect_plain("cancelled; nothing ran", SHORT);
        Ok(())
    });
}

#[test]
fn a_theme_file_changes_the_colors_of_the_panel_and_the_failure_symbol() {
    pty_test(BIN, |ctx| {
        theme_file(
            ctx,
            "mine",
            "[tokens]\ndanger = \"magenta\"\nkey = \"green\"\n",
        )?;
        let mut term = colored(ctx, "[ui]\ntheme = \"mine\"\n")?;
        term.command("rm -rf proj/build")?;
        term.expect("this command is \x1b[35mdangerous\x1b[0m:", SHORT);
        term.expect("\x1b[35m│\x1b[0m rm -rf proj/build", SHORT);
        wait_question(&mut term, Mode::Panel);
        // The keys of the bottom rule are in the `key` token.
        assert!(
            question_text(&term).contains("\x1b[32mEnter\x1b[0m"),
            "{}",
            question_text(&term)
        );
        answer(&mut term, Mode::Panel, Answer::Cancel)?;
        term.expect_plain("cancelled; nothing ran", SHORT);
        // The failure symbol of the prompt is the same token.
        term.command("false")?;
        term.expect("\x1b[35m❯ \x1b[0m", SHORT);
        Ok(())
    });
}

#[test]
fn a_broken_theme_file_is_reported_and_the_shell_starts_with_nord() {
    pty_test(BIN, |ctx| {
        theme_file(
            ctx,
            "bad",
            "[tokens]\naccent = \"#5E81AC\"\ndanger = \"magenta\"\n",
        )?;
        fs::write(
            config_dir(ctx)?.join("config.toml"),
            "[ui]\ntheme = \"bad\"\n",
        )?;
        let mut term = ctx
            .launch()
            .env("NO_COLOR", "")
            .env("LANG", "C.UTF-8")
            .cwd(ctx.home())
            .start_raw()?;
        // Before the first prompt: the file, the line and the column, and why.
        term.expect_plain("themes/bad.toml:2:10: ", SHORT);
        term.expect_plain("\"#5E81AC\" is not a color name", SHORT);
        term.expect("❯ ", SHORT);
        // The good key next to the bad one still applies; the shell works.
        fs::create_dir_all(ctx.home().join("proj/build"))?;
        term.command("rm -rf proj/build")?;
        term.expect("this command is \x1b[35mdangerous\x1b[0m:", SHORT);
        wait_question(&mut term, Mode::Panel);
        answer(&mut term, Mode::Panel, Answer::Cancel)?;
        term.expect_plain("cancelled; nothing ran", SHORT);
        term.command("echo still-here")?;
        term.expect_plain("still-here", SHORT);
        Ok(())
    });
}

#[test]
fn a_theme_that_does_not_exist_is_one_line_and_nord() {
    pty_test(BIN, |ctx| {
        fs::create_dir_all(config_dir(ctx)?)?;
        fs::write(
            config_dir(ctx)?.join("config.toml"),
            "[ui]\ntheme = \"ghost\"\n",
        )?;
        let mut term = ctx
            .launch()
            .env("NO_COLOR", "")
            .env("LANG", "C.UTF-8")
            .cwd(ctx.home())
            .start_raw()?;
        term.expect_plain("no theme called \"ghost\"", SHORT);
        term.expect("❯ ", SHORT);
        term.command("echo fine")?;
        term.expect_plain("fine", SHORT);
        Ok(())
    });
}

// -- the AI wait line and the plan panel ---------------------------------------------------------

fn configure(ctx: &Ctx, server: &Server, ui: &str) -> io::Result<()> {
    let dir = config_dir(ctx)?;
    fs::write(
        dir.join("config.toml"),
        format!(
            "{ui}[ai]\nprovider = \"openai-compatible\"\nmodel = \"model-one\"\nbase_url = \"{}\"\n",
            server.url("/v1")
        ),
    )
}

fn ai_launch(ctx: &Ctx) -> Launch {
    ctx.launch()
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            "unix:path=/nonexistent/omsh-test-bus",
        )
        .env("LANG", "C.UTF-8")
        .cwd(ctx.home())
}

/// A server that holds its answer for a long time, so that the wait line stays up.
fn slow_server() -> io::Result<Server> {
    Server::start_answering(Duration::from_secs(30), false, &["{}"])
}

fn braille_seen(term: &Session) -> usize {
    BRAILLE
        .iter()
        .filter(|glyph| term.transcript().contains(**glyph))
        .count()
}

#[test]
fn with_full_motion_the_glyph_of_the_wait_line_turns() {
    pty_test(BIN, |ctx| {
        let server = slow_server()?;
        configure(ctx, &server, "")?;
        let mut term = ai_launch(ctx).start()?;
        term.command("? something slow")?;
        // In order: each frame is 100 ms after the one before.
        term.expect("⠙ Asking OpenAI-compatible server (model-one)... ", SHORT);
        term.expect("⠸ Asking OpenAI-compatible server (model-one)... ", SHORT);
        term.key(3)?;
        term.expect_plain("cancelled; nothing ran", SHORT);
        Ok(())
    });
}

#[test]
fn over_ssh_the_glyph_does_not_turn_and_the_time_is_drawn_once_a_second() {
    pty_test(BIN, |ctx| {
        let server = slow_server()?;
        configure(ctx, &server, "")?;
        let mut term = ai_launch(ctx)
            .env("SSH_CONNECTION", "192.0.2.1 1 192.0.2.2 22")
            .start()?;
        term.command("? something slow")?;
        // Drawn again about a second later, and again; the time itself varies by a tick.
        let line = "• Asking OpenAI-compatible server (model-one)... ";
        term.expect(line, SHORT);
        term.expect(line, SHORT);
        term.expect(line, SHORT);
        assert_eq!(braille_seen(&term), 0, "{}", term.transcript());
        term.key(3)?;
        term.expect_plain("cancelled; nothing ran", SHORT);
        Ok(())
    });
}

#[test]
fn the_motion_setting_beats_ssh() {
    pty_test(BIN, |ctx| {
        let server = slow_server()?;
        configure(ctx, &server, "[ui]\nmotion = \"full\"\n")?;
        let mut term = ai_launch(ctx)
            .env("SSH_CONNECTION", "192.0.2.1 1 192.0.2.2 22")
            .start()?;
        term.command("? something slow")?;
        term.expect("⠙ Asking OpenAI-compatible server (model-one)... ", SHORT);
        term.key(3)?;
        term.expect_plain("cancelled; nothing ran", SHORT);
        Ok(())
    });
}

#[test]
fn with_motion_off_the_wait_line_is_one_plain_line() {
    pty_test(BIN, |ctx| {
        let server = slow_server()?;
        configure(ctx, &server, "[ui]\nmotion = \"off\"\n")?;
        let mut term = ai_launch(ctx).start()?;
        term.command("? something slow")?;
        term.expect("Asking OpenAI-compatible server (model-one)...\r\n", SHORT);
        term.pump(Duration::from_millis(1500));
        assert_eq!(braille_seen(&term), 0);
        assert!(
            !term.since_cursor().contains(" s"),
            "no time is drawn when nothing moves: {:?}",
            term.since_cursor()
        );
        term.key(3)?;
        term.expect_plain("cancelled; nothing ran", SHORT);
        Ok(())
    });
}

#[test]
fn in_the_c_locale_there_is_no_motion_even_if_the_setting_asks() {
    pty_test(BIN, |ctx| {
        let server = slow_server()?;
        configure(ctx, &server, "[ui]\nmotion = \"full\"\n")?;
        let mut term = ai_launch(ctx).env("LANG", "C").start()?;
        term.command("? something slow")?;
        term.expect("Asking OpenAI-compatible server (model-one)...\r\n", SHORT);
        term.pump(Duration::from_millis(800));
        assert_eq!(braille_seen(&term), 0);
        term.key(3)?;
        term.expect_plain("cancelled; nothing ran", SHORT);
        Ok(())
    });
}

fn plan_of(steps: &[(&str, &str)]) -> String {
    let steps: Vec<String> = steps
        .iter()
        .map(|(title, command)| {
            format!(
                r#"{{"title":"{title}","command":"{command}","explanation":"Because.","risk":"low","needs_root":false}}"#
            )
        })
        .collect();
    format!(
        r#"{{"summary":"Do the things","steps":[{}]}}"#,
        steps.join(",")
    )
}

#[test]
fn the_text_plan_has_the_ai_colored_bar_and_accent_keys() {
    pty_test(BIN, |ctx| {
        let answer = plan_of(&[("First", "true")]);
        let server = Server::start_answering(Duration::ZERO, false, &[&answer])?;
        configure(ctx, &server, "")?;
        let mut term = ai_launch(ctx)
            .env("NO_COLOR", "")
            .env("INSIDE_EMACS", "1")
            .start()?;
        term.command("? do it")?;
        term.expect(
            "\x1b[35mplan from OpenAI-compatible server (model-one)\x1b[0m",
            SHORT,
        );
        term.expect("\x1b[35m│\x1b[0m Do the things", SHORT);
        term.expect("\x1b[35m│\x1b[0m 1. First", SHORT);
        term.expect("\x1b[34m[Enter]\x1b[0m run", SHORT);
        term.write_bytes(b"\x1b")?;
        term.expect_plain("cancelled; nothing ran", SHORT);
        Ok(())
    });
}

#[test]
fn the_plan_panel_has_the_ai_colored_rail_the_risk_word_and_the_keys() {
    pty_test(BIN, |ctx| {
        let answer = plan_of(&[("First", "true")]);
        let server = Server::start_answering(Duration::ZERO, false, &[&answer])?;
        configure(ctx, &server, "")?;
        let mut term = ai_launch(ctx).env("NO_COLOR", "").start()?;
        term.command("? do it")?;
        wait_plan(&mut term, Mode::Panel);
        let seen = term.transcript();
        // The rail is the AI color, the risk is a word with its own color, and the keys of the
        // bottom rule are in the key color.
        assert!(seen.contains("\x1b[35m│\x1b[0m"), "{seen:?}");
        assert!(seen.contains("1. First  \x1b[32msafe\x1b[0m"), "{seen:?}");
        assert!(seen.contains("\x1b[36mEnter\x1b[0m"), "{seen:?}");
        plan_key(&mut term, Mode::Panel, b"\x1b")?;
        term.expect_plain("cancelled; nothing ran", SHORT);
        Ok(())
    });
}

#[test]
fn a_failed_step_marks_the_steps_that_did_not_run() {
    for mode in MODES {
        pty_test(BIN, move |ctx| {
            let answer = plan_of(&[
                ("First", "true"),
                ("Second", "false"),
                ("Third", "touch third"),
            ]);
            let server = Server::start_answering(Duration::ZERO, false, &[&answer])?;
            configure(ctx, &server, "")?;
            let launch = ai_launch(ctx);
            let mut term = match mode {
                Mode::Panel => launch.start()?,
                // The plain reader, in color, as the other tests of this file do.
                Mode::Dumb => launch.env("INSIDE_EMACS", "1").start()?,
            };
            term.command("? do it")?;
            wait_plan(&mut term, mode);
            plan_key(&mut term, mode, b"\r")?;
            term.expect_plain("[1/3] First", SHORT);
            term.expect_plain("✓", SHORT);
            term.expect_plain("[2/3] Second", SHORT);
            term.expect_plain("✗ (status 1)", SHORT);
            term.expect_plain("– 3. Third (not run)", SHORT);
            term.expect_plain("step 2 did not succeed", SHORT);
            assert!(
                !term.transcript().contains("never\r\n"),
                "step 3 must not run"
            );
            Ok(())
        });
    }
}
