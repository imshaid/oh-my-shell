//! Runs the same input through `oh-my-shell` and through `/bin/sh` and compares what they do.
//!
//! The input goes in on standard input, which is how the shell reads for now. For each case
//! the standard output and the exit status must be identical. Standard error is compared only
//! as "empty or not", because the wording of the messages differs on purpose.
//!
//! Cases where this shell is meant to behave differently from `/bin/sh` are tested on their
//! own further down.

use std::fs::{self, File};
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

const OMSH: &str = env!("CARGO_BIN_EXE_oh-my-shell");
const TARGET_TMP: &str = env!("CARGO_TARGET_TMPDIR");

/// A scratch directory with a few files to run, removed at the end of the test.
struct Sandbox {
    dir: PathBuf,
}

impl Sandbox {
    fn new() -> io::Result<Self> {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = Path::new(TARGET_TMP).join(format!("conformance-{}-{n}", std::process::id()));
        fs::create_dir_all(dir.join("bin"))?;
        fs::create_dir_all(dir.join("subdir"))?;
        let sandbox = Self { dir };
        // A text file with no `#!` line: shells run it with `/bin/sh`.
        sandbox.file("script", "echo no-shebang-ran\n", 0o755)?;
        sandbox.file("hashbang", "#!/bin/sh\necho hashbang-ran\n", 0o755)?;
        // A file that is not executable, in the directory and on the PATH.
        sandbox.file("plain", "echo never\n", 0o644)?;
        sandbox.file("bin/plain-in-path", "echo never\n", 0o644)?;
        sandbox.file("bin/tool-in-path", "#!/bin/sh\necho tool-ran\n", 0o755)?;
        Ok(sandbox)
    }

    fn file(&self, name: &str, text: &str, mode: u32) -> io::Result<()> {
        let path = self.dir.join(name);
        fs::write(&path, text)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(mode))
    }

    /// Runs `program` in the sandbox with a fixed environment and `input` on standard input.
    fn run(&self, program: &str, input: &[u8]) -> io::Result<Output> {
        let path = format!("{}:/usr/bin:/bin", self.dir.join("bin").display());
        let mut child = Command::new("timeout")
            .arg("10")
            .arg(program)
            .current_dir(&self.dir)
            .env_clear()
            .env("PATH", path)
            .env("LANG", "C")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            // The program may exit before reading everything (`exit 3`).
            let _ = stdin.write_all(input);
        }
        child.wait_with_output()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Checks that both shells do the same with `input`.
fn assert_same(sandbox: &Sandbox, input: &str) -> io::Result<()> {
    let ours = sandbox.run(OMSH, input.as_bytes())?;
    let reference = sandbox.run("/bin/sh", input.as_bytes())?;
    let show = |out: &Output| {
        format!(
            "status {:?}, stdout {:?}, stderr {:?}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    };
    let context = format!(
        "input {input:?}\n ours: {}\n /bin/sh: {}",
        show(&ours),
        show(&reference)
    );
    assert_eq!(ours.stdout, reference.stdout, "stdout differs\n{context}");
    assert_eq!(
        ours.status.code(),
        reference.status.code(),
        "status differs\n{context}"
    );
    assert_eq!(
        ours.stderr.is_empty(),
        reference.stderr.is_empty(),
        "stderr emptiness differs\n{context}"
    );
    Ok(())
}

#[test]
fn simple_commands_behave_like_sh() -> io::Result<()> {
    let sandbox = Sandbox::new()?;
    for input in [
        "",
        "true\n",
        "false\n",
        "true\nfalse\n",
        "false\ntrue\n",
        "/bin/echo a 'b c' \"d e\" f\\ g\n",
        "printf '%s\\n' x y\n",
        "printf '%s\\n' 'a  b' \"c\td\"\n",
        "# only a comment\n\n   \n",
        "echo one; echo two\n",
        "echo one;echo two;\n",
        "echo no-newline-at-the-end",
        "echo a\\\nb\n",
        "echo 'two\nlines'\n",
        "echo \"two\nlines\"\n",
        "echo a # not this\n",
        "echo \"quoted # stays\"\n",
        "echo বাংলা\n",
        "echo 'it'\\''s'\n",
        "echo a\"b\"'c'\n",
        "echo ''\n",
        "printf '[%s]\\n' '' x\n",
        "FOO=bar /bin/sh -c 'echo \"$FOO\"'\n",
        "A=1 B=2 /bin/sh -c 'echo \"$A$B\"'\n",
        "A=1 A=2 /bin/sh -c 'echo \"$A\"'\n",
        "A='x y' /bin/sh -c 'echo \"$A\"'\n",
        "/bin/sh -c 'echo \"${FOO-unset}\"'\n",
    ] {
        assert_same(&sandbox, input)?;
    }
    Ok(())
}

#[test]
fn exit_statuses_behave_like_sh() -> io::Result<()> {
    let sandbox = Sandbox::new()?;
    for input in [
        "exit 3\n",
        "exit\n",
        "false\nexit\n",
        "exit 259\n",
        "exit 0\necho not-reached\n",
        "echo before; exit 4; echo after\n",
        "/bin/sh -c 'exit 7'\n",
        "/bin/sh -c 'exit 7'\nexit\n",
        "nosuchcmd_omsh\n",
        "nosuchcmd_omsh\ntrue\n",
        "true\nnosuchcmd_omsh\n",
        "./missing\n",
        "/nonexistent/dir/prog\n",
        "./subdir\n",
        "/tmp\n",
        "./plain\n",
        "./plain\ntrue\n",
    ] {
        assert_same(&sandbox, input)?;
    }
    Ok(())
}

#[test]
fn scripts_and_path_lookup_behave_like_sh() -> io::Result<()> {
    let sandbox = Sandbox::new()?;
    for input in [
        "./script\n",
        "./hashbang\n",
        "tool-in-path\n",
        "tool-in-path\ntool-in-path\n",
        "./script a b\n",
        "/bin/sh ./script\n",
    ] {
        assert_same(&sandbox, input)?;
    }
    Ok(())
}

#[test]
fn programs_killed_by_a_signal_behave_like_sh() -> io::Result<()> {
    let sandbox = Sandbox::new()?;
    for input in [
        "/bin/sh -c 'kill -TERM $$'\n",
        "/bin/sh -c 'kill -KILL $$'\n",
        "/bin/sh -c 'kill -SEGV $$'\n",
        "/bin/sh -c 'kill -HUP $$'\n",
        "/bin/sh -c 'kill -TERM $$'\ntrue\n",
        "/bin/sh -c 'kill -PIPE $$'\n",
    ] {
        assert_same(&sandbox, input)?;
    }
    Ok(())
}

#[test]
fn a_program_killed_by_a_signal_is_named_on_stderr() -> io::Result<()> {
    let sandbox = Sandbox::new()?;
    let out = sandbox.run(OMSH, b"/bin/sh -c 'kill -SEGV $$'\n")?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(139));
    assert!(
        stderr.contains("killed by SIGSEGV (segmentation fault)"),
        "{stderr}"
    );
    Ok(())
}

#[test]
fn a_missing_command_is_named_on_stderr() -> io::Result<()> {
    let sandbox = Sandbox::new()?;
    let out = sandbox.run(OMSH, b"nosuchcmd_omsh\n")?;
    assert_eq!(out.status.code(), Some(127));
    assert!(out.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "oh-my-shell: nosuchcmd_omsh: not found\n"
    );
    Ok(())
}

// From here on: behavior that is meant to differ from `/bin/sh`.

/// A file found through `PATH` that cannot be run gives 126, as POSIX and bash say and as
/// blueprint section 7.1 requires. dash prints "Permission denied" but exits with 127.
#[test]
fn a_non_executable_file_on_the_path_gives_126() -> io::Result<()> {
    let sandbox = Sandbox::new()?;
    let out = sandbox.run(OMSH, b"plain-in-path\n")?;
    assert_eq!(out.status.code(), Some(126));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "oh-my-shell: plain-in-path: permission denied\n"
    );
    Ok(())
}

/// `/bin/sh` ends a script at its first syntax error. An interactive shell reports it and
/// reads on, so one typing mistake does not end the session.
#[test]
fn a_syntax_error_is_reported_and_the_next_line_still_runs() -> io::Result<()> {
    let sandbox = Sandbox::new()?;
    let out = sandbox.run(OMSH, b"fi\necho after\n")?;
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
    assert_eq!(out.status.code(), Some(0));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("fi"),
        "the error names the word"
    );
    Ok(())
}

#[test]
fn a_syntax_error_on_the_last_line_gives_status_2() -> io::Result<()> {
    let sandbox = Sandbox::new()?;
    let out = sandbox.run(OMSH, b"echo before\nfi\n")?;
    assert_eq!(String::from_utf8_lossy(&out.stdout), "before\n");
    assert_eq!(out.status.code(), Some(2));
    Ok(())
}

#[test]
fn an_unfinished_line_at_the_end_of_input_is_an_error() -> io::Result<()> {
    let sandbox = Sandbox::new()?;
    let out = sandbox.run(OMSH, b"echo 'never closed\n")?;
    assert!(out.stdout.is_empty());
    assert_eq!(out.status.code(), Some(2));
    assert!(!out.stderr.is_empty());
    Ok(())
}

/// A line that needs a feature that does not exist yet is refused as a whole: the commands
/// before the unsupported one on the same line must not run either.
#[test]
fn a_line_with_something_not_implemented_runs_nothing() -> io::Result<()> {
    let sandbox = Sandbox::new()?;
    let out = sandbox.run(OMSH, b"touch marker; echo a | cat\n")?;
    assert!(
        !sandbox.dir.join("marker").exists(),
        "the first command ran"
    );
    assert!(out.stdout.is_empty());
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("pipelines cannot run yet"));
    Ok(())
}

#[test]
fn a_pattern_is_refused_and_not_passed_on_as_text() -> io::Result<()> {
    let sandbox = Sandbox::new()?;
    let out = sandbox.run(OMSH, b"echo *\n")?;
    assert!(out.stdout.is_empty());
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("pathname expansion cannot run yet"));
    Ok(())
}

/// A line that is not UTF-8 is skipped with a message, and the next one still runs.
#[test]
fn input_that_is_not_utf8_is_skipped() -> io::Result<()> {
    let sandbox = Sandbox::new()?;
    let out = sandbox.run(OMSH, b"echo \xff\necho after\n")?;
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
    assert!(String::from_utf8_lossy(&out.stderr).contains("not valid UTF-8"));
    Ok(())
}

/// The shell takes exactly one line at a time from its input, so a program that reads the
/// same input continues after that line. `head -n 1` on a file puts the position back to just
/// after the line it printed, which makes the result exact.
#[test]
fn a_program_reads_the_input_that_follows_its_line() -> io::Result<()> {
    let sandbox = Sandbox::new()?;
    let script = sandbox.dir.join("input.txt");
    fs::write(&script, "head -n 1\nsecond line\necho done\n")?;

    let out = Command::new("timeout")
        .arg("10")
        .arg(OMSH)
        .current_dir(&sandbox.dir)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(File::open(&script)?)
        .output()?;

    assert_eq!(String::from_utf8_lossy(&out.stdout), "second line\ndone\n");
    assert_eq!(out.status.code(), Some(0));
    Ok(())
}
