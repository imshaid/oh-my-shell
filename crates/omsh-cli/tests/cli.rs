//! Runs the real `oh-my-shell` binary and checks what a user would see.

use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

const BINARY: &str = env!("CARGO_BIN_EXE_oh-my-shell");

/// Runs the shell's own interpreter on piped input; without it, piped input goes to `/bin/sh`.
const OWN: &str = "--interpret-stdin";

fn oh_my_shell(args: &[&str]) -> io::Result<Output> {
    Command::new(BINARY).args(args).output()
}

#[test]
fn version_flag_prints_the_binary_name_and_version() -> io::Result<()> {
    let output = oh_my_shell(&["--version"])?;

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        concat!("oh-my-shell ", env!("CARGO_PKG_VERSION"), "\n"),
    );
    assert!(output.stderr.is_empty());
    Ok(())
}

#[test]
fn help_flag_prints_usage_and_options() -> io::Result<()> {
    let output = oh_my_shell(&["--help"])?;

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Usage: oh-my-shell"));
    assert!(stdout.contains("--help"));
    assert!(stdout.contains("--version"));
    assert!(output.stderr.is_empty());
    Ok(())
}

/// Runs the shell with `input` on standard input.
fn with_input(input: &[u8]) -> io::Result<Output> {
    let mut child = Command::new(BINARY)
        .arg(OWN)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(input)?;
    }
    child.wait_with_output()
}

#[test]
fn empty_input_ends_the_shell_with_status_zero() -> io::Result<()> {
    let output = with_input(b"")?;

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    Ok(())
}

#[test]
fn a_command_on_standard_input_runs_without_a_prompt() -> io::Result<()> {
    let output = with_input(b"echo hello\n")?;

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "hello\n");
    // No prompt is printed when input is not a terminal.
    assert!(output.stderr.is_empty());
    Ok(())
}

/// Regression test: `eprintln!` panics when stderr cannot be written, which made the
/// binary exit with 101 instead of with the status of the last command. The command here
/// fails, so the shell has a message to write to a stderr that rejects it.
#[test]
fn unwritable_stderr_does_not_cause_a_panic() -> io::Result<()> {
    // Every write to /dev/full fails with "no space left on device".
    let stderr = OpenOptions::new().write(true).open("/dev/full")?;

    let mut child = Command::new(BINARY)
        .arg(OWN)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(stderr)
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(b"nosuchcmd_omsh\n")?;
    }
    let status = child.wait()?;

    assert_eq!(status.code(), Some(127));
    Ok(())
}

#[test]
fn unknown_flag_is_rejected_with_a_usage_error() -> io::Result<()> {
    let output = oh_my_shell(&["--no-such-flag"])?;

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--no-such-flag"));
    assert!(stderr.contains("Usage: oh-my-shell"));
    Ok(())
}

// Invocation modes (S11, blueprint §8). `-c`, a script and piped input are run by `/bin/sh`.

/// A scratch directory, removed at the end of the test.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> io::Result<Self> {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("modes-{}-{n}", std::process::id()));
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

/// The shell with a small fixed environment, `home` as `HOME` and `input` on standard input.
fn run_in(home: &Path, args: &[OsString], input: &[u8]) -> io::Result<Output> {
    let mut child = Command::new(BINARY)
        .args(args)
        .current_dir(home)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input);
    }
    child.wait_with_output()
}

fn args(list: &[&str]) -> Vec<OsString> {
    list.iter().map(OsString::from).collect()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn dash_c_runs_the_command_with_a_name_and_arguments() -> io::Result<()> {
    let home = Scratch::new()?;
    let out = run_in(
        home.path(),
        &args(&["-c", "echo \"$0|$1|$2\"", "zero", "one", "two"]),
        b"",
    )?;

    assert_eq!(stdout(&out), "zero|one|two\n");
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stderr.is_empty());
    Ok(())
}

#[test]
fn dash_c_passes_the_exit_status_through() -> io::Result<()> {
    let home = Scratch::new()?;
    for (command, status) in [("exit 7", 7), ("nosuchcmd_omsh", 127), ("false", 1)] {
        let out = run_in(home.path(), &args(&["-c", command]), b"")?;
        assert_eq!(out.status.code(), Some(status), "{command}");
    }
    Ok(())
}

#[test]
fn dash_c_without_a_command_is_a_usage_error() -> io::Result<()> {
    let out = oh_my_shell(&["-c"])?;

    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("-c"));
    Ok(())
}

#[test]
fn a_hyphen_after_the_command_is_an_argument_of_sh() -> io::Result<()> {
    let home = Scratch::new()?;
    let out = run_in(home.path(), &args(&["-c", "echo $0 $1", "-x", "a"]), b"")?;

    assert_eq!(stdout(&out), "-x a\n");
    Ok(())
}

#[test]
fn a_script_runs_with_its_arguments() -> io::Result<()> {
    let home = Scratch::new()?;
    fs::write(home.path().join("s.sh"), "echo \"$#:$1:$2\"\nexit 4\n")?;
    let out = run_in(home.path(), &args(&["s.sh", "a", "b c"]), b"")?;

    assert_eq!(stdout(&out), "2:a:b c\n");
    assert_eq!(out.status.code(), Some(4));
    Ok(())
}

#[test]
fn piped_input_without_arguments_is_run_by_sh() -> io::Result<()> {
    let home = Scratch::new()?;
    let out = run_in(home.path(), &[], b"echo piped-$0\nexit 3\n")?;

    // `sh` is the name `/bin/sh` was started under: this shell handed the input over.
    assert_eq!(stdout(&out), "piped-sh\n");
    assert_eq!(out.status.code(), Some(3));
    assert!(out.stderr.is_empty());
    Ok(())
}

/// `-c` must replace the process and not start a second one: ssh, cron and editors look at the
/// process they started.
#[test]
fn dash_c_replaces_the_process_with_bin_sh() -> io::Result<()> {
    let home = Scratch::new()?;
    let child = Command::new(BINARY)
        .args(["-c", "echo $$; readlink /proc/$$/exe"])
        .current_dir(home.path())
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let pid = child.id();
    let out = child.wait_with_output()?;

    let text = stdout(&out);
    let mut lines = text.lines();
    assert_eq!(lines.next(), Some(pid.to_string().as_str()));
    let sh = fs::canonicalize("/bin/sh")?;
    assert_eq!(lines.next(), Some(sh.to_string_lossy().as_ref()));
    Ok(())
}

/// A nested shell must not change `SHLVL` when it only hands the work over.
#[test]
fn dash_c_leaves_shlvl_alone() -> io::Result<()> {
    let out = Command::new(BINARY)
        .args(["-c", "echo ${SHLVL-unset}"])
        .env_clear()
        .env("SHLVL", "5")
        .output()?;

    assert_eq!(stdout(&out), "5\n");
    Ok(())
}

#[test]
fn an_argument_that_is_not_utf8_reaches_the_command_unchanged() -> io::Result<()> {
    let home = Scratch::new()?;
    let odd = OsString::from_vec(vec![b'a', 0xff, b'b']);
    let out = run_in(
        home.path(),
        &[
            OsString::from("-c"),
            OsString::from("printf %s \"$1\""),
            OsString::from("zero"),
            odd,
        ],
        b"",
    )?;

    assert_eq!(out.stdout, [b'a', 0xff, b'b']);
    Ok(())
}

/// Done-when: `-c` reads no config and writes nothing. A home directory with a broken
/// `config.toml` and an `init.omsh` that would create a file must stay as it was, and stderr
/// must stay empty, in all three ways of handing work to `/bin/sh`.
#[test]
fn the_handed_over_modes_read_no_configuration_and_touch_nothing() -> io::Result<()> {
    let home = Scratch::new()?;
    let config = home.path().join(".config/oh-my-shell");
    fs::create_dir_all(&config)?;
    fs::write(config.join("config.toml"), "this is [not toml\n")?;
    fs::write(
        config.join("init.omsh"),
        format!(
            "touch {}/marker\nexport FROM_INIT=1\n",
            home.path().display()
        ),
    )?;
    fs::write(home.path().join("s.sh"), "echo ${FROM_INIT-none}\n")?;
    let before = listing(home.path())?;

    let by_c = run_in(home.path(), &args(&["-c", "echo ${FROM_INIT-none}"]), b"")?;
    let by_script = run_in(home.path(), &args(&["s.sh"]), b"")?;
    let by_pipe = run_in(home.path(), &[], b"echo ${FROM_INIT-none}\n")?;

    for (what, out) in [("-c", by_c), ("script", by_script), ("pipe", by_pipe)] {
        assert_eq!(stdout(&out), "none\n", "{what}");
        assert!(out.stderr.is_empty(), "{what}: {:?}", out.stderr);
        assert_eq!(out.status.code(), Some(0), "{what}");
    }
    assert!(!home.path().join("marker").exists());
    assert_eq!(listing(home.path())?, before);
    Ok(())
}

/// Every path below `dir`, sorted, with its size.
fn listing(dir: &Path) -> io::Result<Vec<(PathBuf, u64)>> {
    let mut found = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(next) = todo.pop() {
        for entry in fs::read_dir(&next)? {
            let entry = entry?;
            let meta = entry.metadata()?;
            if meta.is_dir() {
                todo.push(entry.path());
            }
            found.push((entry.path(), meta.len()));
        }
    }
    found.sort();
    Ok(found)
}

#[test]
fn a_login_shell_with_dash_c_reads_the_profile() -> io::Result<()> {
    let home = Scratch::new()?;
    fs::write(
        home.path().join(".profile"),
        "FROM_PROFILE=yes\nexport FROM_PROFILE\n",
    )?;

    let out = run_in(
        home.path(),
        &args(&["-lc", "echo ${FROM_PROFILE-missing}"]),
        b"",
    )?;
    assert_eq!(stdout(&out), "yes\n");

    // Without `-l` the profile is not read.
    let plain = run_in(
        home.path(),
        &args(&["-c", "echo ${FROM_PROFILE-missing}"]),
        b"",
    )?;
    assert_eq!(stdout(&plain), "missing\n");
    Ok(())
}

/// How `sshd`, `login` and `su -` mark a login shell: a program name that starts with `-`.
#[test]
fn a_program_name_that_starts_with_a_hyphen_is_a_login() -> io::Result<()> {
    let home = Scratch::new()?;
    fs::write(
        home.path().join(".profile"),
        "FROM_PROFILE=yes\nexport FROM_PROFILE\n",
    )?;
    let out = Command::new(BINARY)
        .arg0("-oh-my-shell")
        .arg("-c")
        .arg("echo ${FROM_PROFILE-missing}")
        .current_dir(home.path())
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", home.path())
        .output()?;

    assert_eq!(stdout(&out), "yes\n");
    Ok(())
}

#[test]
fn dash_i_runs_the_shells_own_loop_on_piped_input() -> io::Result<()> {
    let home = Scratch::new()?;
    let out = run_in(home.path(), &args(&["-i"]), b"echo own-$0\n")?;

    // Our interpreter: the fixed name, and the prompt on stderr.
    assert_eq!(stdout(&out), "own-oh-my-shell\n");
    assert!(String::from_utf8_lossy(&out.stderr).contains("$ "));
    Ok(())
}

// Configuration (S12). Only the shell's own loop reads `config.toml` and `init.omsh`: here
// that is `-i` on a pipe. The terminal side is tested in `omsh-tests/tests/config.rs`.

fn write_config(home: &Path, name: &str, text: &str) -> io::Result<PathBuf> {
    let dir = home.join(".config/oh-my-shell");
    fs::create_dir_all(&dir)?;
    let file = dir.join(name);
    fs::write(&file, text)?;
    Ok(file)
}

#[test]
fn dash_i_runs_the_startup_file_and_reports_a_broken_configuration() -> io::Result<()> {
    let home = Scratch::new()?;
    let config = write_config(home.path(), "config.toml", "[ui]\nmotion = \"fast\"\n")?;
    write_config(
        home.path(),
        "init.omsh",
        "echo from-init\nalias hi='echo aliased'\n",
    )?;
    let out = run_in(home.path(), &args(&["-i"]), b"hi\necho $?\n")?;

    assert_eq!(stdout(&out), "from-init\naliased\n0\n");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains(&format!(
            "oh-my-shell: {}:2:10: ui.motion must be",
            config.display()
        )),
        "{err}"
    );
    Ok(())
}

#[test]
fn dash_i_writes_the_first_run_file_without_a_word() -> io::Result<()> {
    let home = Scratch::new()?;
    let out = run_in(home.path(), &args(&["-i"]), b"true\n")?;

    let file = home.path().join(".config/oh-my-shell/config.toml");
    assert!(fs::read_to_string(file)?.starts_with("# Oh My Shell configuration"));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!err.contains("config"), "{err}");
    Ok(())
}

#[test]
fn the_modes_that_are_not_the_interactive_shell_read_no_configuration() -> io::Result<()> {
    let home = Scratch::new()?;
    let init = "echo from-init\n";
    let config = "[ui\n";
    write_config(home.path(), "init.omsh", init)?;
    write_config(home.path(), "config.toml", config)?;
    // The hidden switch is this shell's interpreter on a pipe, which is not interactive.
    let piped = run_in(home.path(), &args(&["--interpret-stdin"]), b"echo x\n")?;
    assert_eq!(stdout(&piped), "x\n");
    assert!(piped.stderr.is_empty());
    // `--no-config` turns the interactive shell's reading off.
    let off = run_in(home.path(), &args(&["-i", "--no-config"]), b"echo y\n")?;
    assert_eq!(stdout(&off), "y\n");
    assert!(!String::from_utf8_lossy(&off.stderr).contains("config"));
    // Neither touched the files.
    let dir = home.path().join(".config/oh-my-shell");
    assert_eq!(fs::read_to_string(dir.join("init.omsh"))?, init);
    assert_eq!(fs::read_to_string(dir.join("config.toml"))?, config);
    assert_eq!(fs::read_dir(dir)?.count(), 2);
    Ok(())
}

#[test]
fn without_any_configuration_the_modes_that_do_not_read_it_create_nothing() -> io::Result<()> {
    let home = Scratch::new()?;
    run_in(home.path(), &args(&["--interpret-stdin"]), b"true\n")?;
    run_in(home.path(), &args(&["-i", "--no-config"]), b"true\n")?;
    assert!(!home.path().join(".config").exists());
    assert!(!home.path().join(".local").exists());
    Ok(())
}

#[test]
fn no_ai_is_accepted_in_every_mode() -> io::Result<()> {
    let home = Scratch::new()?;
    let by_c = run_in(home.path(), &args(&["--no-ai", "-c", "echo c"]), b"")?;
    let by_pipe = run_in(home.path(), &args(&["--no-ai"]), b"echo p\n")?;
    let own = run_in(home.path(), &args(&["--no-ai", "-i"]), b"echo i\n")?;

    assert_eq!(stdout(&by_c), "c\n");
    assert_eq!(stdout(&by_pipe), "p\n");
    assert_eq!(stdout(&own), "i\n");
    Ok(())
}

#[test]
fn the_hidden_switch_is_not_in_the_help() -> io::Result<()> {
    let out = oh_my_shell(&["--help"])?;
    let text = stdout(&out);

    assert!(!text.contains("interpret-stdin"));
    assert!(text.contains("--login"));
    assert!(text.contains("--no-ai"));
    Ok(())
}

/// The color wrappers are for the interactive shell only. The shell's own interpreter on a pipe
/// (`--interpret-stdin`) is not interactive, so `ls` there is the program itself.
#[test]
fn the_color_wrappers_are_not_defined_outside_the_interactive_shell() -> io::Result<()> {
    let output = with_input(b"type ls; type grep\n")?;

    let shown = String::from_utf8_lossy(&output.stdout);
    assert!(!shown.contains("shell function"), "{shown}");
    assert!(shown.contains("ls is /"), "{shown}");
    assert!(shown.contains("grep is /"), "{shown}");
    Ok(())
}
