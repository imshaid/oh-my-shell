//! The interactive side of the earlier segments (S05 to S07b), typed at a real terminal:
//! running programs, exit statuses and their messages, state that must last from one line to the
//! next, and input that comes from the keyboard.

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use omsh_tests::{SHORT, pty_test};

const BIN: &str = env!("CARGO_BIN_EXE_omsh-pty-shell");

/// A directory for one test, removed at the end.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> io::Result<Self> {
        let dir = std::env::temp_dir().join(format!("omsh-pty-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir)?;
        Ok(Self(dir))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn programs_run_and_their_status_is_kept() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.line("/bin/echo ext-$((1+1))")?;
        term.expect("ext-2", SHORT);
        term.line("sh -c 'exit 7'; echo status-$?")?;
        term.expect("status-7", SHORT);
        term.line("false; echo status-$?")?;
        term.expect("status-1", SHORT);
        term.quit()
    });
}

#[test]
fn a_missing_program_gives_127_and_a_message() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.line("no-such-program-omsh")?;
        term.expect("no-such-program-omsh: not found", SHORT);
        term.line("echo status-$?")?;
        term.expect("status-127", SHORT);
        // The same program in a pipeline, and the shell is still there.
        term.sync("alive")?;
        term.quit()
    });
}

#[test]
fn a_file_that_cannot_be_executed_gives_126() {
    let dir = Scratch::new("not-executable").expect("scratch directory");
    fs::write(dir.path().join("plain"), "echo never-runs\n").expect("write the file");
    fs::set_permissions(dir.path().join("plain"), fs::Permissions::from_mode(0o644))
        .expect("set the mode");
    let cwd = dir.path().to_owned();
    pty_test(BIN, move |ctx| {
        let mut term = ctx.launch().cwd(&cwd).start()?;
        term.line("./plain")?;
        term.expect("permission denied", SHORT);
        term.line("echo status-$?")?;
        term.expect("status-126", SHORT);
        assert!(!term.transcript().contains("never-runs"));
        term.quit()
    });
}

#[test]
fn a_program_killed_by_a_signal_is_reported_and_gives_128_plus_the_signal() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.line("sh -c 'kill -SEGV $$'")?;
        term.expect("killed by SIGSEGV", SHORT);
        term.line("echo status-$?")?;
        term.expect("status-139", SHORT);
        term.quit()
    });
}

#[test]
fn cd_export_and_variables_last_from_one_line_to_the_next() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.line("cd /usr")?;
        term.line("echo here-$(pwd)")?;
        term.expect("here-/usr", SHORT);
        // A program sees the change too: its working directory is the real one, not the
        // shell's idea of it.
        term.line("sh -c 'echo phys-$(pwd -P)'")?;
        term.expect("phys-/usr", SHORT);

        term.line("export FOO_OMSH=bar")?;
        term.line("BAZ_OMSH=1")?;
        term.line("sh -c 'echo exported-$FOO_OMSH'")?;
        term.expect("exported-bar", SHORT);
        // A variable that is not exported is the shell's own; the line ends right after "-".
        term.line("sh -c 'echo private-$BAZ_OMSH'")?;
        term.expect("private-\r\n", SHORT);
        term.line("echo shell-$BAZ_OMSH")?;
        term.expect("shell-1", SHORT);
        term.quit()
    });
}

#[test]
fn an_alias_lasts_from_one_line_to_the_next() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.line("alias hi='echo al-$((1+1))'")?;
        term.line("hi")?;
        term.expect("al-2", SHORT);
        term.line("unalias hi")?;
        term.line("hi")?;
        term.expect("hi: not found", SHORT);
        term.quit()
    });
}

#[test]
fn pipes_redirections_and_here_documents_work_at_the_prompt() {
    let dir = Scratch::new("redirect").expect("scratch directory");
    let cwd = dir.path().to_owned();
    pty_test(BIN, move |ctx| {
        let mut term = ctx.launch().cwd(&cwd).start()?;
        term.line("echo pipe-$((1+1)) | tr a-z A-Z")?;
        term.expect("PIPE-2", SHORT);

        term.line("echo red-$((1+1)) > out.txt")?;
        term.line("cat out.txt")?;
        term.expect("red-2", SHORT);
        assert_eq!(
            fs::read_to_string(cwd.join("out.txt"))?,
            "red-2\n",
            "{}",
            term.transcript()
        );

        // The unfinished command gets the continuation prompt.
        term.line("cat <<EOF")?;
        term.expect("> ", SHORT);
        term.line("here-$((1+1))")?;
        term.expect("> ", SHORT);
        term.line("EOF")?;
        term.expect("here-2", SHORT);
        term.quit()
    });
}

/// `read` takes its input from the keyboard, also when the shell itself reads its lines from the
/// same terminal.
#[test]
fn read_takes_a_line_from_the_keyboard() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        // Type `hello` only when `read` is waiting for it. Keys typed before that belong to the
        // line editor (it reads what is already queued), not to the command.
        term.line("printf 'ready>'; read word")?;
        term.expect("ready>", SHORT);
        term.line("hello")?;
        term.line("echo r-$word")?;
        term.expect("r-hello", SHORT);
        term.quit()
    });
}

#[test]
fn a_loop_over_the_keyboard_ends_at_end_of_input_and_the_shell_stays() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        // Two continuation prompts, then the loop runs and waits for the keyboard.
        term.line("while read l; do")?;
        term.expect("> ", SHORT);
        term.line("echo got-$l")?;
        term.expect("> ", SHORT);
        term.line("done")?;
        term.pump(Duration::from_millis(200));
        term.line("one")?;
        term.expect("got-one", SHORT);
        term.line("two")?;
        term.expect("got-two", SHORT);
        // Ctrl+D on an empty line is the end of input for `read`: the loop ends.
        term.key(4)?;
        term.sync("after")?;
        assert!(!term.has_exited()?);
        term.quit()
    });
}

#[test]
fn a_syntax_error_does_not_end_the_session() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.line("echo )")?;
        term.line("if then fi")?;
        term.sync("alive")?;
        assert!(!term.has_exited()?);
        term.quit()
    });
}

#[test]
fn the_interactive_flags_and_the_shell_variables_are_set() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.launch().env("SHLVL", "4").start()?;

        // `$-` has `i` (interactive) and `m` (job control). The first "fl-" is the typed line.
        term.line("echo fl-$-")?;
        term.expect("echo fl-$-", SHORT);
        term.expect("\r\n", SHORT);
        term.expect("\r\n", SHORT);
        let shown = term.recent(60);
        let flags = shown
            .rfind("fl-")
            .map(|at| {
                shown[at + 3..]
                    .split('\r')
                    .next()
                    .unwrap_or_default()
                    .to_owned()
            })
            .unwrap_or_default();
        assert!(
            flags.contains('i') && flags.contains('m'),
            "flags were {flags:?}\n{}",
            term.transcript()
        );

        term.line("echo lvl-$SHLVL")?;
        term.expect("lvl-5", SHORT);
        term.line("echo ver-$OMSH_VERSION")?;
        term.expect(concat!("ver-", env!("CARGO_PKG_VERSION")), SHORT);
        // Both are exported.
        term.line("sh -c 'echo env-$SHLVL-$OMSH_VERSION'")?;
        term.expect(concat!("env-5-", env!("CARGO_PKG_VERSION")), SHORT);
        term.quit()
    });
}
