//! The command history (S13b): the database under the data directory, the Up arrow that reads
//! it, and the `history` builtin.
//!
//! A typed command line is echoed by the terminal, so a test never looks for the text it
//! typed. Commands print something their text does not hold (`echo a-$((1+1))` prints `a-2`),
//! and listings are counted with patterns that do not match themselves (`grep -c 'a-[b]c'`).

use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use omsh_tests::{Ctx, SHORT, Session, pty_test};

const BIN: &str = env!("CARGO_BIN_EXE_omsh-pty-shell");

/// Up arrow, as a terminal sends it.
const UP: &[u8] = b"\x1b[A";

fn database(ctx: &Ctx) -> PathBuf {
    ctx.home().join(".local/share/oh-my-shell/history.db")
}

/// Everything the database holds on disk: the file and its write-ahead log.
fn stored_bytes(ctx: &Ctx) -> Vec<u8> {
    let mut bytes = fs::read(database(ctx)).unwrap_or_default();
    let mut log = database(ctx).into_os_string();
    log.push("-wal");
    bytes.extend(fs::read(log).unwrap_or_default());
    bytes
}

fn holds(bytes: &[u8], text: &str) -> bool {
    bytes
        .windows(text.len())
        .any(|window| window == text.as_bytes())
}

/// Runs `command`, whose output is one number, and returns it. `tag` names the answer and
/// must be new in this terminal: the first answer with that name is the one that is read.
fn number(term: &mut Session, tag: &str, command: &str) -> io::Result<u32> {
    term.line(&format!("echo {tag}=$({command})="))?;
    let needle = format!("{tag}=");
    let end = std::time::Instant::now() + SHORT;
    loop {
        // The typed line holds `tag=$(`; the answer holds `tag=<digits>=`.
        let shown = term.transcript();
        let answer = shown.match_indices(&needle).find_map(|(at, _)| {
            let rest = &shown[at + needle.len()..];
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            (!digits.is_empty() && rest[digits.len()..].starts_with('='))
                .then(|| digits.parse::<u32>().ok())
                .flatten()
        });
        if let Some(answer) = answer {
            return Ok(answer);
        }
        assert!(
            std::time::Instant::now() < end,
            "no answer for {tag}; the terminal showed:\n{shown}"
        );
        term.pump(Duration::from_millis(50));
    }
}

/// How many lines of `history <options>` match the pattern.
fn count(term: &mut Session, tag: &str, options: &str, pattern: &str) -> io::Result<u32> {
    number(
        term,
        tag,
        &format!("history {options} | grep -c '{pattern}'"),
    )
}

/// Done when: history survives restarts.
#[test]
fn history_survives_a_restart() {
    pty_test(BIN, |ctx| {
        let mut first = ctx.start()?;
        first.line("echo early-$((1+1))")?;
        first.expect("early-2", SHORT);
        first.line("sh -c 'exit 3'")?;
        first.sync("first-done")?;
        first.quit()?;
        assert!(database(ctx).is_file());

        let mut second = ctx.start()?;
        // The newest entry that starts with what is typed.
        second.write_bytes(b"echo ear")?;
        second.write_bytes(UP)?;
        second.write_bytes(b"\r")?;
        second.expect("early-2", SHORT);
        // The status came along, and so did the shell's own `exit`.
        assert_eq!(count(&mut second, "failed", "--failed", "exit [3]")?, 1);
        assert_eq!(count(&mut second, "exits", "", "  [e]xit$")?, 1);
        second.quit()
    });
}

/// Done when: concurrent shells don't corrupt the DB. Two shells write at the same time; a
/// third finds every command of both.
#[test]
fn shells_that_run_at_the_same_time_share_the_history() {
    pty_test(BIN, |ctx| {
        const EACH: u32 = 25;
        let mut a = ctx.start()?;
        let mut b = ctx.start()?;
        for n in 0..EACH {
            a.line(&format!(": from-a-{n}"))?;
            b.line(&format!(": from-b-{n}"))?;
        }
        a.sync("a-done")?;
        b.sync("b-done")?;

        // Up in one shell does not show what the other ran meanwhile.
        a.write_bytes(UP)?;
        a.write_bytes(b"\r")?;
        a.expect("a-done-2", SHORT);
        a.sync("a-again")?;
        assert_eq!(a.transcript().matches("a-done-2").count(), 2);
        assert_eq!(a.transcript().matches("b-done-2").count(), 0);
        // The builtin shows every shell's commands.
        assert_eq!(count(&mut a, "others", "", "from-[b]-")?, EACH);

        let mut c = ctx.start()?;
        assert_eq!(count(&mut c, "both", "", "from-[ab]-")?, 2 * EACH);
        assert_eq!(count(&mut c, "of-a", "--all", "from-[a]-")?, EACH);
        // A shell that started later sees what the others ran before.
        c.write_bytes(b": from-b-2")?;
        c.write_bytes(UP)?;
        c.write_bytes(b"\r")?;
        c.sync("c-done")?;
        assert_eq!(count(&mut c, "rerun", "", "from-[b]-2[0-9]$")?, 6);
        a.quit()?;
        b.quit()?;
        c.quit()
    });
}

/// `history --here`, `--failed`, `--since`, `search`, `-n` and `delete`.
#[test]
fn the_builtin_filters_and_deletes() {
    pty_test(BIN, |ctx| {
        let sub = ctx.home().join("sub");
        fs::create_dir(&sub)?;
        let mut term = ctx.start()?;
        term.line(&format!("cd {}", sub.display()))?;
        term.line(": in-sub-one")?;
        term.line("sh -c 'exit 4' in-sub-two")?;
        term.line(&format!("cd {}", ctx.home().display()))?;
        term.line(": at-home")?;
        term.sync("ready")?;

        assert_eq!(count(&mut term, "all", "", "in-[s]ub")?, 2);
        assert_eq!(count(&mut term, "away", "--here", "in-[s]ub")?, 0);
        assert_eq!(count(&mut term, "home", "--here", "at-[h]ome")?, 1);
        assert_eq!(count(&mut term, "failed", "--failed", "in-[s]ub")?, 1);
        assert_eq!(count(&mut term, "recent", "--since 1h", "in-[s]ub")?, 2);
        // The search is for the text as it is; the command that searches holds it too.
        assert_eq!(count(&mut term, "found", "search in-sub-t", "exit [4]")?, 1);
        assert_eq!(count(&mut term, "other", "search in-sub-o", "exit [4]")?, 0);
        assert_eq!(
            count(&mut term, "middle", "search b-o", "  : in-[s]ub-one$")?,
            1
        );
        // The newest two: this command and the one before it.
        assert_eq!(count(&mut term, "two", "-n 2", ".")?, 2);

        // Two seconds later they are older than one second.
        term.line("sleep 2")?;
        assert_eq!(count(&mut term, "old", "--since 1s", "in-[s]ub")?, 0);
        assert_eq!(count(&mut term, "new", "--since 1s", "[s]leep 2")?, 0);
        assert_eq!(count(&mut term, "newer", "--since 1m", "[s]leep 2")?, 1);

        term.line(&format!("cd {}", sub.display()))?;
        assert_eq!(count(&mut term, "here", "--here", "in-[s]ub")?, 2);
        assert_eq!(count(&mut term, "both", "--here --failed", "in-[s]ub")?, 1);

        term.line("history delete $(history | grep 'in-[s]ub-one' | awk '{print $1}')")?;
        assert_eq!(count(&mut term, "gone", "", "  : in-[s]ub-one$")?, 0);
        assert_eq!(count(&mut term, "left", "", "in-[s]ub-two$")?, 1);
        term.line("history delete 99999; echo status-$?")?;
        term.expect("history: delete: 99999: no such entry", SHORT);
        term.expect("status-1", SHORT);
        term.line("history --wrong; echo status-$?")?;
        term.expect("history: --wrong: unknown option", SHORT);
        term.expect("usage: history", SHORT);
        term.expect("status-2", SHORT);
        term.quit()
    });
}

/// Plain `history` on a terminal shows the newest 100, oldest first; through a pipe it shows
/// everything. `-n` and `--all` mean the same in both.
#[test]
fn a_terminal_gets_the_newest_hundred_and_a_pipe_everything() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        for n in 1..=105 {
            term.line(&format!(": w{n}"))?;
        }
        term.sync("typed")?;

        // In a listing the command follows the time after two spaces; a typed line follows
        // the prompt after one.
        let listed = |shown: &str, n: u32| shown.contains(&format!("  : w{n}\r\n"));
        let from = term.transcript().len();
        term.line("history")?;
        term.sync("listed")?;
        let shown = term.transcript()[from..].to_owned();
        // 100 entries: `history` itself, the `echo typed`, and `: w105` back to `: w8`.
        assert!(listed(&shown, 105) && listed(&shown, 8), "{shown}");
        assert!(!listed(&shown, 7) && !listed(&shown, 1), "{shown}");
        assert!(
            shown.find("  : w8\r\n") < shown.find("  : w105\r\n"),
            "oldest first"
        );
        // Each entry starts with its number and the date and time.
        let line = shown
            .lines()
            .find(|line| line.ends_with("  : w50"))
            .unwrap();
        let shape: String = line
            .trim_start()
            .chars()
            .map(|c| if c.is_ascii_digit() { '9' } else { c })
            .collect();
        assert!(
            shape.trim_start_matches('9') == "  9999-99-99 99:99  : w99",
            "{line:?}"
        );
        // No colors: an entry is plain text from its number to its end.
        let entries: Vec<&str> = shown
            .lines()
            .filter(|line| line.contains("  : w"))
            .collect();
        assert_eq!(entries.len(), 98);
        assert!(entries.iter().all(|line| !line.contains('\x1b')), "{shown}");

        let from = term.transcript().len();
        term.line("history --all")?;
        term.sync("listed-all")?;
        assert!(listed(&term.transcript()[from..], 1));
        let from = term.transcript().len();
        term.line("history -n 4")?;
        term.sync("listed-four")?;
        let shown = term.transcript()[from..].to_owned();
        // `history -n 4`, the sync, `history --all`, and the sync before it.
        assert!(!listed(&shown, 105), "{shown}");

        assert_eq!(count(&mut term, "piped", "", "  : w[0-9]*$")?, 105);
        assert_eq!(count(&mut term, "limited", "-n 3", ".")?, 3);
        let listing = ctx.home().join("listing.txt");
        term.line(&format!("history > {}", listing.display()))?;
        term.sync("to-file")?;
        let file = fs::read_to_string(&listing).unwrap_or_default();
        assert!(
            file.lines().count() > 105,
            "a file gets everything:\n{file}"
        );
        term.quit()
    });
}

/// A line that looks like it holds a secret is never written, but can be recalled once,
/// right after it ran.
#[test]
fn a_secret_is_not_stored_but_can_be_recalled_once() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.line("echo kept-$((1+1))")?;
        term.expect("kept-2", SHORT);
        term.line("DEMO_TOKEN=tok-abc-123 sh -c 'echo ran-$((1+1))'")?;
        term.expect("ran-2", SHORT);
        // Once, to fix a typing mistake.
        term.write_bytes(UP)?;
        term.write_bytes(b"\r")?;
        term.sync("again")?;
        assert_eq!(term.transcript().matches("ran-2").count(), 2);
        // Now it is gone: Up gives the sync, then `kept`.
        term.write_bytes(UP)?;
        term.write_bytes(UP)?;
        term.write_bytes(b"\r")?;
        term.sync("after")?;
        assert_eq!(term.transcript().matches("kept-2").count(), 2);
        assert_eq!(term.transcript().matches("ran-2").count(), 2);
        assert_eq!(count(&mut term, "secrets", "", "tok-[a]bc")?, 0);
        term.quit()?;
        let stored = stored_bytes(ctx);
        assert!(
            holds(&stored, "echo kept-"),
            "the test reads the right file"
        );
        assert!(!holds(&stored, "tok-abc-123"));
        assert!(!holds(&stored, "DEMO_TOKEN"));
        Ok(())
    });
}

/// The `[history] ignore` patterns of the configuration are used, not only the default.
#[test]
fn the_ignore_patterns_come_from_the_configuration() {
    pty_test(BIN, |ctx| {
        let dir = ctx.home().join(".config/oh-my-shell");
        fs::create_dir_all(&dir)?;
        fs::write(
            dir.join("config.toml"),
            "[history]\nignore = [\"*private*\", \"fg\"]\n",
        )?;
        let mut term = ctx.start()?;
        term.line(": this-is-private-stuff")?;
        term.line(": this-is-public-stuff")?;
        // The default pattern was replaced, so a leading space is stored now.
        term.line(" : with-a-space")?;
        term.sync("typed")?;
        assert_eq!(count(&mut term, "hidden", "", "this-is-[p]rivate")?, 0);
        assert_eq!(count(&mut term, "shown", "", "this-is-[p]ublic")?, 1);
        assert_eq!(count(&mut term, "spaced", "", "with-a-[s]pace")?, 1);
        term.quit()
    });
}

/// `--no-config` writes nothing: no history file. Up and `history` still work in the session.
#[test]
fn no_config_keeps_the_history_in_memory() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.launch().arg("--no-config").start()?;
        term.line("echo once-$((1+1))")?;
        term.expect("once-2", SHORT);
        term.write_bytes(UP)?;
        term.write_bytes(b"\r")?;
        term.sync("again")?;
        assert_eq!(term.transcript().matches("once-2").count(), 2);
        assert_eq!(count(&mut term, "listed", "", "[o]nce-")?, 2);
        term.quit()?;
        assert!(!ctx.home().join(".local").exists(), "nothing was written");

        // And nothing is left for the next shell.
        let mut next = ctx.launch().arg("--no-config").start()?;
        assert_eq!(count(&mut next, "listed", "", "[o]nce-")?, 0);
        next.quit()
    });
}

/// A command of several lines is one entry.
#[test]
fn a_command_of_several_lines_is_one_entry() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.line("if true; then")?;
        term.expect("> ", SHORT);
        term.line("echo deep-$((1+1))")?;
        term.line("fi")?;
        term.expect("deep-2", SHORT);
        assert_eq!(count(&mut term, "heads", "", "if true; [t]hen$")?, 1);
        assert_eq!(count(&mut term, "tails", "", "^[f]i$")?, 1);
        term.quit()
    });
}

/// The plain reader (a dumb terminal) records too. It reads a command line by line, and the
/// unfinished beginnings are not entries.
#[test]
fn the_plain_reader_records_whole_commands() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.launch().dumb().start()?;
        term.line("if true; then")?;
        term.line("echo plain-$((1+1))")?;
        term.line("fi")?;
        term.expect("plain-2", SHORT);
        term.line("sh -c 'exit 5'")?;
        assert_eq!(count(&mut term, "heads", "", "if true; [t]hen$")?, 1);
        assert_eq!(count(&mut term, "bodies", "", "^echo [p]lain-")?, 1);
        assert_eq!(count(&mut term, "failed", "--failed", "exit [5]")?, 1);
        // A terminal, so plain `history` is the short listing; a line is `number  time  text`.
        term.line("history -n 1")?;
        term.expect("  history -n 1", SHORT);
        term.quit()?;

        // The editor of the next shell finds what the plain reader stored.
        let mut next = ctx.start()?;
        next.write_bytes(b"if")?;
        next.write_bytes(UP)?;
        next.write_bytes(b"\r")?;
        next.expect("plain-2", SHORT);
        next.quit()
    });
}

/// A database that cannot be used does not stop the shell: one line says so, and history is
/// kept in memory.
#[test]
fn a_history_file_that_cannot_be_used_is_reported_once() {
    pty_test(BIN, |ctx| {
        // A directory where the file should be.
        fs::create_dir_all(database(ctx))?;
        let mut term = ctx.launch().start_raw()?;
        term.expect("oh-my-shell: history is kept in memory only: ", SHORT);
        term.expect("history.db: not a regular file", SHORT);
        term.line("echo still-$((1+1))")?;
        term.expect("still-2", SHORT);
        term.write_bytes(UP)?;
        term.write_bytes(b"\r")?;
        term.sync("again")?;
        assert_eq!(term.transcript().matches("still-2").count(), 2);
        assert_eq!(
            term.transcript().matches("kept in memory only").count(),
            1,
            "{}",
            term.transcript()
        );
        term.quit()?;
        assert!(database(ctx).is_dir());
        Ok(())
    });
}

/// What `init.omsh` runs was not typed and is not history; `history` is a builtin in the
/// file as anywhere else.
#[test]
fn the_startup_file_is_not_history() {
    pty_test(BIN, |ctx| {
        let dir = ctx.home().join(".config/oh-my-shell");
        fs::create_dir_all(&dir)?;
        fs::write(dir.join("init.omsh"), ": from-the-startup-file\n")?;
        let mut term = ctx.start()?;
        term.sync("typed")?;
        assert_eq!(count(&mut term, "startup", "", "from-the-[s]tartup")?, 0);
        assert_eq!(count(&mut term, "typed", "", "echo [t]yped-")?, 1);
        term.line("type history")?;
        term.expect("history is a shell builtin", SHORT);
        term.quit()
    });
}
