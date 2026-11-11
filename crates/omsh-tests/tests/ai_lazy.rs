//! The AI layer starts only when asked (S20, blueprint §13.1 and §16).
//!
//! "Starts" is checked from outside, in `/proc`: a normal session has no network socket and no thread
//! of the AI machinery (tokio, the HTTP client, the keyring connection). Then, as a positive
//! control, `omsh ai test` against a slow local server shows a socket while the request is in
//! flight, so the probe is known to see what it looks for, and none again afterwards.

use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use omsh_tests::fakehttp::Server;
use omsh_tests::{Ctx, Launch, SHORT, Session, pty_test};

const BIN: &str = env!("CARGO_BIN_EXE_omsh-pty-shell");

/// A launch whose keyring does not exist, so the real one is never touched.
fn launch(ctx: &Ctx) -> Launch {
    ctx.launch().env(
        "DBUS_SESSION_BUS_ADDRESS",
        "unix:path=/nonexistent/omsh-test-bus",
    )
}

fn write_config(ctx: &Ctx, text: &str) -> io::Result<()> {
    let dir: PathBuf = ctx.home().join(".config/oh-my-shell");
    fs::create_dir_all(&dir)?;
    fs::write(dir.join("config.toml"), text)
}

/// The inodes of the local (AF_UNIX) sockets of the machine. The line editor keeps a pair of
/// them for waking itself up; they are not network sockets.
fn local_socket_inodes() -> Vec<String> {
    fs::read_to_string("/proc/net/unix")
        .unwrap_or_default()
        .lines()
        .skip(1)
        .filter_map(|line| line.split_whitespace().nth(6).map(str::to_owned))
        .collect()
}

/// The network sockets the process holds open, as `socket:[inode]`: every socket that is not
/// an AF_UNIX one.
fn sockets(pid: u32) -> Vec<String> {
    let Ok(entries) = fs::read_dir(format!("/proc/{pid}/fd")) else {
        return Vec::new();
    };
    let local = local_socket_inodes();
    entries
        .flatten()
        .filter_map(|entry| fs::read_link(entry.path()).ok())
        .map(|target| target.to_string_lossy().into_owned())
        .filter_map(|target| {
            let inode = target
                .strip_prefix("socket:[")?
                .strip_suffix(']')?
                .to_owned();
            (!local.contains(&inode)).then_some(target)
        })
        .collect()
}

/// The names of the process's threads.
fn thread_names(pid: u32) -> Vec<String> {
    let Ok(entries) = fs::read_dir(format!("/proc/{pid}/task")) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| fs::read_to_string(entry.path().join("comm")).ok())
        .map(|name| name.trim().to_owned())
        .collect()
}

/// Names of threads that only the AI machinery starts.
fn ai_threads(pid: u32) -> Vec<String> {
    thread_names(pid)
        .into_iter()
        .filter(|name| {
            let name = name.to_ascii_lowercase();
            ["tokio", "zbus", "async-io", "reqwest", "hyper"]
                .iter()
                .any(|marker| name.contains(marker))
        })
        .collect()
}

fn assert_nothing_started(term: &Session, when: &str) {
    let pid = term.shell_pid();
    assert_eq!(sockets(pid), Vec::<String>::new(), "a socket {when}");
    assert_eq!(ai_threads(pid), Vec::<String>::new(), "an AI thread {when}");
}

#[test]
fn a_normal_session_has_no_network_socket_and_no_ai_thread() {
    pty_test(BIN, |ctx| {
        let mut term = launch(ctx).start()?;
        assert_nothing_started(&term, "at the first prompt");

        for command in [
            "echo hello",
            "ls / | cat > /dev/null",
            "x=$(echo a); echo $x",
            "history | tail -1",
            "sleep 0.1 & wait",
            "omsh ai status",
        ] {
            term.command(command)?;
        }
        term.sync("done")?;
        assert_nothing_started(&term, "after ordinary commands and `omsh ai status`");
        Ok(())
    });
}

#[test]
fn no_ai_keeps_even_the_omsh_commands_from_starting_anything() {
    pty_test(BIN, |ctx| {
        let mut term = launch(ctx).arg("--no-ai").start()?;
        for command in [
            "omsh ai status",
            "omsh ai setup",
            "omsh ai test",
            "omsh ai forget",
        ] {
            term.command(command)?;
        }
        term.sync("done")?;
        assert_nothing_started(&term, "under --no-ai");
        Ok(())
    });
}

/// The positive control, and the request's own cleanup.
#[test]
fn a_request_shows_a_socket_and_leaves_none_behind() {
    pty_test(BIN, |ctx| {
        // The server holds its answer for a second and a half, so there is time to look, and
        // keeps the connection open afterwards, as the real providers do.
        let server = Server::start_with(Duration::from_millis(1500), true)?;
        write_config(
            ctx,
            &format!(
                "[ai]\nprovider = \"openai-compatible\"\nmodel = \"m\"\nbase_url = \"{}\"\n",
                server.url("/v1")
            ),
        )?;
        let mut term = launch(ctx).start()?;
        let pid = term.shell_pid();
        assert_nothing_started(&term, "before the request");

        term.command("omsh ai test")?;
        let end = Instant::now() + SHORT;
        let mut seen = Vec::new();
        while seen.is_empty() && Instant::now() < end {
            term.pump(Duration::from_millis(20));
            seen = sockets(pid);
        }
        assert!(
            !seen.is_empty(),
            "the probe never saw the socket of the request: {}",
            term.transcript()
        );
        term.expect("ok: OpenAI-compatible server answered in ", SHORT);
        term.prompt();

        // The request is over: its connection is closed, whatever the server does.
        let mut left = sockets(pid);
        for _ in 0..50 {
            if left.is_empty() {
                break;
            }
            term.pump(Duration::from_millis(20));
            left = sockets(pid);
        }
        assert_eq!(left, Vec::<String>::new(), "a socket outlived the request");
        // The runtime of the request lives on the shell's own thread and leaves no thread of
        // its own. The one thread that may remain is `async-io`: the keyring's client (zbus)
        // starts it when the key is first looked up. It was not there before the request.
        let after = ai_threads(pid);
        assert!(
            after.iter().all(|name| name == "async-io"),
            "unexpected threads after the request: {after:?}"
        );
        Ok(())
    });
}

/// A panel that waits for a key holds no connection, before and after a chat turn: each request
/// has its own provider, dropped when the request ends.
#[test]
fn a_plan_panel_holds_no_socket_while_it_waits() {
    pty_test(BIN, |ctx| {
        let answer = r#"{"summary":"Do the thing","steps":[{"title":"Say","command":"echo hi","explanation":"x","risk":"low","needs_root":false}],"language":"en"}"#;
        // Keep-alive on: the server leaves connections open, as the real providers do.
        let server = Server::start_answering(Duration::ZERO, true, &[answer])?;
        write_config(
            ctx,
            &format!(
                "[ai]\nprovider = \"openai-compatible\"\nmodel = \"m\"\nbase_url = \"{}\"\n",
                server.url("/v1")
            ),
        )?;
        let mut term = launch(ctx).start()?;
        let pid = term.shell_pid();
        let no_socket_soon = |term: &mut Session, when: &str| {
            let mut left = sockets(pid);
            for _ in 0..100 {
                if left.is_empty() {
                    break;
                }
                term.pump(Duration::from_millis(20));
                left = sockets(pid);
            }
            assert_eq!(left, Vec::<String>::new(), "a socket {when}");
        };

        term.command("? say hi")?;
        term.expect("[Enter] run", SHORT);
        no_socket_soon(&mut term, "while the first panel waits");
        term.write_bytes(b"c")?;
        term.expect_plain("chat> ", SHORT);
        no_socket_soon(&mut term, "at the chat line");
        term.line("again")?;
        term.expect_plain("after 1 chat turn", SHORT);
        no_socket_soon(&mut term, "while the panel after a chat turn waits");
        term.write_bytes(b"\x1b")?;
        term.expect("cancelled; nothing ran", SHORT);
        Ok(())
    });
}
