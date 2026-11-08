//! Syntax highlighting in the line editor (S14a), on a terminal that takes escape codes.
//!
//! The terminal draws nothing, so these tests read the escape codes the editor writes while
//! text is typed (without pressing Enter) and look for the color around each word. The colors
//! are the 16 ANSI ones: red is `ESC[31m`, cyan `ESC[36m`, and so on. The harness sets
//! `NO_COLOR=1`; the tests that need colors set it empty, which means "not set".

use std::fs;
use std::io;
use std::path::Path;

use omsh_tests::{Ctx, SHORT, Session, pty_test};

const BIN: &str = env!("CARGO_BIN_EXE_omsh-pty-shell");

const CTRL_C: u8 = 3;
const RED: &str = "\x1b[31m";
const YELLOW: &str = "\x1b[33m";
const GREEN: &str = "\x1b[32m";
const CYAN: &str = "\x1b[36m";

/// True if the output sets a foreground or background color (30-38, 40-48, 90-97, 100-107), as
/// opposed to bold, dim, underline, reset or the terminal's default color (39, 49).
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

/// A shell with colors on, started in `dir`.
fn colored(ctx: &Ctx, dir: &Path) -> io::Result<Session> {
    ctx.launch().env("NO_COLOR", "").cwd(dir).start()
}

/// Types `text` without pressing Enter, waits until the editor has drawn `shown`, and returns
/// everything the editor wrote while doing so.
fn type_and_see(term: &mut Session, text: &str, shown: &str) -> io::Result<String> {
    let mark = term.transcript().len();
    term.write_bytes(text.as_bytes())?;
    term.expect(shown, SHORT);
    Ok(term.transcript()[mark..].to_owned())
}

/// Throws the typed text away (Ctrl+C) and waits for the next prompt.
fn clear(term: &mut Session) -> io::Result<()> {
    term.key(CTRL_C)?;
    term.expect("$ ", SHORT);
    Ok(())
}

/// Ends a test: the half-typed line must go first, or `exit` would be typed after it.
fn finish(term: &mut Session) -> io::Result<()> {
    clear(term)?;
    term.quit()
}

#[test]
fn a_command_that_is_not_found_is_red_and_one_that_exists_is_not() {
    pty_test(BIN, |ctx| {
        let mut term = colored(ctx, ctx.home())?;
        let drawn = type_and_see(&mut term, "nosuchcmd-xyz", &format!("{RED}nosuchcmd-xyz"))?;
        assert!(drawn.contains(RED), "{drawn:?}");
        clear(&mut term)?;

        // `ls` exists. The option after it is drawn (cyan), so the whole line has been painted,
        // and no red may be in it.
        let drawn = type_and_see(&mut term, "ls -l", &format!("{CYAN}-l"))?;
        assert!(!drawn.contains(RED), "a found command was red: {drawn:?}");
        // A builtin too.
        clear(&mut term)?;
        let drawn = type_and_see(&mut term, "cd -P", &format!("{CYAN}-P"))?;
        assert!(!drawn.contains(RED), "{drawn:?}");
        finish(&mut term)
    });
}

#[test]
fn the_color_follows_the_word_while_it_is_edited() {
    pty_test(BIN, |ctx| {
        let mut term = colored(ctx, ctx.home())?;
        type_and_see(&mut term, "lsx", &format!("{RED}lsx"))?;
        // Backspace: `ls` is a command again, so the red goes away.
        let mark = term.transcript().len();
        term.write_bytes(b"\x7f -l")?;
        term.expect(&format!("{CYAN}-l"), SHORT);
        let drawn = &term.transcript()[mark..];
        assert!(!drawn.contains(&format!("{RED}ls")), "{drawn:?}");
        finish(&mut term)
    });
}

#[test]
fn quotes_operators_and_separators_have_their_colors() {
    pty_test(BIN, |ctx| {
        let mut term = colored(ctx, ctx.home())?;
        let drawn = type_and_see(
            &mut term,
            "echo 'a b' ; ls | cat && ls > out",
            &format!("{CYAN}>"),
        )?;
        assert!(drawn.contains(&format!("{YELLOW}'a b'")), "{drawn:?}");
        assert!(drawn.contains(&format!("{GREEN};")), "{drawn:?}");
        assert!(drawn.contains(&format!("{CYAN}|")), "{drawn:?}");
        assert!(drawn.contains(&format!("{CYAN}&&")), "{drawn:?}");
        finish(&mut term)
    });
}

#[test]
fn a_comment_is_dim_red() {
    pty_test(BIN, |ctx| {
        let mut term = colored(ctx, ctx.home())?;
        let drawn = type_and_see(&mut term, "ls # note", "# note")?;
        assert!(
            drawn.contains("\x1b[2;31m# note") || drawn.contains("\x1b[31;2m# note"),
            "{drawn:?}"
        );
        finish(&mut term)
    });
}

#[test]
fn an_existing_file_is_underlined_and_keeps_its_color() {
    pty_test(BIN, |ctx| {
        fs::write(ctx.home().join("marker.txt"), "x")?;
        let mut term = colored(ctx, ctx.home())?;
        let drawn = type_and_see(&mut term, "cat marker.txt nope.txt", "nope.txt")?;
        // Underlined and cyan, in either order of the two codes.
        assert!(
            drawn.contains("\x1b[4;36mmarker.txt") || drawn.contains("\x1b[36;4mmarker.txt"),
            "{drawn:?}"
        );
        assert!(
            !drawn.contains("\x1b[4;36mnope.txt") && !drawn.contains("\x1b[36;4mnope.txt"),
            "a missing file was underlined: {drawn:?}"
        );
        assert!(drawn.contains(&format!("{CYAN}nope.txt")), "{drawn:?}");
        finish(&mut term)
    });
}

#[test]
fn functions_and_aliases_are_known_commands() {
    pty_test(BIN, |ctx| {
        let dir = ctx.home().join(".config/oh-my-shell");
        fs::create_dir_all(&dir)?;
        fs::write(dir.join("init.omsh"), "myfn() { :; }\nalias myal=ls\n")?;
        let mut term = colored(ctx, ctx.home())?;
        let drawn = type_and_see(&mut term, "myfn -x", &format!("{CYAN}-x"))?;
        assert!(!drawn.contains(RED), "a function was red: {drawn:?}");
        clear(&mut term)?;
        let drawn = type_and_see(&mut term, "myal -x", &format!("{CYAN}-x"))?;
        assert!(!drawn.contains(RED), "an alias was red: {drawn:?}");
        // And the check is live: a name that is none of them is red.
        clear(&mut term)?;
        type_and_see(&mut term, "myother", &format!("{RED}myother"))?;
        finish(&mut term)
    });
}

#[test]
fn a_program_installed_by_the_last_command_is_found_at_the_next_prompt() {
    pty_test(BIN, |ctx| {
        let mut term = colored(ctx, ctx.home())?;
        term.line("mkdir -p $HOME/bin; export PATH=$HOME/bin:$PATH")?;
        term.sync("path")?;
        // Not installed yet: red.
        type_and_see(&mut term, "newtool", &format!("{RED}newtool"))?;
        clear(&mut term)?;
        term.line("printf '#!/bin/sh\\n' > $HOME/bin/newtool; chmod +x $HOME/bin/newtool")?;
        term.sync("made")?;
        let drawn = type_and_see(&mut term, "newtool -x", &format!("{CYAN}-x"))?;
        assert!(!drawn.contains(RED), "the new program was red: {drawn:?}");
        finish(&mut term)
    });
}

#[test]
fn no_color_removes_the_colors_and_keeps_underline_and_dim() {
    pty_test(BIN, |ctx| {
        fs::write(ctx.home().join("marker.txt"), "x")?;
        // The harness sets NO_COLOR=1.
        let mut term = ctx.launch().cwd(ctx.home()).start()?;
        let drawn = type_and_see(&mut term, "nosuch 'q' marker.txt ; ls # why", "# why")?;
        assert!(!sets_a_color(&drawn), "{drawn:?}");
        assert!(drawn.contains("\x1b[4mmarker.txt"), "{drawn:?}");
        assert!(drawn.contains("\x1b[2m# why"), "{drawn:?}");
        finish(&mut term)
    });
}

#[test]
fn nothing_is_ever_bold_in_the_command_line() {
    pty_test(BIN, |ctx| {
        let mut term = colored(ctx, ctx.home())?;
        // The prompt's directory is bold; the typed line must add no bold.
        let drawn = type_and_see(
            &mut term,
            "nosuch -a 'b' \"c\" $d ; ls | cat > f # z",
            "# z",
        )?;
        // Everything after the last prompt symbol is the typed line.
        let line = drawn
            .rsplit_once("$ ")
            .map_or(drawn.as_str(), |(_, line)| line);
        assert!(line.contains(RED), "the line was not highlighted: {line:?}");
        assert!(
            !line.contains("\x1b[1m"),
            "bold in the typed line: {line:?}"
        );
        assert!(!line.contains(";1m") && !line.contains("[1;"), "{line:?}");
        finish(&mut term)
    });
}
