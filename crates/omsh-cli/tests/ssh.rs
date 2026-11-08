//! What `ssh` and `scp` do to a login shell, repeated without a real `sshd`.
//!
//! A real test needs a second user or `chsh` and root, which these tests must not do. So they
//! run what `sshd` runs for the user's shell (`do_child` in `session.c`):
//!
//! - `ssh host cmd`: the shell is executed as `shell -c cmd`, with the program name set to the
//!   shell's file name (`oh-my-shell`, no hyphen).
//! - `ssh host` with a terminal, or with input piped in: the program name is `-oh-my-shell`
//!   (a login shell), no arguments.
//! - `scp`: the client runs `ssh host 'scp -t dir'` (to upload) or `scp -f file` (to download),
//!   so the remote shell must run `scp` with its standard input and output untouched by
//!   anything: one stray byte from a profile breaks the protocol. Here `-S` replaces the `ssh`
//!   program by a small script that runs the command through the shell under test.
//!
//! The manual check with a real `sshd` is in `.private/PROGRESS.md` (S11).

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

const BINARY: &str = env!("CARGO_BIN_EXE_oh-my-shell");

/// A scratch "home directory" of the pretend remote user.
struct Home(PathBuf);

impl Home {
    fn new() -> io::Result<Self> {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir =
            Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("ssh-{}-{n}", std::process::id()));
        fs::create_dir_all(&dir)?;
        // A profile that prints something: it must not be read for a command, or the scp
        // protocol would break.
        fs::write(
            dir.join(".profile"),
            "echo PROFILE-NOISE\nFROM_PROFILE=yes\nexport FROM_PROFILE\n",
        )?;
        Ok(Self(dir))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The environment `sshd` gives the user's shell.
fn sshd_command(home: &Home, program_name: &str) -> Command {
    let mut command = Command::new(BINARY);
    command
        .arg0(program_name)
        .current_dir(home.path())
        .env_clear()
        .env("HOME", home.path())
        .env("USER", "remote")
        .env("LOGNAME", "remote")
        .env("SHELL", BINARY)
        .env("PATH", "/usr/bin:/bin")
        .env("SSH_CONNECTION", "192.0.2.1 50000 192.0.2.2 22");
    command
}

fn run_with_input(mut command: Command, input: &[u8]) -> io::Result<Output> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input);
    }
    child.wait_with_output()
}

#[test]
fn ssh_with_a_command_runs_it_and_nothing_else() -> io::Result<()> {
    let home = Home::new()?;
    let mut command = sshd_command(&home, "oh-my-shell");
    command.args(["-c", "echo hello; echo \"in:$FROM_PROFILE\"; cat; exit 5"]);
    let out = run_with_input(command, b"from-the-client\n")?;

    // No profile output, no prompt, nothing on stderr; the client's input reaches `cat`.
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "hello\nin:\nfrom-the-client\n"
    );
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
    assert_eq!(out.status.code(), Some(5));
    Ok(())
}

#[test]
fn ssh_without_a_command_and_without_a_terminal_is_a_login_that_reads_the_script() -> io::Result<()>
{
    let home = Home::new()?;
    let command = sshd_command(&home, "-oh-my-shell");
    let out = run_with_input(command, b"echo \"profile:$FROM_PROFILE\"\nexit 3\n")?;

    // The profile ran (it is a login shell) and then the script from the client.
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.ends_with("profile:yes\n"), "{text:?}");
    assert_eq!(out.status.code(), Some(3));
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
    Ok(())
}

/// Finds a program on `PATH`.
fn find(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

/// The `sftp-server` program of OpenSSH, if this machine has it.
fn sftp_server() -> Option<PathBuf> {
    [
        "/usr/lib/openssh/sftp-server",
        "/usr/lib/ssh/sftp-server",
        "/usr/libexec/openssh/sftp-server",
        "/usr/libexec/sftp-server",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|path| path.is_file())
}

/// Writes the program that `scp -S` runs in place of `ssh`: it runs the remote command (its
/// last argument) through the shell under test, as `sshd` would. For `-s host sftp` it starts
/// the SFTP server.
fn fake_ssh(dir: &Path) -> io::Result<PathBuf> {
    let path = dir.join("fake-ssh");
    fs::write(
        &path,
        r#"#!/bin/sh
subsystem=
for arg; do
    [ "$arg" = -s ] && subsystem=1
    last=$arg
done
if [ -n "$subsystem" ]; then
    exec "$OMSH" -c "exec $SFTP_SERVER"
fi
exec "$OMSH" -c "$last"
"#,
    )?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
    Ok(path)
}

/// Runs `scp` with `args` against the pretend remote user. `legacy` selects the old protocol
/// (`-O`), where the remote side runs `scp -t` or `scp -f` through the shell.
fn scp(home: &Home, legacy: bool, args: &[&str]) -> io::Result<Output> {
    let fake = fake_ssh(home.path())?;
    let mut command = Command::new("timeout");
    command.args(["30", "scp", "-q"]);
    if legacy {
        command.arg("-O");
    }
    command
        .arg("-S")
        .arg(&fake)
        .args(args)
        .current_dir(home.path())
        .env_clear()
        .env("HOME", home.path())
        .env("PATH", "/usr/bin:/bin")
        .env("OMSH", BINARY)
        .env("SFTP_SERVER", sftp_server().unwrap_or_default().as_os_str())
        .stdin(Stdio::null());
    command.output()
}

/// True if the tools this test needs exist; prints why when it does not.
fn have_scp() -> bool {
    let found = find("scp").is_some() && find("timeout").is_some();
    if !found {
        eprintln!("skipped: `scp` or `timeout` is not installed");
    }
    found
}

/// A tree with a nested directory, a name with a space, binary data and an executable.
fn make_tree(root: &Path) -> io::Result<()> {
    fs::create_dir_all(root.join("sub dir"))?;
    fs::write(root.join("plain.txt"), "plain\n")?;
    fs::write(
        root.join("sub dir/with space.bin"),
        (0..=255_u8).collect::<Vec<_>>(),
    )?;
    fs::write(root.join("run.sh"), "#!/bin/sh\necho hi\n")?;
    fs::set_permissions(root.join("run.sh"), fs::Permissions::from_mode(0o750))?;
    Ok(())
}

fn same_tree(a: &Path, b: &Path) -> io::Result<()> {
    for name in ["plain.txt", "sub dir/with space.bin", "run.sh"] {
        assert_eq!(fs::read(a.join(name))?, fs::read(b.join(name))?, "{name}");
        let mode = |p: &Path| fs::metadata(p).map(|m| m.permissions().mode() & 0o777);
        assert_eq!(mode(&a.join(name))?, mode(&b.join(name))?, "mode of {name}");
    }
    Ok(())
}

#[test]
fn scp_legacy_protocol_uploads_and_downloads_through_the_shell() -> io::Result<()> {
    if !have_scp() {
        return Ok(());
    }
    let home = Home::new()?;
    let source = home.path().join("source");
    make_tree(&source)?;

    // Upload a tree: the remote side is `oh-my-shell -c 'scp -r -t -- dest'`.
    let dest = home.path().join("uploaded");
    fs::create_dir_all(&dest)?;
    let up = scp(
        &home,
        true,
        &[
            "-r",
            source.to_str().unwrap_or_default(),
            &format!("localhost:{}", dest.display()),
        ],
    )?;
    assert!(
        up.status.success(),
        "{}",
        String::from_utf8_lossy(&up.stderr)
    );
    same_tree(&source, &dest.join("source"))?;

    // Download it again: `oh-my-shell -c 'scp -r -f -- path'`.
    let back = home.path().join("downloaded");
    fs::create_dir_all(&back)?;
    let down = scp(
        &home,
        true,
        &[
            "-r",
            &format!("localhost:{}", dest.join("source").display()),
            back.to_str().unwrap_or_default(),
        ],
    )?;
    assert!(
        down.status.success(),
        "{}",
        String::from_utf8_lossy(&down.stderr)
    );
    same_tree(&source, &back.join("source"))?;
    Ok(())
}

#[test]
fn scp_with_a_single_file_and_a_space_in_its_name() -> io::Result<()> {
    if !have_scp() {
        return Ok(());
    }
    let home = Home::new()?;
    let file = home.path().join("a file.txt");
    fs::write(&file, "content\n")?;
    let target = home.path().join("copy of file.txt");

    let out = scp(
        &home,
        true,
        &[
            file.to_str().unwrap_or_default(),
            // The legacy protocol gives the remote path to the remote shell as it is, so a
            // path with a space needs its own quotes (with any shell; this is how scp -O works).
            &format!("localhost:'{}'", target.display()),
        ],
    )?;

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(fs::read_to_string(target)?, "content\n");
    Ok(())
}

#[test]
fn scp_over_sftp_works_when_the_server_program_exists() -> io::Result<()> {
    if !have_scp() {
        return Ok(());
    }
    if sftp_server().is_none() {
        eprintln!("skipped: OpenSSH's `sftp-server` is not installed");
        return Ok(());
    }
    let home = Home::new()?;
    let file = home.path().join("via-sftp.txt");
    fs::write(&file, "sftp\n")?;
    let target = home.path().join("sftp-copy.txt");

    let out = scp(
        &home,
        false,
        &[
            file.to_str().unwrap_or_default(),
            &format!("localhost:{}", target.display()),
        ],
    )?;

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(fs::read_to_string(target)?, "sftp\n");
    Ok(())
}
