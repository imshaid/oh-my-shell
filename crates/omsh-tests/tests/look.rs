//! The look of the shell (UI upgrade, phase 2), in a real terminal: the rule between commands,
//! the status chip at the right, the `ask AI` tag, the hint bar and its fade-out, and the OSC 133
//! marks.
//!
//! The test terminal draws nothing, so these tests read what the shell writes. The harness runs
//! in the C locale with `NO_COLOR=1` unless a test says otherwise, so by default the look is the
//! ASCII one without color (`-----`, `ok 4 ms`, `x 1`).

use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use omsh_tests::fakehttp::Server;
use omsh_tests::session::strip_escapes;
use omsh_tests::{Ctx, SHORT, Session, pty_test};

const BIN: &str = env!("CARGO_BIN_EXE_omsh-pty-shell");

const MARK_PROMPT: &str = "\x1b]133;A\x1b\\";
const MARK_OUTPUT: &str = "\x1b]133;C\x1b\\";

fn config(ctx: &Ctx, text: &str) -> io::Result<()> {
    let dir = ctx.home().join(".config/oh-my-shell");
    fs::create_dir_all(&dir)?;
    fs::write(dir.join("config.toml"), text)
}

/// A config file that is there, so that the start is not a first run (no welcome).
fn quiet(ctx: &Ctx) -> io::Result<()> {
    config(ctx, "")
}

/// A config file with the hint bar off, so that its words (`? ask AI`) are not mixed up with
/// the tag in the prompt.
fn no_bar(ctx: &Ctx) -> io::Result<()> {
    config(ctx, "[ui]\ngreeting = \"never\"\n")
}

/// What a person reads: the transcript without escape sequences.
fn text_of(term: &Session) -> String {
    strip_escapes(term.transcript()).0
}

fn state_file(ctx: &Ctx) -> PathBuf {
    ctx.home().join(".local/state/oh-my-shell/greeting")
}

/// The lines of `text` as a person reads them: escape sequences gone, `\r` gone.
fn lines_of(text: &str) -> Vec<String> {
    strip_escapes(text)
        .0
        .replace('\r', "")
        .lines()
        .map(str::to_owned)
        .collect()
}

/// How many of the lines are a rule: 40 or more dashes and nothing else.
fn rules_in(text: &str) -> usize {
    lines_of(text)
        .iter()
        .filter(|line| line.len() >= 40 && line.chars().all(|c| c == '-'))
        .count()
}

/// Runs `line` and waits for the prompt that follows it. Returns what the terminal showed from
/// just before the line to the end.
fn run(term: &mut Session, line: &str) -> io::Result<String> {
    term.prompt();
    let from = term.transcript().len();
    term.line(line)?;
    term.prompt();
    Ok(term.transcript()[from..].to_owned())
}

/// Waits until the editor has finished a paint (it hides the cursor when it starts and shows it
/// when it is done) after byte `from` of the transcript.
fn painted(term: &mut Session, from: usize) {
    let end = Instant::now() + SHORT;
    loop {
        let tail = &term.transcript()[from..];
        if let Some(start) = tail.find("\x1b[?25l")
            && tail[start..].contains("\x1b[?25h")
        {
            return;
        }
        assert!(
            Instant::now() < end,
            "the editor drew nothing after the key; the terminal showed:\n{}",
            term.transcript()
        );
        term.pump(Duration::from_millis(10));
    }
}

// -- the rule -----------------------------------------------------------------------------------

#[test]
fn no_rule_is_drawn_before_the_first_prompt() {
    pty_test(BIN, |ctx| {
        quiet(ctx)?;
        let mut term = ctx.start()?;
        assert_eq!(rules_in(term.transcript()), 0, "{}", term.transcript());
        term.quit()
    });
}

#[test]
fn a_command_is_followed_by_a_blank_line_and_a_rule() {
    pty_test(BIN, |ctx| {
        quiet(ctx)?;
        let mut term = ctx.start()?;
        let shown = run(&mut term, "echo hello")?;
        let lines = lines_of(&shown);
        let at = lines
            .iter()
            .position(|line| line.len() >= 40 && line.chars().all(|c| c == '-'))
            .unwrap_or_else(|| panic!("no rule after the command:\n{lines:#?}"));
        assert_eq!(rules_in(&shown), 1);
        assert_eq!(lines[at - 1], "", "a blank line above the rule");
        assert_eq!(lines[at - 2], "hello", "the output comes first");
        // The last column stays free: the rule is the window's width less one (80 - 1).
        assert_eq!(lines[at].len(), 79);
        term.quit()
    });
}

#[test]
fn the_rule_is_made_of_unicode_in_a_utf8_locale() {
    pty_test(BIN, |ctx| {
        quiet(ctx)?;
        let mut term = ctx.launch().env("LANG", "C.UTF-8").start()?;
        let shown = run(&mut term, "true")?;
        assert!(shown.contains(&"─".repeat(79)), "{shown}");
        term.quit()
    });
}

#[test]
fn nothing_is_drawn_for_a_line_that_ran_nothing() {
    pty_test(BIN, |ctx| {
        quiet(ctx)?;
        let mut term = ctx.start()?;
        let after_blank = run(&mut term, "")?;
        assert_eq!(rules_in(&after_blank), 0, "a blank line: {after_blank}");
        term.quit()
    });
}

/// A blank line ran nothing, so the chip goes on saying how the last real command ended, with
/// that command's own time (not the time of the blank line).
#[test]
fn a_blank_line_leaves_the_chip_of_the_last_command() {
    pty_test(BIN, |ctx| {
        quiet(ctx)?;
        let mut term = ctx.start()?;
        run(&mut term, "sleep 2.2")?;
        let after_blank = run(&mut term, "")?;
        let last = &after_blank[after_blank.rfind("\x1b[?25l").expect("a paint")..];
        let plain = strip_escapes(last).0;
        assert!(
            plain.contains("ok 2.2 s") || plain.contains("ok 2.3 s"),
            "the chip of the slow command is kept:\n{plain}"
        );
        term.quit()
    });
}

#[test]
fn no_rule_follows_ctrl_c() {
    pty_test(BIN, |ctx| {
        quiet(ctx)?;
        let mut term = ctx.start()?;
        term.prompt();
        let from = term.transcript().len();
        term.write_bytes(b"echo half")?;
        term.cancel_line()?;
        assert_eq!(
            rules_in(&term.transcript()[from..]),
            0,
            "{}",
            &term.transcript()[from..]
        );
        term.quit()
    });
}

#[test]
fn the_plain_style_is_the_prompt_of_version_1_0() {
    pty_test(BIN, |ctx| {
        config(ctx, "[ui]\nstyle = \"plain\"\n")?;
        let mut term = ctx.start()?;
        let shown = run(&mut term, "true")?;
        assert_eq!(rules_in(&shown), 0, "{shown}");
        // No success chip either: in the plain style the right side says only what failed.
        assert!(!shown.contains("ok "), "{shown}");
        let failed = run(&mut term, "false")?;
        assert!(failed.contains("x 1"), "{failed}");
        term.quit()
    });
}

// -- the chip -----------------------------------------------------------------------------------

#[test]
fn the_right_side_says_how_the_last_command_ended() {
    pty_test(BIN, |ctx| {
        quiet(ctx)?;
        let mut term = ctx.start()?;
        let shown = run(&mut term, "true")?;
        let (plain, _) = strip_escapes(&shown);
        assert!(
            plain.contains("ok <1 ms") || plain.contains(" ms"),
            "a success with a real time:\n{plain}"
        );
        assert!(plain.contains("ok "), "{plain}");
        let failed = run(&mut term, "false")?;
        // The last paint is the new prompt (the ones before it still show the old chip while
        // `false` is typed).
        let last = &failed[failed.rfind("\x1b[?25l").expect("a paint")..];
        let plain = strip_escapes(last).0;
        assert!(plain.contains("x 1"), "{plain}");
        assert!(
            !plain.contains("ok "),
            "no success chip after a failure: {plain}"
        );
        let missing = run(&mut term, "no-such-command-here")?;
        assert!(strip_escapes(&missing).0.contains("x 127"), "{missing}");
        term.quit()
    });
}

#[test]
fn colors_follow_the_meaning_green_for_ok_and_red_for_failed() {
    pty_test(BIN, |ctx| {
        quiet(ctx)?;
        let mut term = ctx
            .launch()
            .env("NO_COLOR", "")
            .env("LANG", "C.UTF-8")
            .start()?;
        let shown = run(&mut term, "true")?;
        assert!(shown.contains("\x1b[32m✓ "), "green check:\n{shown}");
        let failed = run(&mut term, "false")?;
        assert!(
            failed.contains("\x1b[31m✘ 1\x1b[0m"),
            "red cross:\n{failed}"
        );
        // The rule is dim, the directory blue.
        assert!(failed.contains("\x1b[2;90m─"), "{failed}");
        term.quit()
    });
}

// -- the tag ------------------------------------------------------------------------------------

/// Typing `?` makes `ask AI` appear on the right of the prompt and Backspace takes it away: the
/// editor repaints the right side with each key, not only when the prompt is first drawn.
#[test]
fn ask_ai_appears_with_a_question_mark_and_goes_with_backspace() {
    pty_test(BIN, |ctx| {
        no_bar(ctx)?;
        let mut term = ctx.start()?;
        term.prompt();
        assert!(!term.transcript().contains("ask AI"), "not before a key");

        let before_key = term.transcript().len();
        term.write_bytes(b"?")?;
        painted(&mut term, before_key);
        // The tag was written by a repaint caused by the key, after the prompt was up.
        assert!(
            term.transcript()[before_key..].contains("ask AI"),
            "{}",
            &term.transcript()[before_key..]
        );

        let before_backspace = term.transcript().len();
        term.write_bytes(&[0x7f])?;
        painted(&mut term, before_backspace);
        let repaint = &term.transcript()[before_backspace..];
        // The whole prompt is drawn again after the screen below it is cleared, and the new
        // right side does not have the tag.
        let cleared = ["\x1b[J", "\x1b[0J"]
            .iter()
            .filter_map(|clear| repaint.rfind(clear))
            .max()
            .unwrap_or_else(|| panic!("the prompt was not cleared and drawn again:\n{repaint:?}"));
        let drawn = &repaint[cleared..];
        assert!(
            drawn.contains("$ "),
            "the prompt is drawn again:\n{drawn:?}"
        );
        assert!(
            !drawn.contains("ask AI"),
            "the tag is still there after Backspace:\n{drawn:?}"
        );
        term.cancel_line()?;
        term.quit()
    });
}

#[test]
fn the_tag_goes_with_every_kind_of_request_and_comes_back() {
    pty_test(BIN, |ctx| {
        no_bar(ctx)?;
        let mut term = ctx.start()?;
        term.prompt();
        for typed in ["?? why", "ls | ? count"] {
            let from = term.transcript().len();
            term.write_bytes(typed.as_bytes())?;
            painted(&mut term, from);
            assert!(term.transcript()[from..].contains("ask AI"), "{typed}");
            term.cancel_line()?;
        }
        term.quit()
    });
}

#[test]
fn no_tag_while_the_ai_is_switched_off() {
    pty_test(BIN, |ctx| {
        config(ctx, "[ai]\nenabled = false\n")?;
        let mut term = ctx.start()?;
        term.prompt();
        let from = term.transcript().len();
        term.write_bytes(b"? hello")?;
        painted(&mut term, from);
        assert!(
            !term.transcript().contains("ask AI"),
            "{}",
            term.transcript()
        );
        term.cancel_line()?;
        term.quit()
    });
}

// -- the OSC 133 marks --------------------------------------------------------------------------

#[test]
fn the_marks_come_in_order_prompt_output_end() {
    pty_test(BIN, |ctx| {
        quiet(ctx)?;
        let mut term = ctx.start()?;
        run(&mut term, "true")?;
        run(&mut term, "false")?;
        let text = term.transcript().to_owned();
        let mut at = 0;
        for (what, mark) in [
            ("A, first prompt", MARK_PROMPT.to_owned()),
            ("C, before true", MARK_OUTPUT.to_owned()),
            ("D;0", "\x1b]133;D;0\x1b\\".to_owned()),
            ("A, second prompt", MARK_PROMPT.to_owned()),
            ("C, before false", MARK_OUTPUT.to_owned()),
            ("D;1", "\x1b]133;D;1\x1b\\".to_owned()),
            ("A, third prompt", MARK_PROMPT.to_owned()),
        ] {
            let found = text[at..]
                .find(&mark)
                .unwrap_or_else(|| panic!("no {what} after byte {at}:\n{text:?}"));
            at += found + mark.len();
        }
        term.quit()
    });
}

#[test]
fn the_marks_do_not_disturb_what_the_harness_reads_as_the_prompt() {
    pty_test(BIN, |ctx| {
        quiet(ctx)?;
        let mut term = ctx.start()?;
        // `sync` waits for the output and then for the prompt, as a person's eyes would.
        term.sync("one")?;
        term.sync("two")?;
        assert!(!strip_escapes(term.transcript()).0.contains("133;"));
        term.quit()
    });
}

#[test]
fn no_marks_on_the_linux_console() {
    pty_test(BIN, |ctx| {
        quiet(ctx)?;
        let mut term = ctx.launch().env("TERM", "linux").start()?;
        run(&mut term, "true")?;
        assert!(
            !term.transcript().contains("\x1b]133"),
            "{:?}",
            term.transcript()
        );
        // The look itself is still there.
        assert_eq!(rules_in(term.transcript()), 1);
        term.quit()
    });
}

#[test]
fn no_marks_for_a_dumb_terminal_or_a_command_string() {
    pty_test(BIN, |ctx| {
        quiet(ctx)?;
        let mut dumb = ctx.launch().dumb().start()?;
        dumb.line("true")?;
        dumb.prompt();
        assert!(
            !dumb.transcript().contains("\x1b]"),
            "{:?}",
            dumb.transcript()
        );
        dumb.quit()?;

        let mut command = ctx.launch().arg("-c").arg("echo ran").start_raw()?;
        command.expect("ran", SHORT);
        command.wait_exit(SHORT);
        assert!(
            !command.transcript().contains("\x1b]133"),
            "{:?}",
            command.transcript()
        );
        Ok(())
    });
}

// -- the hint bar -------------------------------------------------------------------------------

/// Words of the hint bar. (Its keys and its words are drawn in different styles, so this is
/// looked for in the text a person reads, not in the bytes.)
const BAR: &str = "Ctrl+P palette";

#[test]
fn no_hint_bar_in_the_session_that_shows_the_welcome() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        assert!(term.transcript().contains("Welcome to Oh My Shell."));
        assert!(!text_of(&term).contains(BAR), "{}", term.transcript());
        term.quit()?;
        assert!(
            !state_file(ctx).exists(),
            "the welcome session is not counted"
        );
        Ok(())
    });
}

#[test]
fn the_hint_bar_is_shown_for_five_sessions_after_the_welcome_and_then_never() {
    pty_test(BIN, |ctx| {
        // The welcome session.
        ctx.start()?.quit()?;
        for n in 1..=5 {
            let mut term = ctx.start()?;
            let plain = text_of(&term);
            assert!(
                plain.contains("? ask AI - Ctrl+P palette - Ctrl+R history - omsh tour"),
                "session {n} after the welcome:\n{plain}"
            );
            term.quit()?;
            assert_eq!(fs::read_to_string(state_file(ctx))?, format!("shown={n}\n"));
        }
        let mut term = ctx.start()?;
        assert!(!text_of(&term).contains(BAR), "{}", term.transcript());
        term.quit()?;
        assert_eq!(fs::read_to_string(state_file(ctx))?, "shown=5\n");
        Ok(())
    });
}

#[test]
fn the_bar_is_above_the_first_prompt_and_names_ai_setup_until_the_ai_is_set_up() {
    pty_test(BIN, |ctx| {
        quiet(ctx)?;
        let term = ctx.start()?;
        let text = text_of(&term);
        let bar = text.find(BAR).expect("a hint bar");
        let prompt = text.find("$ ").expect("a prompt");
        assert!(bar < prompt, "{text}");
        assert!(text.contains("omsh ai setup"), "{text}");
        Ok(())
    });
}

#[test]
fn a_ready_ai_does_not_get_the_setup_hint_and_an_off_ai_does_not_get_the_question_mark() {
    pty_test(BIN, |ctx| {
        config(
            ctx,
            "[ai]\nprovider = \"ollama\"\nmodel = \"m\"\n[ui]\ngreeting = \"always\"\n",
        )?;
        let term = ctx.start()?;
        let (plain, _) = strip_escapes(term.transcript());
        assert!(plain.contains("? ask AI"), "{plain}");
        assert!(!plain.contains("omsh ai setup"), "{plain}");
        drop(term);

        let term = ctx.launch().arg("--no-ai").start()?;
        let (plain, _) = strip_escapes(term.transcript());
        assert!(plain.contains(BAR), "{plain}");
        assert!(!plain.contains("ask AI"), "{plain}");
        assert!(!plain.contains("omsh ai setup"), "{plain}");
        Ok(())
    });
}

#[test]
fn greeting_never_and_always() {
    pty_test(BIN, |ctx| {
        config(ctx, "[ui]\ngreeting = \"never\"\n")?;
        for _ in 0..2 {
            let mut term = ctx.start()?;
            assert!(!text_of(&term).contains(BAR));
            term.quit()?;
        }
        assert!(!state_file(ctx).exists());

        config(ctx, "[ui]\ngreeting = \"always\"\n")?;
        for _ in 0..7 {
            let mut term = ctx.start()?;
            assert!(text_of(&term).contains(BAR), "{}", term.transcript());
            term.quit()?;
        }
        assert!(!state_file(ctx).exists(), "always does not count");
        Ok(())
    });
}

#[test]
fn no_hint_bar_without_a_line_editor_or_without_a_config_folder() {
    pty_test(BIN, |ctx| {
        quiet(ctx)?;
        // The plain reader has no Ctrl+P and no Ctrl+R.
        let mut dumb = ctx.launch().dumb().start()?;
        assert!(!text_of(&dumb).contains(BAR), "{}", dumb.transcript());
        dumb.quit()?;
        // `--no-config` has nowhere to count: `auto` stays quiet and nothing is written.
        let mut term = ctx.launch().arg("--no-config").start()?;
        assert!(!text_of(&term).contains(BAR), "{}", term.transcript());
        term.quit()?;
        assert!(!state_file(ctx).exists());
        Ok(())
    });
}

#[test]
fn the_bar_is_only_for_a_terminal_not_for_a_command_string() {
    pty_test(BIN, |ctx| {
        quiet(ctx)?;
        let mut term = ctx.launch().arg("-c").arg("echo ran").start_raw()?;
        term.expect("ran", SHORT);
        term.wait_exit(SHORT);
        assert!(!text_of(&term).contains(BAR), "{}", term.transcript());
        assert!(!state_file(ctx).exists());
        Ok(())
    });
}

// -- Bangla -------------------------------------------------------------------------------------

#[test]
fn a_bangla_folder_is_whole_in_the_header_and_editing_moves_by_letter() {
    pty_test(BIN, |ctx| {
        quiet(ctx)?;
        let folder = ctx.home().join("প্রকল্প");
        fs::create_dir_all(&folder)?;
        fs::write(folder.join("রিপোর্ট.txt"), "x")?;
        let mut term = ctx
            .launch()
            .env("LANG", "C.UTF-8")
            .env("NO_COLOR", "")
            .cwd(&folder)
            .start()?;
        let (plain, _) = strip_escapes(term.transcript());
        assert!(plain.contains("~/প্রকল্প"), "{plain}");

        // The existing file is underlined from its first letter to its last.
        term.prompt();
        term.write_bytes("cat রিপোর্ট.txt".as_bytes())?;
        term.expect("\x1b[4mরিপোর্ট.txt", SHORT);
        term.cancel_line()?;

        // Left moves over a whole letter with its vowel sign: `বি` is one step.
        term.prompt();
        term.write_bytes("echo ছবি".as_bytes())?;
        term.write_bytes(b"\x1b[D")?;
        term.write_bytes(b"X")?;
        term.write_bytes(b"\r")?;
        term.expect_plain("ছXবি", SHORT);
        term.prompt();
        term.quit()
    });
}

// -- NO_COLOR and a slow command ----------------------------------------------------------------

/// `NO_COLOR` takes the colors away and keeps what is not a color: the rule is still dim, and the
/// chip still says `ok` and `x 1` in words.
#[test]
fn no_color_keeps_dim_and_the_words() {
    pty_test(BIN, |ctx| {
        quiet(ctx)?;
        // The harness sets NO_COLOR=1.
        let mut term = ctx.launch().env("LANG", "C.UTF-8").start()?;
        let ok = run(&mut term, "true")?;
        assert!(ok.contains("\x1b[2m───"), "the rule stays dim:\n{ok}");
        assert!(strip_escapes(&ok).0.contains("✓ "), "{ok}");
        let failed = run(&mut term, "false")?;
        assert!(strip_escapes(&failed).0.contains("✘ 1"), "{failed}");
        for shown in [&ok, &failed] {
            // Dim is not a color: 2 is allowed, 30 to 37 and 90 to 97 are not.
            let mut rest = shown.as_str();
            while let Some(at) = rest.find("\x1b[") {
                rest = &rest[at + 2..];
                let end = rest
                    .find(|c: char| !(c.is_ascii_digit() || c == ';'))
                    .unwrap_or(0);
                for code in rest[..end].split(';').filter_map(|c| c.parse::<u16>().ok()) {
                    assert!(
                        !matches!(code, 30..=38 | 40..=48 | 90..=97 | 100..=107),
                        "color {code} under NO_COLOR:\n{shown:?}"
                    );
                }
            }
        }
        term.quit()
    });
}

/// A command that took over two seconds says so, with a real number.
#[test]
fn a_slow_command_shows_its_time() {
    pty_test(BIN, |ctx| {
        quiet(ctx)?;
        let mut term = ctx.start()?;
        let shown = run(&mut term, "sleep 2.2")?;
        let (plain, _) = strip_escapes(&shown);
        assert!(
            plain.contains("ok 2.2 s") || plain.contains("ok 2.3 s"),
            "{plain}"
        );
        term.quit()
    });
}

// -- the explain hint (phase 3) -----------------------------------------------------------------

/// A shell whose AI is set up (a fake server on this machine), with the hint bar off.
fn with_ai(ctx: &Ctx, server: &Server) -> io::Result<()> {
    config(
        ctx,
        &format!(
            "[ui]\ngreeting = \"never\"\n[ai]\nprovider = \"openai-compatible\"\nmodel = \"m\"\nbase_url = \"{}\"\n",
            server.url("/v1")
        ),
    )
}

fn offline_launch(ctx: &Ctx) -> omsh_tests::Launch {
    ctx.launch().env(
        "DBUS_SESSION_BUS_ADDRESS",
        "unix:path=/nonexistent/omsh-test-bus",
    )
}

/// The text of the prompt that was drawn last: from the last `$ ` back to the line before it.
fn last_header(term: &Session) -> String {
    let text = text_of(term).replace('\r', "");
    let rows: Vec<&str> = text.lines().collect();
    rows.iter()
        .rev()
        .find(|row| row.contains("x 1") || row.contains("ok ") || row.contains("Alt+?"))
        .copied()
        .unwrap_or_default()
        .to_owned()
}

#[test]
fn after_a_failure_the_prompt_says_alt_question_mark_explains_when_the_ai_is_ready() {
    pty_test(BIN, |ctx| {
        let server = Server::start_answering(Duration::ZERO, false, &["It failed."])?;
        with_ai(ctx, &server)?;
        let mut term = offline_launch(ctx).start()?;
        term.command("false")?;
        term.prompt();
        let header = last_header(&term);
        assert!(header.contains("Alt+? explain x 1"), "{header:?}");
        // After a success there is nothing to explain.
        term.command("true")?;
        term.prompt();
        let header = last_header(&term);
        assert!(!header.contains("Alt+?"), "{header:?}");
        assert!(server.requests().is_empty(), "the hint is not a request");
        term.quit()
    });
}

#[test]
fn the_hint_is_not_shown_when_nothing_can_be_asked() {
    // No AI set up, `--no-ai`, and `ai.enabled = false`: no hint, only the result.
    for (name, config_text, arg) in [
        ("not set up", "[ui]\ngreeting = \"never\"\n", None),
        (
            "--no-ai",
            "[ui]\ngreeting = \"never\"\n[ai]\nprovider = \"ollama\"\nmodel = \"m\"\n",
            Some("--no-ai"),
        ),
        (
            "switched off",
            "[ui]\ngreeting = \"never\"\n[ai]\nenabled = false\n",
            None,
        ),
    ] {
        pty_test(BIN, move |ctx| {
            config(ctx, config_text)?;
            let mut launch = ctx.launch();
            if let Some(arg) = arg {
                launch = launch.arg(arg);
            }
            let mut term = launch.start()?;
            term.command("false")?;
            term.prompt();
            let header = last_header(&term);
            assert!(header.contains("x 1"), "{name}: {header:?}");
            assert!(!header.contains("Alt+?"), "{name}: {header:?}");
            term.quit()
        });
    }
}

#[test]
fn the_hint_is_not_shown_in_the_plain_style() {
    pty_test(BIN, |ctx| {
        let server = Server::start_answering(Duration::ZERO, false, &["x"])?;
        config(
            ctx,
            &format!(
                "[ui]\ngreeting = \"never\"\nstyle = \"plain\"\n[ai]\nprovider = \"openai-compatible\"\nmodel = \"m\"\nbase_url = \"{}\"\n",
                server.url("/v1")
            ),
        )?;
        let mut term = offline_launch(ctx).start()?;
        term.command("false")?;
        term.prompt();
        assert!(!text_of(&term).contains("Alt+?"), "{}", text_of(&term));
        term.quit()
    });
}

#[test]
fn the_hint_goes_once_alt_question_mark_has_been_used() {
    pty_test(BIN, |ctx| {
        let server = Server::start_answering(Duration::ZERO, false, &["It failed because."])?;
        with_ai(ctx, &server)?;
        let mut term = offline_launch(ctx).start()?;
        term.command("false")?;
        term.prompt();
        assert!(last_header(&term).contains("Alt+? explain"));
        let from = term.transcript().len();
        term.write_bytes(b"\x1b?")?;
        term.expect("It failed because.", SHORT);
        let answered = from
            + term.transcript()[from..]
                .find("It failed because.")
                .unwrap();
        painted(&mut term, answered);
        // The prompt that came back keeps the result and no longer offers the key.
        let text = text_of(&term);
        let after = &text[text.rfind("It failed because.").unwrap()..];
        assert!(after.contains("x 1"), "{after:?}");
        assert!(!after.contains("Alt+?"), "{after:?}");
        assert_eq!(server.requests().len(), 1, "one explanation was asked for");
        term.cancel_line()?;
        term.quit()
    });
}
