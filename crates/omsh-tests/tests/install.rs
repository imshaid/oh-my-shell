//! `scripts/install.sh` run the way a person runs it (S29c): no `--base-url`, and with or
//! without `--version`, against a fake release server on this machine that answers like
//! github.com (`releases/latest` is a redirect to `releases/tag/<tag>`).
//!
//! v0.1.0 shipped an installer that failed in exactly this case: a helper overwrote the
//! variable that holds the release number with the glibc number, so the download address was
//! `.../download/v2 36/...`. The tests of S29a always passed `--base-url` and never saw it.
//!
//! Every test runs through [`Sandbox`] (the installer writes to `HOME/.local/bin`, inside it).

use std::path::PathBuf;
use std::process::Output;

use omsh_tests::fakerelease::{FakeRelease, Spoil, archive_name};
use omsh_tests::sandbox::Sandbox;
use omsh_tests::{Ctx, pty_test};

const BIN: &str = env!("CARGO_BIN_EXE_omsh-pty-shell");
const LATEST: &str = "GET /imshaid/oh-my-shell/releases/latest";

fn installer() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts/install.sh")
}

/// What `--version` of a release prints.
fn program(version: &str) -> Vec<u8> {
    format!("#!/bin/sh\necho 'oh-my-shell {version}'\n").into_bytes()
}

fn download(tag: &str, file: &str) -> String {
    format!("GET /imshaid/oh-my-shell/releases/download/{tag}/{file}")
}

/// Runs the installer against `server` with `args`. The server is named by the same hook
/// `omsh update` has.
fn install(sandbox: &Sandbox, server: &FakeRelease, args: &[&str]) -> std::io::Result<Output> {
    sandbox
        .installer(&installer())
        .args(args)
        .env("OMSH_RELEASE_SERVER", server.origin())
        .output()
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn installed(ctx: &Ctx) -> PathBuf {
    ctx.home().join(".local/bin/oh-my-shell")
}

#[test]
fn the_latest_release_is_found_without_any_option() {
    pty_test(BIN, |ctx| {
        let sandbox = Sandbox::new(ctx, BIN)?;
        let server = FakeRelease::start();
        server.publish("v0.1.0", &program("0.1.0"), Spoil::None);
        server.publish("v0.1.1", &program("0.1.1"), Spoil::None);
        let output = install(&sandbox, &server, &[])?;
        let shown = text(&output);
        assert!(output.status.success(), "{shown}");
        assert!(shown.contains("Looking for the latest release"), "{shown}");
        assert!(shown.contains("/releases/download/v0.1.1"), "{shown}");
        assert!(shown.contains("Checksum ok."), "{shown}");
        assert_eq!(std::fs::read(installed(ctx))?, program("0.1.1"));
        assert_eq!(
            server.requests(),
            vec![
                LATEST.to_owned(),
                download("v0.1.1", &archive_name()),
                download("v0.1.1", "sha256.sum"),
            ],
            "one redirect lookup (no API), then the two files"
        );
        Ok(())
    });
}

#[test]
fn a_chosen_version_is_installed_without_a_lookup() {
    pty_test(BIN, |ctx| {
        let sandbox = Sandbox::new(ctx, BIN)?;
        let server = FakeRelease::start();
        server.publish("v0.1.0", &program("0.1.0"), Spoil::None);
        server.publish("v0.1.1", &program("0.1.1"), Spoil::None);
        // Asked for 0.1.0 although 0.1.1 is the latest.
        let output = install(&sandbox, &server, &["--version", "0.1.0"])?;
        let shown = text(&output);
        assert!(output.status.success(), "{shown}");
        assert_eq!(std::fs::read(installed(ctx))?, program("0.1.0"));
        assert_eq!(
            server.requests(),
            vec![
                download("v0.1.0", &archive_name()),
                download("v0.1.0", "sha256.sum"),
            ],
            "no lookup of the latest release"
        );
        Ok(())
    });
}

#[test]
fn a_version_with_a_v_in_front_is_the_same_release() {
    pty_test(BIN, |ctx| {
        let sandbox = Sandbox::new(ctx, BIN)?;
        let server = FakeRelease::start();
        server.publish("v0.1.1", &program("0.1.1"), Spoil::None);
        let output = install(&sandbox, &server, &["--version", "v0.1.1"])?;
        assert!(output.status.success(), "{}", text(&output));
        assert_eq!(std::fs::read(installed(ctx))?, program("0.1.1"));
        Ok(())
    });
}

#[test]
fn a_repository_with_no_release_is_said_in_words() {
    pty_test(BIN, |ctx| {
        let sandbox = Sandbox::new(ctx, BIN)?;
        let server = FakeRelease::start();
        server.publish_nothing();
        let output = install(&sandbox, &server, &[])?;
        let shown = text(&output);
        assert!(!output.status.success(), "{shown}");
        assert!(shown.contains("cannot read the latest release"), "{shown}");
        assert!(!installed(ctx).exists());
        assert_eq!(server.requests(), vec![LATEST.to_owned()]);
        Ok(())
    });
}

#[test]
fn a_changed_archive_is_still_refused_on_this_path() {
    pty_test(BIN, |ctx| {
        let sandbox = Sandbox::new(ctx, BIN)?;
        let server = FakeRelease::start();
        server.publish("v0.1.1", &program("0.1.1"), Spoil::WrongChecksum);
        let output = install(&sandbox, &server, &[])?;
        let shown = text(&output);
        assert!(!output.status.success(), "{shown}");
        assert!(shown.contains("checksum mismatch"), "{shown}");
        assert!(!installed(ctx).exists());
        Ok(())
    });
}

#[test]
fn the_test_hook_never_replaces_github_with_a_remote_server() {
    pty_test(BIN, |ctx| {
        let sandbox = Sandbox::new(ctx, BIN)?;
        for value in [
            "http://example.com",
            "https://github.com.evil.example",
            "http://127.0.0.2:80",
            "http://localhost.evil.example",
            "http://127.0.0.1@evil.example",
            "http://127.0.0.1:80/path",
            "http://127.0.0.1:80@evil.example",
            "http://127.0.0.1:80.evil.example",
            "http://localhost:80?x=1",
            "ftp://127.0.0.1",
            "localhost",
        ] {
            let output = sandbox
                .installer(&installer())
                .env("OMSH_RELEASE_SERVER", value)
                .output()?;
            let shown = text(&output);
            assert!(!output.status.success(), "{value}: {shown}");
            assert!(
                shown.contains("not a server on this computer"),
                "{value}: {shown}"
            );
            assert!(!installed(ctx).exists(), "{value}");
        }
        Ok(())
    });
}
