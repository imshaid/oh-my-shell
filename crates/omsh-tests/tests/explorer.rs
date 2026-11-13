//! The history explorer on `Ctrl+R` (S27a), on a terminal that takes escape codes.
//!
//! The harness keeps the bytes the shell wrote and does not draw a screen, and the view writes
//! only the cells that changed (blank cells are skipped, so the text of a row is not always
//! contiguous). So these tests look at what the view *does*: which command ends up in the line,
//! what runs, what is stored, how the terminal is left. How it looks is tested on a test
//! backend in `omsh-ui`, and was looked at in tmux.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use nix::sys::signal::Signal;
use nix::sys::termios::LocalFlags;
use omsh_history::Store;
use omsh_tests::procfs::hup_is_ignored;
use omsh_tests::skip::skip;
use omsh_tests::{Ctx, SHORT, Session, pty_test};

const BIN: &str = env!("CARGO_BIN_EXE_omsh-pty-shell");

const CTRL_C: u8 = 3;
const CTRL_R: u8 = 0x12;
const ESC: u8 = 0x1b;

/// The terminal switches to and from the alternate screen.
const ENTER: &str = "\x1b[?1049h";
const LEAVE: &str = "\x1b[?1049l";

/// One command to put in the history before the shell starts.
struct Row {
    command: &'static str,
    cwd: String,
    status: i32,
}

fn row(command: &'static str, cwd: &str, status: i32) -> Row {
    Row {
        command,
        cwd: cwd.to_owned(),
        status,
    }
}

/// Writes `rows` (oldest first) to the history database the shell will open.
fn seed(ctx: &Ctx, rows: &[Row]) -> io::Result<()> {
    let data = ctx.home().join(".local/share/oh-my-shell");
    fs::create_dir_all(&data)?;
    let store = Store::open(&data.join("history.db"), 100_000);
    for row in rows {
        let token = store.start(row.command, &row.cwd);
        store.finish(token, row.status, Duration::from_millis(5));
    }
    assert!(
        store.settle(Duration::from_secs(10)),
        "the history was not written"
    );
    store.close(Duration::from_secs(5));
    Ok(())
}

/// A folder of its own in the test's home, as the shell will see it (symbolic links resolved).
fn folder(ctx: &Ctx, name: &str) -> io::Result<PathBuf> {
    let path = ctx.home().join(name);
    fs::create_dir_all(&path)?;
    path.canonicalize()
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Presses `Ctrl+R` and waits until the view is on the screen. The first drawing is written top
/// to bottom (title, list, detail, note, query, keys), so a test that wants to see something in
/// it asks for it in that order, and then calls [`first_drawing_done`]. Later drawings write
/// only the cells that changed, so their text is not found whole.
fn press_ctrl_r(term: &mut Session) -> io::Result<()> {
    term.key(CTRL_R)?;
    term.expect(ENTER, SHORT);
    Ok(())
}

/// Waits for the end of the first drawing: the note about what the list covers.
fn first_drawing_done(term: &mut Session) {
    term.expect_plain("Looks at the newest 10,000 runs only", SHORT);
}

/// Runs the line without storing it in the history (a leading space is an ignored line), so that
/// running a picked command does not change what the next view lists.
fn run_unstored(term: &mut Session) -> io::Result<()> {
    term.write_bytes(b"\x01 \r")
}

/// Opens the view and waits until it is fully drawn.
fn open_view(term: &mut Session) -> io::Result<()> {
    press_ctrl_r(term)?;
    first_drawing_done(term);
    Ok(())
}

/// Waits until the view has given the screen back and the line editor has drawn the text it
/// holds again.
fn left_view(term: &mut Session) {
    term.expect(LEAVE, SHORT);
}

fn counts(term: &Session) -> (usize, usize) {
    let all = term.transcript();
    (all.matches(ENTER).count(), all.matches(LEAVE).count())
}

/// The text of the line goes to the view as the first query; Enter in the view puts the best
/// match in the line and does not run it; a second Enter runs it.
#[test]
fn a_chosen_command_goes_into_the_line_and_runs_only_after_enter() {
    pty_test(BIN, |ctx| {
        seed(
            ctx,
            &[
                row("echo picked-$((1+1))", "/x", 0),
                row("ls /", "/x", 0),
                row("date", "/x", 0),
            ],
        )?;
        let mut term = ctx.start()?;
        // The shell opens its history on a thread of its own, after the first prompt; Ctrl+R
        // right away would find it not ready (see `omsh_tests::files`).
        term.wait_for_history()?;
        open_view(&mut term)?;
        term.write_bytes(b"pick")?;
        term.write_bytes(b"\r")?;
        left_view(&mut term);
        // In the line, not run.
        term.expect_plain("echo picked-$((1+1))", SHORT);
        assert!(
            !term.try_expect("picked-2", Duration::from_millis(400)),
            "the command ran without Enter:\n{}",
            term.transcript()
        );
        term.write_bytes(b"\r")?;
        term.expect("picked-2", SHORT);
        term.prompt();
        term.quit()
    });
}

/// `Ctrl+R` in the first moments of a shell, while the history writer has not finished opening
/// the file (S28c). The view used to wait 50 ms and then say "The history cannot be read right
/// now."; it now waits for the writer, as the `history` builtin does. To make the state certain,
/// another connection holds the database locked, so the writer is stuck in its first step when
/// `Ctrl+R` is pressed; the lock is let go 400 ms later.
#[test]
fn ctrl_r_waits_for_a_history_file_that_is_still_opening() {
    pty_test(BIN, |ctx| {
        seed(ctx, &[row("echo listed-here", "/x", 0)])?;
        let blocker = rusqlite::Connection::open(ctx.history_file()).expect("open the history");
        blocker
            .execute_batch("BEGIN EXCLUSIVE")
            .expect("take the lock");
        let mut term = ctx.start()?;
        // The shell is up and its writer is stuck. Let go only after the key has been sent.
        let started = std::time::Instant::now();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(400));
            blocker.execute_batch("COMMIT").expect("let go");
        });
        press_ctrl_r(&mut term)?;
        assert!(
            started.elapsed() >= Duration::from_millis(300),
            "the view opened after {:?}: the writer was not held back, so this test shows nothing",
            started.elapsed()
        );
        release.join().expect("the lock holder ended");
        term.write_bytes(b"listed")?;
        term.write_bytes(b"\r")?;
        left_view(&mut term);
        term.expect_plain("echo listed-here", SHORT);
        term.cancel_line()?;
        term.quit()
    });
}

/// In vi mode `Ctrl+R` opens the view in insert mode and in normal mode.
#[test]
fn ctrl_r_works_in_both_vi_modes() {
    pty_test(BIN, |ctx| {
        let config = ctx.home().join(".config/oh-my-shell");
        fs::create_dir_all(&config)?;
        fs::write(config.join("config.toml"), "[editor]\nmode = \"vi\"\n")?;
        seed(ctx, &[row("echo vi-$((3+4))", "/x", 0)])?;
        let mut term = ctx.start()?;
        term.wait_for_history()?;
        // Insert mode.
        open_view(&mut term)?;
        term.key(ESC)?;
        left_view(&mut term);
        // Normal mode: Escape in the editor (the prompt shows `: `), then the key. Sent together
        // they would be read as one key, Alt+Ctrl+R.
        term.key(ESC)?;
        term.expect(": ", SHORT);
        press_ctrl_r(&mut term)?;
        first_drawing_done(&mut term);
        term.write_bytes(b"\r")?;
        assert_eq!(counts(&term), (2, 1));
        left_view(&mut term);
        // The chosen command is in the line; Enter in normal mode runs it.
        term.write_bytes(b"\r")?;
        term.expect("vi-7", SHORT);
        term.quit()
    });
}

/// A chosen command replaces what was typed; it is not added to it.
#[test]
fn a_chosen_command_replaces_what_was_typed() {
    pty_test(BIN, |ctx| {
        seed(ctx, &[row("echo xx-$((1+1))", "/x", 0), row("ls", "/x", 0)])?;
        let mut term = ctx.start()?;
        term.wait_for_history()?;
        term.write_bytes(b"xx")?;
        open_view(&mut term)?;
        term.write_bytes(b"\r")?;
        left_view(&mut term);
        // If `xx` were still in the line, this would run `xxecho`.
        run_unstored(&mut term)?;
        term.expect("xx-2", SHORT);
        assert!(!term.transcript().contains("xxecho: not found"));
        term.prompt();
        term.quit()
    });
}

/// What was typed before `Ctrl+R` is the first query (the title counts one match of three), and
/// leaving with Escape keeps the typed text in the line.
#[test]
fn text_typed_before_ctrl_r_is_the_first_query_and_escape_keeps_it() {
    pty_test(BIN, |ctx| {
        seed(
            ctx,
            &[
                row("git push origin main", "/x", 0),
                row("docker ps -a", "/x", 0),
                row("ls", "/x", 0),
            ],
        )?;
        let mut term = ctx.start()?;
        term.wait_for_history()?;
        term.write_bytes(b"dock")?;
        press_ctrl_r(&mut term)?;
        term.expect_plain("1 of 3", SHORT);
        first_drawing_done(&mut term);
        term.key(ESC)?;
        left_view(&mut term);
        // The line still holds `dock`: running it is a command that does not exist.
        term.write_bytes(b"\r")?;
        term.expect("dock: not found", SHORT);
        term.prompt();
        term.quit()
    });
}

/// Escape and Ctrl+C both leave the line as it was, and the editor is usable afterwards.
#[test]
fn escape_and_ctrl_c_keep_the_line() {
    pty_test(BIN, |ctx| {
        seed(ctx, &[row("echo old", "/x", 0)])?;
        let mut term = ctx.start()?;
        term.wait_for_history()?;
        term.write_bytes(b"echo keep-$((2+3))")?;
        for (round, key) in [ESC, CTRL_C].into_iter().enumerate() {
            open_view(&mut term)?;
            term.key(key)?;
            left_view(&mut term);
            assert_eq!(
                counts(&term),
                (round + 1, round + 1),
                "{}",
                term.transcript()
            );
        }
        term.write_bytes(b"\r")?;
        term.expect("keep-5", SHORT);
        term.prompt();
        term.quit()
    });
}

/// `Ctrl+R` inside the view moves through the scopes (all, this folder, failed): the command
/// that Enter picks is the best match of the scope.
#[test]
fn ctrl_r_inside_the_view_cycles_the_scope() {
    pty_test(BIN, |ctx| {
        let here = folder(ctx, "proj")?;
        seed(
            ctx,
            &[
                row("echo here-$((1+1))", &text(&here), 0),
                row("echo failed-$((3+3))", "/elsewhere", 1),
                row("echo else-$((2+2))", "/elsewhere", 0),
            ],
        )?;
        let mut term = ctx.launch().cwd(&here).start()?;
        term.wait_for_history()?;

        // All commands: three, and the newest is the best match.
        press_ctrl_r(&mut term)?;
        term.expect_plain("3 of 3", SHORT);
        first_drawing_done(&mut term);
        term.write_bytes(b"\r")?;
        left_view(&mut term);
        run_unstored(&mut term)?;
        term.expect("else-4", SHORT);
        term.prompt();

        // This folder: only the command that was run here.
        open_view(&mut term)?;
        term.key(CTRL_R)?;
        term.write_bytes(b"\r")?;
        left_view(&mut term);
        run_unstored(&mut term)?;
        term.expect("here-2", SHORT);
        term.prompt();

        // Failed: two steps on, and only the command that failed.
        open_view(&mut term)?;
        term.key(CTRL_R)?;
        term.key(CTRL_R)?;
        term.write_bytes(b"\r")?;
        left_view(&mut term);
        run_unstored(&mut term)?;
        term.expect("failed-6", SHORT);
        term.prompt();

        // Three steps are back at all commands.
        open_view(&mut term)?;
        for _ in 0..3 {
            term.key(CTRL_R)?;
        }
        term.write_bytes(b"\r")?;
        left_view(&mut term);
        run_unstored(&mut term)?;
        term.expect("else-4", SHORT);
        term.prompt();
        term.quit()
    });
}

/// A command run several times is one row, and the row is the newest run.
#[test]
fn a_command_that_ran_many_times_is_one_row_with_its_newest_run() {
    pty_test(BIN, |ctx| {
        seed(
            ctx,
            &[
                row("echo again-$((1+1))", "/old-place", 0),
                row("echo other-$((9+9))", "/x", 0),
                row("echo again-$((1+1))", "/new-place", 3),
            ],
        )?;
        let mut term = ctx.start()?;
        term.wait_for_history()?;
        press_ctrl_r(&mut term)?;
        // Two commands, not three rows.
        term.expect_plain("2 of 2", SHORT);
        // The best match is the repeated command; its detail line is its newest run.
        term.expect_plain("in /new-place", SHORT);
        term.expect_plain("status 3", SHORT);
        term.expect_plain("2 runs", SHORT);
        first_drawing_done(&mut term);
        term.key(ESC)?;
        left_view(&mut term);
        term.quit()
    });
}

/// Keys that arrive in one piece are all handled, in order, before anything is drawn: the
/// query and Enter together.
#[test]
fn a_query_and_enter_sent_together_pick_the_match() {
    pty_test(BIN, |ctx| {
        seed(
            ctx,
            &[
                row("echo dk-ps-$((1+1))", "/x", 0),
                row("ls", "/x", 0),
                row("pwd", "/x", 0),
            ],
        )?;
        let mut term = ctx.start()?;
        term.wait_for_history()?;
        open_view(&mut term)?;
        term.write_bytes(b"dkps\r")?;
        left_view(&mut term);
        term.write_bytes(b"\r")?;
        term.expect("dk-ps-2", SHORT);
        term.prompt();
        term.quit()
    });
}

/// A paste (bracketed) goes into the query without its line end, and does not choose anything.
#[test]
fn a_paste_goes_into_the_query_without_its_line_end() {
    pty_test(BIN, |ctx| {
        // Without the query, the newest command (`git status`) would be the best match.
        seed(
            ctx,
            &[
                row("echo dock-$((1+1))", "/x", 0),
                row("git status", "/x", 0),
            ],
        )?;
        let mut term = ctx.start()?;
        term.wait_for_history()?;
        open_view(&mut term)?;
        term.write_bytes(b"\x1b[200~dock\n\x1b[201~")?;
        // The newline did not pick anything: the view is still open.
        assert!(!term.try_expect(LEAVE, Duration::from_millis(400)));
        assert_eq!(counts(&term), (1, 0));
        // The query is `dock`: Enter picks the one command that matches.
        term.write_bytes(b"\r")?;
        left_view(&mut term);
        term.write_bytes(b"\r")?;
        term.expect("dock-2", SHORT);
        term.prompt();
        term.quit()
    });
}

/// `Ctrl+R` also works at the `> ` prompt of an unfinished command.
#[test]
fn ctrl_r_works_while_a_command_is_unfinished() {
    pty_test(BIN, |ctx| {
        seed(ctx, &[row("echo old", "/x", 0)])?;
        let mut term = ctx.start()?;
        term.wait_for_history()?;
        term.line("if true; then")?;
        term.expect("> ", SHORT);
        open_view(&mut term)?;
        term.key(ESC)?;
        left_view(&mut term);
        term.line("echo inside-$((1+1))")?;
        term.line("fi")?;
        term.expect("inside-2", SHORT);
        term.prompt();
        term.quit()
    });
}

/// Choosing a command, or leaving, stores nothing: the history holds only what was run.
#[test]
fn the_view_writes_nothing_to_the_history() {
    pty_test(BIN, |ctx| {
        seed(
            ctx,
            &[
                row("echo one", "/x", 0),
                row("echo two", "/x", 0),
                row("echo three", "/x", 0),
            ],
        )?;
        let mut term = ctx.start()?;
        term.wait_for_history()?;
        open_view(&mut term)?;
        term.write_bytes(b"two\r")?;
        left_view(&mut term);
        term.write_bytes(b"\x15")?;
        open_view(&mut term)?;
        term.key(ESC)?;
        left_view(&mut term);
        term.write_bytes(b"echo count=$(history --all | wc -l)\r")?;
        // Three seeded commands and the line that counts; not a fifth entry for the choice.
        term.expect("count=4", SHORT);
        term.prompt();
        term.quit()
    });
}

/// The terminal is as it was after the view: the screen is given back once for each time it was
/// taken, and the editor works again.
#[test]
fn leaving_the_view_gives_the_screen_and_the_terminal_mode_back() {
    pty_test(BIN, |ctx| {
        seed(ctx, &[row("echo a", "/x", 0)])?;
        let mut term = ctx.start()?;
        term.wait_for_history()?;
        term.sync("before")?;
        let before = term.termios()?.local_flags;
        for round in 1..=3 {
            open_view(&mut term)?;
            term.key(ESC)?;
            left_view(&mut term);
            assert_eq!(counts(&term), (round, round));
        }
        term.line("echo after-$((1+1))")?;
        term.expect("after-2", SHORT);
        term.prompt();
        assert_eq!(term.termios()?.local_flags, before, "{}", term.transcript());
        term.quit()
    });
}

/// A `SIGHUP` while the view is open ends the shell (status 129), the screen is given back, and
/// the terminal is in line mode when the shell is gone.
#[test]
fn sighup_while_the_view_is_open_ends_the_shell_and_restores_the_terminal() {
    if hup_is_ignored() {
        skip(
            "sighup_while_the_view_is_open_ends_the_shell_and_restores_the_terminal",
            "SIGHUP is ignored here (started under nohup?)",
        );
        return;
    }
    pty_test(BIN, |ctx| {
        seed(ctx, &[row("echo a", "/x", 0)])?;
        let mut term = ctx.start()?;
        term.wait_for_history()?;
        open_view(&mut term)?;
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

/// Closing the terminal while the view is open ends the shell too.
#[test]
fn closing_the_terminal_while_the_view_is_open_ends_the_shell() {
    pty_test(BIN, |ctx| {
        seed(ctx, &[row("echo a", "/x", 0)])?;
        let mut term = ctx.start()?;
        term.wait_for_history()?;
        open_view(&mut term)?;
        term.hang_up_terminal()?;
        let status = term.wait_exit(SHORT);
        assert_eq!(status.code(), Some(129));
        Ok(())
    });
}

/// With `NO_COLOR` unset the view draws in the theme's colors; the escape codes it writes are
/// the plain 256-color forms of the 16 names, and nothing it writes can change the title or
/// move the terminal (a stored command with an escape code is shown harmless).
#[test]
fn a_stored_escape_code_is_not_written_to_the_terminal() {
    pty_test(BIN, |ctx| {
        // The command text holds a "clear screen" and a "set title" code.
        seed(ctx, &[row("echo \x1b[2J\x1b]0;pwned\x07 end", "/x", 0)])?;
        let mut term = ctx.start()?;
        term.wait_for_history()?;
        open_view(&mut term)?;
        term.key(ESC)?;
        left_view(&mut term);
        // Everything the shell wrote while the view was open.
        let transcript = term.transcript();
        let inside = &transcript[transcript.find(ENTER).unwrap()..transcript.rfind(LEAVE).unwrap()];
        assert!(!inside.contains("pwned\x07"), "{inside:?}");
        assert!(!inside.contains("\x1b]0;"), "{inside:?}");
        assert!(!inside.contains("\x1b[2J"), "{inside:?}");
        term.quit()
    });
}
