//! `omsh ai setup`, `status` and `test` through the real shell on a terminal (S20).
//!
//! The keyring is never touched: every launch gets a D-Bus address that leads nowhere, so keys
//! go to the credentials file of the test's own home directory. The "provider" is a fake server
//! on this machine (`omsh_tests::fakehttp`).
//!
//! A typed command line is echoed by the terminal, so a test never looks for the text it typed.
//! Checks for a secret use patterns that do not match themselves (`sk-tes[t]-`).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;

use nix::sys::termios::LocalFlags;
use omsh_tests::fakehttp::Server;
use omsh_tests::{Ctx, Launch, SHORT, pty_test};

const BIN: &str = env!("CARGO_BIN_EXE_omsh-pty-shell");

/// A made-up key. The text before the marker is what `grep` patterns below look for.
const KEY: &str = "sk-test-5f3c9a1e77b24d0c";

fn config_dir(ctx: &Ctx) -> PathBuf {
    ctx.home().join(".config/oh-my-shell")
}

/// A launch whose keyring does not exist.
fn launch(ctx: &Ctx) -> Launch {
    ctx.launch().env(
        "DBUS_SESSION_BUS_ADDRESS",
        "unix:path=/nonexistent/omsh-test-bus",
    )
}

fn contains_bytes(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

/// Done-when of S20: setup stores a key and uses it at once, in the same session.
#[test]
fn setup_stores_the_key_checks_it_and_the_same_session_uses_it() {
    pty_test(BIN, |ctx| {
        let server = Server::start(Duration::ZERO)?;
        let mut term = launch(ctx).start()?;

        term.command("omsh ai setup")?;
        term.expect("Choose a provider", SHORT);
        term.expect("Number (1-5)", SHORT);
        term.line("5")?;
        term.expect("Server address", SHORT);
        term.line(&server.url("/v1"))?;
        term.expect("API key", SHORT);
        // While the key is asked, the terminal does not echo.
        assert!(
            !term.termios()?.local_flags.contains(LocalFlags::ECHO),
            "echo must be off at the key prompt"
        );
        term.line(KEY)?;
        term.expect("model-one", SHORT);
        term.expect("Model (number or name)", SHORT);
        term.line("1")?;
        term.expect(
            "AI is set up: OpenAI-compatible server, model model-one",
            SHORT,
        );
        term.prompt();
        // And echo is on again afterwards: `stty` prints `-echo` when it is off.
        term.command("echo tty-$(stty -a | grep -c ' -echo ')-end")?;
        term.expect("tty-0-end", SHORT);

        // The key went to the credentials file: directory 0700, file 0600.
        let credentials = config_dir(ctx).join("credentials");
        assert_eq!(
            fs::metadata(config_dir(ctx))?.permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&credentials)?.permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::read_to_string(&credentials)?,
            format!("openai-compatible={KEY}\n")
        );
        // The settings went to config.toml, with its comments, and the key did not.
        let config = fs::read_to_string(config_dir(ctx).join("config.toml"))?;
        assert!(
            config.contains("# The AI layer is off until you set up a provider."),
            "{config}"
        );
        assert!(
            config.contains("[ai]\nprovider = \"openai-compatible\"\n"),
            "{config}"
        );
        assert!(config.contains("model = \"model-one\"\n"), "{config}");
        assert!(
            config.contains(&format!("base_url = \"{}\"", server.url("/v1"))),
            "{config}"
        );
        assert!(!config.contains(KEY), "{config}");
        // Nothing on the screen shows the key.
        assert!(!term.transcript().contains(KEY), "{}", term.transcript());

        // The same session uses it: no restart, no `source`.
        term.command("omsh ai test")?;
        term.expect("ok: OpenAI-compatible server answered in ", SHORT);
        term.prompt();

        // Every request carried the key, to the address that was chosen.
        let seen = server.requests();
        let paths: Vec<_> = seen.iter().map(|r| r.path().to_owned()).collect();
        assert_eq!(
            paths,
            ["/v1/models", "/v1/chat/completions", "/v1/chat/completions"],
            "the list of models, the check, the test"
        );
        for request in &seen {
            assert_eq!(
                request.header("authorization"),
                Some(format!("Bearer {KEY}").as_str()),
                "{}",
                request.line
            );
        }
        assert!(
            seen[1].body.contains("\"model\":\"model-one\""),
            "{}",
            seen[1].body
        );

        // The key is not in the environment of the shell or of a program it starts.
        term.command("echo env-count-$(env | grep -c 'sk-tes[t]-')-end")?;
        term.expect("env-count-0-end", SHORT);
        let environ = fs::read(format!("/proc/{}/environ", term.shell_pid()))?;
        assert!(
            !contains_bytes(&environ, "sk-test-"),
            "the shell's own environment"
        );

        // And not in the history, which sees the commands typed and nothing more.
        term.quit()?;
        let data = ctx.home().join(".local/share/oh-my-shell");
        for entry in fs::read_dir(&data)? {
            let path = entry?.path();
            assert!(
                !contains_bytes(&fs::read(&path)?, KEY),
                "{} holds the key",
                path.display()
            );
        }
        Ok(())
    });
}

/// Ctrl+C at the hidden prompt: nothing is saved, and the terminal is left as it was found.
#[test]
fn ctrl_c_at_the_key_prompt_saves_nothing_and_gives_the_echo_back() {
    pty_test(BIN, |ctx| {
        let server = Server::start(Duration::ZERO)?;
        let mut term = launch(ctx).start()?;
        let before = fs::read_to_string(config_dir(ctx).join("config.toml"))?;

        term.command("omsh ai setup")?;
        term.expect("Number (1-5)", SHORT);
        term.line("5")?;
        term.expect("Server address", SHORT);
        term.line(&server.url("/v1"))?;
        term.expect("API key", SHORT);
        assert!(!term.termios()?.local_flags.contains(LocalFlags::ECHO));
        term.write_bytes(b"sk-half-typed")?;
        term.key(3)?;
        term.expect("cancelled; nothing was saved", SHORT);
        term.prompt();
        term.command("echo status-$?")?;
        term.expect("status-130", SHORT);

        // A program that runs now finds the terminal as it should be.
        term.line("sleep 30")?;
        let mut settings = term.termios()?;
        for _ in 0..50 {
            if settings.local_flags.contains(LocalFlags::ECHO) {
                break;
            }
            term.pump(Duration::from_millis(20));
            settings = term.termios()?;
        }
        for pid in term.in_session("sleep") {
            nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGKILL)?;
        }
        assert!(
            settings
                .local_flags
                .contains(LocalFlags::ECHO | LocalFlags::ICANON),
            "echo and line mode are back after Ctrl+C: {settings:?}"
        );
        term.sync("after-sleep")?;

        assert_eq!(
            fs::read_to_string(config_dir(ctx).join("config.toml"))?,
            before
        );
        assert!(!config_dir(ctx).join("credentials").exists());
        assert!(server.requests().is_empty(), "nothing was sent anywhere");
        assert!(
            !term.transcript().contains("half-typed"),
            "the key is not echoed"
        );
        Ok(())
    });
}

#[test]
fn a_plain_http_address_elsewhere_stops_before_the_key_is_asked() {
    pty_test(BIN, |ctx| {
        let mut term = launch(ctx).start()?;
        let before = fs::read_to_string(config_dir(ctx).join("config.toml"))?;
        term.command("omsh ai setup")?;
        term.expect("Number (1-5)", SHORT);
        term.line("5")?;
        term.expect("Server address", SHORT);
        term.line("http://example.com/v1")?;
        term.expect("is reached over plain http and is not this computer", SHORT);
        term.expect("Continue without a key? [y/N]", SHORT);
        term.line("n")?;
        term.expect("stopped; nothing was saved", SHORT);
        term.prompt();
        assert!(
            !term.transcript().contains("what you type is hidden"),
            "the key was never asked: {}",
            term.transcript()
        );
        assert!(
            !term.transcript().contains("Checking with"),
            "no request was made: {}",
            term.transcript()
        );
        assert_eq!(
            fs::read_to_string(config_dir(ctx).join("config.toml"))?,
            before
        );
        assert!(!config_dir(ctx).join("credentials").exists());
        Ok(())
    });
}

#[test]
fn no_ai_switches_every_command_but_status_off() {
    pty_test(BIN, |ctx| {
        let mut term = launch(ctx).arg("--no-ai").start()?;
        term.command("omsh ai status")?;
        term.expect("AI: off for this session (--no-ai)", SHORT);
        term.prompt();
        for command in ["setup", "test", "forget"] {
            term.command(&format!("omsh ai {command}"))?;
            term.expect("AI is off for this session (--no-ai)", SHORT);
            term.prompt();
        }
        assert!(!config_dir(ctx).join("credentials").exists());
        Ok(())
    });
}

#[test]
fn no_config_refuses_setup_because_it_would_write_a_file() {
    pty_test(BIN, |ctx| {
        let mut term = launch(ctx).arg("--no-config").start()?;
        term.command("omsh ai setup")?;
        term.expect("this shell does not use one", SHORT);
        term.prompt();
        assert!(!config_dir(ctx).exists(), "--no-config writes nothing");
        Ok(())
    });
}

#[test]
fn status_in_a_fresh_shell_says_what_to_do() {
    pty_test(BIN, |ctx| {
        let mut term = launch(ctx).start()?;
        term.command("omsh ai status")?;
        term.expect("AI: not set up yet (run: omsh ai setup)", SHORT);
        term.prompt();
        term.command("omsh")?;
        term.expect("usage: omsh <command>", SHORT);
        term.prompt();
        term.command("type omsh")?;
        term.expect("omsh is a shell builtin", SHORT);
        Ok(())
    });
}

#[test]
fn the_wrong_answer_to_a_question_is_met_and_asked_again() {
    pty_test(BIN, |ctx| {
        let server = Server::start(Duration::ZERO)?;
        let mut term = launch(ctx).start()?;
        term.command("omsh ai setup")?;
        term.expect("Number (1-5)", SHORT);
        term.line("banana")?;
        term.expect("That is not one of the choices.", SHORT);
        term.expect("Number (1-5)", SHORT);
        term.line("5")?;
        term.expect("Server address", SHORT);
        term.line("not an address")?;
        term.expect("That address must start with http:// or https://", SHORT);
        term.line(&server.url("/v1"))?;
        term.expect("API key", SHORT);
        term.line("")?;
        term.expect("Model (number or name)", SHORT);
        term.line("model-two")?;
        term.expect(
            "AI is set up: OpenAI-compatible server, model model-two",
            SHORT,
        );
        term.prompt();
        // No key was given for this server, and the request carried the placeholder.
        assert!(!config_dir(ctx).join("credentials").exists());
        let seen = server.requests();
        assert!(!seen.is_empty());
        assert!(
            seen.iter()
                .all(|r| r.header("authorization") == Some("Bearer none"))
        );
        Ok(())
    });
}
