//! `omsh update` and the daily check (S29b), end to end: a copy of the shell installed in a
//! sandbox, started as `omsh`, talking to a fake release server on this machine.
//!
//! Every test runs through [`Sandbox`], which checks that nothing can touch the real home
//! directory (see `src/sandbox.rs`).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use omsh_tests::fakerelease::{FakeRelease, Spoil};
use omsh_tests::sandbox::Sandbox;
use omsh_tests::{Ctx, SHORT, pty_test};

const BIN: &str = env!("CARGO_BIN_EXE_omsh-pty-shell");

/// What `--version` of a release prints.
fn program(version: &str) -> Vec<u8> {
    format!("#!/bin/sh\necho 'oh-my-shell {version}'\n").into_bytes()
}

fn state_dir(ctx: &Ctx) -> std::path::PathBuf {
    ctx.home().join(".local/state/oh-my-shell")
}

fn leftovers(dir: &Path) -> std::io::Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in fs::read_dir(dir)? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if name.starts_with(".oh-my-shell") {
            names.push(name);
        }
    }
    Ok(names)
}

/// Runs `omsh <words>` against `server`, to its end. Returns the status and what it showed.
fn omsh(
    sandbox: &Sandbox,
    server: &FakeRelease,
    words: &[&str],
) -> std::io::Result<(Option<i32>, String)> {
    let mut launch = sandbox.omsh()?.env("OMSH_RELEASE_SERVER", &server.origin());
    for word in words {
        launch = launch.arg(word);
    }
    let mut term = launch.start_raw()?;
    let status = term.wait_exit(SHORT).code();
    term.pump(Duration::from_millis(50));
    Ok((status, term.transcript().to_owned()))
}

#[test]
fn update_replaces_the_program_and_keeps_its_mode() {
    pty_test(BIN, |ctx| {
        let sandbox = Sandbox::new(ctx, BIN)?;
        let server = FakeRelease::start();
        server.publish("v9.9.9", &program("9.9.9"), Spoil::None);
        let (status, shown) = omsh(&sandbox, &server, &["update"])?;
        assert_eq!(status, Some(0), "{shown}");
        assert!(shown.contains("Checksum ok."), "{shown}");
        assert!(shown.contains("-> 9.9.9"), "{shown}");
        assert_eq!(fs::read(sandbox.program())?, program("9.9.9"));
        let mode = fs::metadata(sandbox.program())?.permissions().mode() & 0o777;
        assert_eq!(mode & 0o100, 0o100, "still executable: {mode:o}");
        assert!(leftovers(&ctx.home().join("bin"))?.is_empty());
        // The link still points at the program, which now answers.
        let out = std::process::Command::new(sandbox.program())
            .arg("--version")
            .output()?;
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "oh-my-shell 9.9.9"
        );
        let requests = server.requests();
        assert_eq!(requests.len(), 3, "{requests:?}");
        Ok(())
    });
}

#[test]
fn a_bad_checksum_changes_nothing() {
    pty_test(BIN, |ctx| {
        let sandbox = Sandbox::new(ctx, BIN)?;
        let before = fs::read(sandbox.program())?;
        let server = FakeRelease::start();
        server.publish("v9.9.9", &program("9.9.9"), Spoil::WrongChecksum);
        let (status, shown) = omsh(&sandbox, &server, &["update"])?;
        assert_eq!(status, Some(1), "{shown}");
        assert!(shown.contains("checksum mismatch"), "{shown}");
        assert!(shown.contains("nothing was changed"), "{shown}");
        assert!(
            fs::read(sandbox.program())? == before,
            "the program changed"
        );
        assert!(leftovers(&ctx.home().join("bin"))?.is_empty());
        Ok(())
    });
}

#[test]
fn a_release_that_does_not_list_the_archive_changes_nothing() {
    pty_test(BIN, |ctx| {
        let sandbox = Sandbox::new(ctx, BIN)?;
        let before = fs::read(sandbox.program())?;
        let server = FakeRelease::start();
        server.publish("v9.9.9", &program("9.9.9"), Spoil::NoChecksum);
        let (status, shown) = omsh(&sandbox, &server, &["update"])?;
        assert_eq!(status, Some(1), "{shown}");
        assert!(shown.contains("does not list"), "{shown}");
        assert!(fs::read(sandbox.program())? == before);
        Ok(())
    });
}

#[test]
fn a_new_program_that_does_not_run_changes_nothing() {
    pty_test(BIN, |ctx| {
        let sandbox = Sandbox::new(ctx, BIN)?;
        let before = fs::read(sandbox.program())?;
        let server = FakeRelease::start();
        server.publish("v9.9.9", b"#!/bin/sh\nexit 3\n", Spoil::None);
        let (status, shown) = omsh(&sandbox, &server, &["update"])?;
        assert_eq!(status, Some(1), "{shown}");
        assert!(fs::read(sandbox.program())? == before);
        assert!(leftovers(&ctx.home().join("bin"))?.is_empty());
        Ok(())
    });
}

#[test]
fn the_newest_release_downloads_nothing() {
    pty_test(BIN, |ctx| {
        let sandbox = Sandbox::new(ctx, BIN)?;
        let before = fs::read(sandbox.program())?;
        let server = FakeRelease::start();
        let current = env!("CARGO_PKG_VERSION");
        server.publish(&format!("v{current}"), &program(current), Spoil::None);
        let (status, shown) = omsh(&sandbox, &server, &["update"])?;
        assert_eq!(status, Some(0), "{shown}");
        assert!(shown.contains("is the newest release"), "{shown}");
        assert_eq!(server.requests().len(), 1, "{:?}", server.requests());
        assert!(fs::read(sandbox.program())? == before);
        Ok(())
    });
}

#[test]
fn check_only_looks() {
    pty_test(BIN, |ctx| {
        let sandbox = Sandbox::new(ctx, BIN)?;
        let before = fs::read(sandbox.program())?;
        let server = FakeRelease::start();
        server.publish("v9.9.9", &program("9.9.9"), Spoil::None);
        let (status, shown) = omsh(&sandbox, &server, &["update", "--check"])?;
        assert_eq!(status, Some(0), "{shown}");
        assert!(shown.contains("9.9.9 is available"), "{shown}");
        assert_eq!(server.requests().len(), 1);
        assert!(fs::read(sandbox.program())? == before);
        Ok(())
    });
}

#[test]
fn a_program_installed_by_cargo_is_refused_and_the_server_is_not_asked() {
    pty_test(BIN, |ctx| {
        let sandbox = Sandbox::new(ctx, BIN)?;
        // The same program, in the place `cargo install` would put it.
        let cargo_bin = ctx.home().join(".cargo/bin");
        fs::create_dir_all(&cargo_bin)?;
        let installed = cargo_bin.join("oh-my-shell");
        fs::hard_link(sandbox.program(), &installed)?;
        let before = fs::read(&installed)?;
        let server = FakeRelease::start();
        server.publish("v9.9.9", &program("9.9.9"), Spoil::None);
        let mut term = sandbox
            .launch_at(&installed)
            .env("OMSH_RELEASE_SERVER", &server.origin())
            .named(&cargo_bin, "omsh")?
            .arg("update")
            .start_raw()?;
        let status = term.wait_exit(SHORT).code();
        term.pump(Duration::from_millis(50));
        let shown = term.transcript().to_owned();
        assert_eq!(status, Some(1), "{shown}");
        assert!(shown.contains("installed by cargo"), "{shown}");
        assert!(server.requests().is_empty(), "{:?}", server.requests());
        assert!(fs::read(&installed)? == before);
        Ok(())
    });
}

#[test]
fn the_build_in_target_is_never_replaced() {
    pty_test(BIN, |ctx| {
        // The binary the tests run is in `target/`; the refusal is what keeps `omsh update` from
        // replacing it. Started directly under the name `omsh` (a link to it).
        let server = FakeRelease::start();
        server.publish("v9.9.9", &program("9.9.9"), Spoil::None);
        let dir = ctx.home().join("links");
        fs::create_dir_all(&dir)?;
        let sandbox_free = ctx
            .launch()
            .env("OMSH_RELEASE_SERVER", &server.origin())
            .named(&dir, "omsh")?
            .arg("update");
        let before = fs::read_link(dir.join("omsh"))?;
        let mut term = sandbox_free.start_raw()?;
        let status = term.wait_exit(SHORT).code();
        term.pump(Duration::from_millis(50));
        let shown = term.transcript().to_owned();
        assert_eq!(status, Some(1), "{shown}");
        assert!(shown.contains("build from source"), "{shown}");
        assert!(server.requests().is_empty());
        assert_eq!(fs::read_link(dir.join("omsh"))?, before);
        Ok(())
    });
}

#[test]
fn a_server_that_is_not_on_this_computer_is_refused() {
    pty_test(BIN, |ctx| {
        let sandbox = Sandbox::new(ctx, BIN)?;
        let before = fs::read(sandbox.program())?;
        let mut term = sandbox
            .omsh()?
            .env("OMSH_RELEASE_SERVER", "http://example.com")
            .arg("update")
            .start_raw()?;
        let status = term.wait_exit(SHORT).code();
        term.pump(Duration::from_millis(50));
        let shown = term.transcript().to_owned();
        assert_eq!(status, Some(1), "{shown}");
        assert!(shown.contains("not replaced"), "{shown}");
        assert!(fs::read(sandbox.program())? == before);
        Ok(())
    });
}

#[test]
fn a_server_that_is_down_is_a_clear_line() {
    pty_test(BIN, |ctx| {
        let sandbox = Sandbox::new(ctx, BIN)?;
        let origin = {
            let server = FakeRelease::start();
            server.origin()
        };
        let mut term = sandbox
            .omsh()?
            .env("OMSH_RELEASE_SERVER", &origin)
            .arg("update")
            .start_raw()?;
        let status = term.wait_exit(SHORT).code();
        term.pump(Duration::from_millis(50));
        let shown = term.transcript().to_owned();
        assert_eq!(status, Some(1), "{shown}");
        assert!(shown.contains("cannot reach the release server"), "{shown}");
        Ok(())
    });
}

// -- the daily check ----------------------------------------------------------------------

fn turn_check_on(ctx: &Ctx) -> std::io::Result<()> {
    let dir = ctx.home().join(".config/oh-my-shell");
    fs::create_dir_all(&dir)?;
    fs::write(dir.join("config.toml"), "[update]\ncheck = true\n")
}

fn wait_for_requests(server: &FakeRelease, n: usize) {
    let end = std::time::Instant::now() + SHORT;
    while server.requests().len() < n && std::time::Instant::now() < end {
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn the_daily_check_shows_a_toast_once_and_then_stays_quiet_for_a_day() {
    pty_test(BIN, |ctx| {
        let sandbox = Sandbox::new(ctx, BIN)?;
        turn_check_on(ctx)?;
        let server = FakeRelease::start();
        server.publish("v9.9.9", &program("9.9.9"), Spoil::None);
        {
            let mut shell = sandbox
                .launch()
                .env("OMSH_RELEASE_SERVER", &server.origin())
                .start()?;
            // A couple of seconds after the prompt.
            shell.expect("is out (you have", SHORT);
            shell.expect("run omsh update", SHORT);
            shell.quit()?;
        }
        assert_eq!(
            server.requests(),
            vec!["GET /imshaid/oh-my-shell/releases/latest".to_owned()],
            "one lookup, and no download"
        );
        let stamp = fs::read_to_string(state_dir(ctx).join("update-check"))?;
        assert!(stamp.contains("latest=9.9.9"), "{stamp}");
        // A second shell the same day: no lookup and no toast.
        let mut again = sandbox
            .launch()
            .env("OMSH_RELEASE_SERVER", &server.origin())
            .start()?;
        assert!(
            !again.try_expect("is out (you have", Duration::from_millis(3500)),
            "no second toast"
        );
        assert_eq!(server.requests().len(), 1, "{:?}", server.requests());
        again.quit()?;
        Ok(())
    });
}

#[test]
fn the_check_is_off_by_default_and_asks_nothing() {
    pty_test(BIN, |ctx| {
        let sandbox = Sandbox::new(ctx, BIN)?;
        let server = FakeRelease::start();
        server.publish("v9.9.9", &program("9.9.9"), Spoil::None);
        let mut shell = sandbox
            .launch()
            .env("OMSH_RELEASE_SERVER", &server.origin())
            .start()?;
        assert!(
            !shell.try_expect("is out (you have", Duration::from_millis(3500)),
            "no toast"
        );
        assert!(server.requests().is_empty(), "{:?}", server.requests());
        assert!(!state_dir(ctx).join("update-check").exists());
        shell.quit()?;
        Ok(())
    });
}

#[test]
fn the_check_with_no_config_asks_nothing_and_writes_nothing() {
    pty_test(BIN, |ctx| {
        let sandbox = Sandbox::new(ctx, BIN)?;
        turn_check_on(ctx)?;
        let server = FakeRelease::start();
        server.publish("v9.9.9", &program("9.9.9"), Spoil::None);
        let mut shell = sandbox
            .launch()
            .arg("--no-config")
            .env("OMSH_RELEASE_SERVER", &server.origin())
            .start()?;
        assert!(!shell.try_expect("is out (you have", Duration::from_millis(3500)));
        assert!(server.requests().is_empty());
        assert!(!state_dir(ctx).exists());
        shell.quit()?;
        Ok(())
    });
}

#[test]
fn a_failed_check_is_silent_and_not_repeated_at_the_next_start() {
    pty_test(BIN, |ctx| {
        let sandbox = Sandbox::new(ctx, BIN)?;
        turn_check_on(ctx)?;
        let server = FakeRelease::start(); // nothing published: every path is 404
        {
            let mut shell = sandbox
                .launch()
                .env("OMSH_RELEASE_SERVER", &server.origin())
                .start()?;
            wait_for_requests(&server, 1);
            assert!(!shell.try_expect("is out (you have", Duration::from_millis(500)));
            shell.quit()?;
        }
        let stamp = fs::read_to_string(state_dir(ctx).join("update-check"))?;
        assert!(stamp.contains("checked="), "{stamp}");
        let mut again = sandbox
            .launch()
            .env("OMSH_RELEASE_SERVER", &server.origin())
            .start()?;
        assert!(!again.try_expect("is out (you have", Duration::from_millis(3000)));
        assert_eq!(server.requests().len(), 1, "{:?}", server.requests());
        again.quit()?;
        Ok(())
    });
}

#[test]
fn update_typed_inside_the_shell_works_too() {
    pty_test(BIN, |ctx| {
        let sandbox = Sandbox::new(ctx, BIN)?;
        let server = FakeRelease::start();
        server.publish("v9.9.9", &program("9.9.9"), Spoil::None);
        let mut shell = sandbox
            .launch()
            .env("OMSH_RELEASE_SERVER", &server.origin())
            .start()?;
        shell.command("omsh update --check")?;
        shell.expect("9.9.9 is available", SHORT);
        shell.quit()?;
        Ok(())
    });
}
