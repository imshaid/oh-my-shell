//! Gate C, pulled forward to S20 (blueprint §21): no network socket is created by a session
//! that does not use the AI.
//!
//! The shell runs under `strace -f`, which records every `socket` and `connect` call of the
//! shell and of everything it starts. After a session of ordinary work, nothing may have made
//! a socket other than `AF_UNIX` (name lookups talk to `nscd` that way), and nothing may have
//! connected to a D-Bus bus (that is the keyring's door). The same under `--no-ai`, where
//! `omsh ai` commands are also run.
//!
//! S25 completes Gate C (the full suite with the network disabled); this is its socket test.

use std::fs;
use std::io;
use std::path::Path;
use std::time::Duration;

use omsh_tests::skip::skip;
use omsh_tests::{Ctx, Launch, SHORT, Session, pty_test};

const BIN: &str = env!("CARGO_BIN_EXE_omsh-pty-shell");

fn have_strace() -> bool {
    std::process::Command::new("strace")
        .arg("-V")
        .output()
        .is_ok_and(|out| out.status.success())
}

/// The shell under `strace`, writing to `trace`. `extra` are arguments of the shell.
fn traced(ctx: &Ctx, trace: &Path, extra: &[&str]) -> io::Result<Session> {
    let mut launch = Launch::new("strace")
        .home(ctx.home())
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            "unix:path=/nonexistent/omsh-test-bus",
        )
        .cwd(ctx.home());
    for arg in [
        "-f",
        "-qq",
        "-e",
        "trace=network",
        "-o",
        &trace.to_string_lossy(),
        "--",
        BIN,
    ] {
        launch = launch.arg(arg);
    }
    for arg in extra {
        launch = launch.arg(arg);
    }
    launch.start()
}

/// What the trace shows that a session without AI must not do. Empty when all is well.
fn violations(trace: &str) -> Vec<String> {
    trace
        .lines()
        .filter(|line| {
            let made_a_socket = line.contains("socket(") && !line.contains("socket(AF_UNIX");
            let to_a_bus = line.contains("connect(")
                && (line.contains("dbus")
                    || line.contains("/bus\"")
                    || line.contains("bus_socket"));
            made_a_socket || to_a_bus
        })
        .map(str::to_owned)
        .collect()
}

#[test]
fn the_checker_itself_finds_what_it_is_meant_to_find() {
    let tcp = "123 socket(AF_INET, SOCK_STREAM|SOCK_CLOEXEC, IPPROTO_TCP) = 5";
    let tcp6 = "123 socket(AF_INET6, SOCK_STREAM, IPPROTO_IP) = 5";
    let netlink = "123 socket(AF_NETLINK, SOCK_RAW|SOCK_CLOEXEC, NETLINK_ROUTE) = 5";
    let bus = "123 connect(7, {sa_family=AF_UNIX, sun_path=\"/run/user/1000/bus\"}, 110) = 0";
    let nscd =
        "123 connect(7, {sa_family=AF_UNIX, sun_path=\"/var/run/nscd/socket\"}, 110) = -1 ENOENT";
    let unix = "123 socket(AF_UNIX, SOCK_STREAM|SOCK_CLOEXEC|SOCK_NONBLOCK, 0) = 7";
    for bad in [tcp, tcp6, netlink, bus] {
        assert_eq!(violations(bad), [bad.to_owned()], "{bad}");
    }
    for fine in [nscd, unix, ""] {
        assert!(violations(fine).is_empty(), "{fine}");
    }
}

fn session_of_ordinary_work(term: &mut Session, ctx: &Ctx) -> io::Result<()> {
    // A repository on disk, so that the prompt reads a branch name.
    fs::create_dir_all(ctx.home().join("repo/.git"))?;
    fs::write(ctx.home().join("repo/.git/HEAD"), "ref: refs/heads/main\n")?;
    for command in [
        "echo hello",
        "ls / | cat > /dev/null",
        "x=$(echo a); echo $x",
        "echo ~root",
        "cd repo",
        "pwd",
        "cd -",
        "history | tail -1",
        "sleep 0.1 & wait",
        "false",
        // The explain triggers of S24 with no AI set up: they say so and open nothing.
        "?? ls -l",
        "??",
        "ls / | ? what is here",
        "echo a?b | cat",
        "omsh ai status",
    ] {
        term.command(command)?;
    }
    // Alt+? with a failure to explain, and the AI not set up.
    term.command("false")?;
    term.prompt();
    term.write_bytes(b"\x1b?")?;
    term.expect("AI is ", SHORT);
    // Alt+? is not a line, so the harness cannot tell when the editor is back: give it a moment
    // before the next line is typed.
    term.pump(Duration::from_millis(500));
    term.sync("done")
}

#[test]
fn a_session_without_ai_creates_no_network_socket() {
    if !have_strace() {
        skip(
            "a_session_without_ai_creates_no_network_socket",
            "strace is not installed",
        );
        return;
    }
    pty_test(BIN, |ctx| {
        let trace = ctx.home().join("trace.txt");
        let mut term = traced(ctx, &trace, &[])?;
        session_of_ordinary_work(&mut term, ctx)?;
        term.quit()?;
        let text = fs::read_to_string(&trace)?;
        assert_eq!(violations(&text), Vec::<String>::new(), "{text}");
        Ok(())
    });
}

#[test]
fn a_session_with_no_ai_creates_no_network_socket_either() {
    if !have_strace() {
        skip(
            "a_session_with_no_ai_creates_no_network_socket_either",
            "strace is not installed",
        );
        return;
    }
    pty_test(BIN, |ctx| {
        let trace = ctx.home().join("trace.txt");
        let mut term = traced(ctx, &trace, &["--no-ai"])?;
        session_of_ordinary_work(&mut term, ctx)?;
        for command in ["omsh ai setup", "omsh ai test", "omsh ai forget"] {
            term.command(command)?;
        }
        term.sync("done-2")?;
        term.quit()?;
        let text = fs::read_to_string(&trace)?;
        assert_eq!(violations(&text), Vec::<String>::new(), "{text}");
        Ok(())
    });
}

/// The positive control. A trace that is empty proves nothing, so a program that the shell
/// starts makes a TCP connection and the trace must show it: `strace -f` follows the children.
#[test]
fn the_trace_sees_a_connection_made_by_a_program_the_shell_started() {
    if !have_strace() {
        skip(
            "the_trace_sees_a_connection_made_by_a_program_the_shell_started",
            "strace is not installed",
        );
        return;
    }
    if !std::path::Path::new("/bin/bash").exists()
        && !std::path::Path::new("/usr/bin/bash").exists()
    {
        skip(
            "the_trace_sees_a_connection_made_by_a_program_the_shell_started",
            "bash is not installed",
        );
        return;
    }
    pty_test(BIN, |ctx| {
        let trace = ctx.home().join("trace.txt");
        let mut term = traced(ctx, &trace, &[])?;
        // bash opens a TCP socket for /dev/tcp/...; nothing listens on port 9, and that is fine.
        term.command("bash -c 'echo x > /dev/tcp/127.0.0.1/9' 2>/dev/null; true")?;
        term.sync("tried")?;
        term.quit()?;
        let text = fs::read_to_string(&trace)?;
        let found = violations(&text);
        assert!(
            found.iter().any(|line| line.contains("AF_INET")),
            "the checker did not see the child's TCP socket:\n{text}"
        );
        Ok(())
    });
}
