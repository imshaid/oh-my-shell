//! The trash and `undo`, in a real terminal (S18).
//!
//! Everything happens inside the test's own home directory, which also holds the data
//! directory (`.local/share/oh-my-shell`): the trash and `undo.db` of one test are never
//! those of another, or the owner's.

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use omsh_tests::fakehttp::Server;
use omsh_tests::{Ctx, SHORT, Session, pty_test};

const BIN: &str = env!("CARGO_BIN_EXE_omsh-pty-shell");

const QUESTION: &str = "Run it? [y/N] (t = move to the trash instead)";

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).map_or(0, |data| data.permissions().mode() & 0o7777)
}

fn make(path: &Path, text: &str, bits: u32) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, text)?;
    fs::set_permissions(path, fs::Permissions::from_mode(bits))
}

fn data(ctx: &Ctx) -> PathBuf {
    ctx.home().join(".local/share/oh-my-shell")
}

/// A shell in the home directory with `proj/build/{f,sub/g}` and `proj/secret`.
fn shell_with_project(ctx: &Ctx) -> io::Result<Session> {
    let proj = ctx.home().join("proj");
    make(&proj.join("build/f"), "f\n", 0o640)?;
    make(&proj.join("build/sub/g"), "g\n", 0o755)?;
    fs::set_permissions(proj.join("build"), fs::Permissions::from_mode(0o750))?;
    fs::set_permissions(proj.join("build/sub"), fs::Permissions::from_mode(0o700))?;
    make(&proj.join("secret"), "s\n", 0o600)?;
    ctx.launch().cwd(ctx.home()).start()
}

#[test]
fn t_moves_to_the_trash_and_undo_restores_the_files_with_their_permissions() {
    pty_test(BIN, |ctx| {
        let mut term = shell_with_project(ctx)?;
        term.command("rm -rf proj/build proj/secret")?;
        term.expect_plain("this command is dangerous:", SHORT);
        term.expect_plain(QUESTION, SHORT);
        term.line("t")?;
        term.expect_plain(
            "moved 2 items to the trash; \"undo\" brings them back (#1)",
            SHORT,
        );
        term.prompt();
        let proj = ctx.home().join("proj");
        assert!(!proj.join("build").exists() && !proj.join("secret").exists());
        // They are in the trash, in a folder of their own.
        let trash = data(ctx).join("trash");
        let folders: Vec<_> = fs::read_dir(&trash)?.flatten().collect();
        assert_eq!(folders.len(), 1);
        assert_eq!(mode(&trash), 0o700);
        assert_eq!(mode(&data(ctx).join("undo.db")), 0o600);

        term.command("undo")?;
        term.expect_plain("undo #1", SHORT);
        term.expect_plain("- bring back ", SHORT);
        term.expect_plain("Undo it? [y/N]", SHORT);
        term.line("y")?;
        term.expect_plain("restored ", SHORT);
        term.prompt();
        // The whole point: everything is back, as it was.
        assert_eq!(fs::read_to_string(proj.join("build/f"))?, "f\n");
        assert_eq!(fs::read_to_string(proj.join("build/sub/g"))?, "g\n");
        assert_eq!(fs::read_to_string(proj.join("secret"))?, "s\n");
        assert_eq!(mode(&proj.join("build")), 0o750);
        assert_eq!(mode(&proj.join("build/f")), 0o640);
        assert_eq!(mode(&proj.join("build/sub")), 0o700);
        assert_eq!(mode(&proj.join("build/sub/g")), 0o755);
        assert_eq!(mode(&proj.join("secret")), 0o600);

        // And a second `undo` has nothing left.
        term.command("undo")?;
        term.expect_plain("nothing to undo", SHORT);
        term.quit()
    });
}

#[test]
fn undo_asks_and_n_changes_nothing_and_list_shows_the_action() {
    pty_test(BIN, |ctx| {
        let mut term = shell_with_project(ctx)?;
        term.command("rm -rf proj/build")?;
        term.expect_plain(QUESTION, SHORT);
        term.line("t")?;
        term.expect_plain("moved 1 item to the trash", SHORT);
        term.prompt();
        term.command("undo --list")?;
        term.expect_plain("typed  1 item  rm -rf proj/build", SHORT);
        term.prompt();
        term.command("undo")?;
        term.expect_plain("Undo it? [y/N]", SHORT);
        term.line("n")?;
        term.expect_plain("nothing was undone", SHORT);
        term.prompt();
        assert!(!ctx.home().join("proj/build").exists());
        term.command("undo 1")?;
        term.expect_plain("Undo it? [y/N]", SHORT);
        term.line("y")?;
        term.expect_plain("restored ", SHORT);
        term.prompt();
        assert!(ctx.home().join("proj/build/f").exists());
        term.quit()
    });
}

#[test]
fn y_still_deletes_for_good_and_leaves_nothing_in_the_trash() {
    pty_test(BIN, |ctx| {
        let mut term = shell_with_project(ctx)?;
        term.command("rm -rf proj/build")?;
        term.expect_plain(QUESTION, SHORT);
        term.line("y")?;
        term.prompt();
        assert!(!ctx.home().join("proj/build").exists());
        assert!(!data(ctx).join("undo.db").exists(), "nothing was journaled");
        term.command("undo")?;
        term.expect_plain("nothing to undo", SHORT);
        term.quit()
    });
}

#[test]
fn a_line_the_trash_cannot_cover_is_not_offered_the_option() {
    pty_test(BIN, |ctx| {
        let mut term = shell_with_project(ctx)?;
        // `command rm` is a wrapper: the trash does not replace it, so `t` is not offered.
        term.command("command rm -rf proj/build")?;
        term.expect_plain("Run it? [y/N]", SHORT);
        term.line("n")?;
        term.expect_plain("cancelled; nothing ran", SHORT);
        assert!(
            !term.transcript().contains("move to the trash instead"),
            "{}",
            term.transcript()
        );
        // A critical line is never offered it either.
        term.command("rm -rf /nonexistent-omsh-trash-test")?;
        term.expect_plain("Run it? [y/N]", SHORT);
        term.line("n")?;
        term.quit()
    });
}

#[test]
fn the_trash_is_off_under_no_config() {
    pty_test(BIN, |ctx| {
        make(&ctx.home().join("proj/build/f"), "f\n", 0o644)?;
        let mut term = ctx.launch().arg("--no-config").cwd(ctx.home()).start()?;
        term.command("rm -rf proj/build")?;
        term.expect_plain("Run it? [y/N]", SHORT);
        assert!(
            !term.transcript().contains("move to the trash instead"),
            "{}",
            term.transcript()
        );
        term.line("n")?;
        term.prompt();
        term.command("undo")?;
        term.expect_plain("undo: not available in this shell", SHORT);
        term.command("__omsh_trash -rf proj/build")?;
        term.expect_plain(
            "the trash is not available in this shell; nothing was deleted",
            SHORT,
        );
        term.prompt();
        assert!(
            ctx.home().join("proj/build/f").exists(),
            "nothing was deleted"
        );
        assert!(!data(ctx).exists(), "--no-config writes nothing");
        term.quit()
    });
}

#[test]
fn old_trash_is_announced_in_one_line_and_emptied() {
    pty_test(BIN, |ctx| {
        let trash = data(ctx).join("trash");
        let old_secs = 1_000_000_000_i64;
        let fresh_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        for (secs, id) in [(old_secs, 7), (fresh_secs.try_into().unwrap(), 8)] {
            let folder = trash.join(format!("{secs}-{id}/1"));
            fs::create_dir_all(&folder)?;
            fs::write(folder.join("file"), "x")?;
        }
        let mut term = ctx.launch().cwd(ctx.home()).start()?;
        // Printed before the first prompt, so `start` has already seen it.
        assert!(
            term.transcript()
                .contains("oh-my-shell: emptying 1 trash entry older than 8 days"),
            "{}",
            term.transcript()
        );
        term.prompt();
        for _ in 0..100 {
            if !trash.join(format!("{old_secs}-7")).exists()
                && !trash.join(format!(".purging-{old_secs}-7")).exists()
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(!trash.join(format!("{old_secs}-7")).exists());
        assert!(!trash.join(format!(".purging-{old_secs}-7")).exists());
        assert!(trash.join(format!("{fresh_secs}-8/1/file")).exists());
        term.quit()?;
        // A second shell has nothing to announce.
        let mut again = ctx.launch().cwd(ctx.home()).start()?;
        assert!(
            !again.transcript().contains("emptying"),
            "{}",
            again.transcript()
        );
        again.quit()
    });
}

#[test]
fn the_retention_setting_decides_what_is_old() {
    pty_test(BIN, |ctx| {
        let trash = data(ctx).join("trash");
        let two_days_ago = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            - 2 * 86_400;
        fs::create_dir_all(trash.join(format!("{two_days_ago}-3/1")))?;
        let config = ctx.home().join(".config/oh-my-shell");
        fs::create_dir_all(&config)?;
        fs::write(
            config.join("config.toml"),
            "[safety]\ntrash_retention_days = 1\n",
        )?;
        let mut term = ctx.launch().cwd(ctx.home()).start()?;
        assert!(
            term.transcript()
                .contains("oh-my-shell: emptying 1 trash entry older than 1 days"),
            "{}",
            term.transcript()
        );
        term.quit()
    });
}

// -- AI plans ---------------------------------------------------------------------------------

fn plan(steps: &[(&str, &str, &str)]) -> String {
    let escape = |text: &str| text.replace('\\', "\\\\").replace('"', "\\\"");
    let steps: Vec<String> = steps
        .iter()
        .map(|(title, command, risk)| {
            format!(
                r#"{{"title":"{}","command":"{}","explanation":"Because.","risk":"{risk}","needs_root":false}}"#,
                escape(title),
                escape(command)
            )
        })
        .collect();
    format!(
        r#"{{"summary":"Do the thing","steps":[{}]}}"#,
        steps.join(",")
    )
}

fn configure(ctx: &Ctx, server: &Server) -> io::Result<()> {
    let dir = ctx.home().join(".config/oh-my-shell");
    fs::create_dir_all(&dir)?;
    fs::write(
        dir.join("config.toml"),
        format!(
            "[ai]\nprovider = \"openai-compatible\"\nmodel = \"model-one\"\nbase_url = \"{}\"\n[safety]\nblast_radius_max_ms = 60000\n",
            server.url("/v1")
        ),
    )
}

#[test]
fn an_ai_plan_deletes_through_the_trash_and_undo_brings_it_back() {
    pty_test(BIN, |ctx| {
        make(&ctx.home().join("proj/build/f"), "f\n", 0o640)?;
        let answer = plan(&[("Remove build", "rm -rf proj/build", "high")]);
        let server = Server::start_answering(Duration::ZERO, false, &[&answer])?;
        configure(ctx, &server)?;
        let mut term = ctx
            .launch()
            .env(
                "DBUS_SESSION_BUS_ADDRESS",
                "unix:path=/nonexistent/omsh-test-bus",
            )
            .cwd(ctx.home())
            .start()?;
        term.command("? remove the build folder")?;
        term.expect_plain("rm goes to the trash; undo can bring it back", SHORT);
        term.write_bytes(b"\r")?;
        term.expect_plain("Run it? [y/N]", SHORT);
        term.line("y")?;
        term.expect_plain("moved 1 item to the trash", SHORT);
        term.expect_plain("plan done (1 step)", SHORT);
        term.prompt();
        assert!(!ctx.home().join("proj/build").exists());

        term.command("undo --list")?;
        term.expect_plain("plan  1 item  rm -rf proj/build", SHORT);
        term.prompt();
        term.command("undo")?;
        term.expect_plain("Undo it? [y/N]", SHORT);
        term.line("y")?;
        term.expect_plain("restored ", SHORT);
        term.prompt();
        assert_eq!(fs::read_to_string(ctx.home().join("proj/build/f"))?, "f\n");
        assert_eq!(mode(&ctx.home().join("proj/build/f")), 0o640);
        term.quit()
    });
}

#[test]
fn an_ai_plan_that_makes_and_moves_files_can_be_undone() {
    pty_test(BIN, |ctx| {
        make(&ctx.home().join("a.txt"), "a\n", 0o644)?;
        let answer = plan(&[("Tidy", "mkdir out && mv a.txt out/", "low")]);
        let server = Server::start_answering(Duration::ZERO, false, &[&answer])?;
        configure(ctx, &server)?;
        let mut term = ctx
            .launch()
            .env(
                "DBUS_SESSION_BUS_ADDRESS",
                "unix:path=/nonexistent/omsh-test-bus",
            )
            .cwd(ctx.home())
            .start()?;
        term.command("? tidy up")?;
        term.expect_plain("undo can reverse: mkdir, mv", SHORT);
        term.write_bytes(b"\r")?;
        term.expect_plain("recorded 2 changes", SHORT);
        term.expect_plain("plan done (1 step)", SHORT);
        term.prompt();
        assert!(ctx.home().join("out/a.txt").exists());
        term.command("undo")?;
        term.expect_plain("Undo it? [y/N]", SHORT);
        term.line("y")?;
        term.expect_plain("renamed ", SHORT);
        term.prompt();
        assert!(ctx.home().join("a.txt").exists());
        assert!(!ctx.home().join("out").exists());
        term.quit()
    });
}

// -- another file system ----------------------------------------------------------------------

#[test]
fn a_trash_on_another_file_system_is_a_verified_copy_and_undo_restores_it_exactly() {
    use std::os::unix::fs::{MetadataExt, symlink};
    pty_test(BIN, |ctx| {
        let shm = PathBuf::from(format!("/dev/shm/omsh-xdev-{}", std::process::id()));
        let _ = fs::remove_dir_all(&shm);
        if fs::create_dir_all(&shm).is_err()
            || fs::metadata(&shm)?.dev() == fs::metadata(ctx.home())?.dev()
        {
            omsh_tests::skip::skip(
                "a_trash_on_another_file_system",
                "/dev/shm is missing or on the same file system as the test's home folder",
            );
            return Ok(());
        }
        // Removed whatever happens next.
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(shm.clone());

        let proj = ctx.home().join("proj");
        make(&proj.join("build/f"), "f\n", 0o640)?;
        make(&proj.join("build/sub/g"), "g\n", 0o755)?;
        symlink("f", proj.join("build/link"))?;
        fs::set_permissions(proj.join("build/sub"), fs::Permissions::from_mode(0o500))?;
        let before = fs::metadata(proj.join("build/f"))?.mtime();

        let mut term = ctx
            .launch()
            .env("XDG_DATA_HOME", &shm.to_string_lossy())
            .cwd(ctx.home())
            .start()?;
        term.command("rm -rf proj/build")?;
        term.expect_plain(QUESTION, SHORT);
        term.line("t")?;
        term.expect_plain("moved 1 item to the trash", SHORT);
        term.prompt();
        assert!(!proj.join("build").exists());
        // The copy is in the other file system's trash, whole.
        let mut found = Vec::new();
        for entry in walk(&shm.join("oh-my-shell/trash")) {
            found.push(entry);
        }
        assert!(
            found.iter().any(|p| p.ends_with("build/sub/g")),
            "{found:?}"
        );

        term.command("undo")?;
        term.expect_plain("Undo it? [y/N]", SHORT);
        term.line("y")?;
        term.expect_plain("restored ", SHORT);
        term.prompt();
        assert_eq!(fs::read_to_string(proj.join("build/f"))?, "f\n");
        assert_eq!(fs::read_link(proj.join("build/link"))?, Path::new("f"));
        assert_eq!(mode(&proj.join("build/f")), 0o640);
        assert_eq!(mode(&proj.join("build/sub")), 0o500);
        assert_eq!(mode(&proj.join("build/sub/g")), 0o755);
        assert_eq!(fs::metadata(proj.join("build/f"))?.mtime(), before);
        // A folder without write permission cannot be removed with the test's home folder.
        fs::set_permissions(proj.join("build/sub"), fs::Permissions::from_mode(0o700))?;
        term.quit()
    });
}

/// Every path under `root`, files and folders.
fn walk(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut todo = vec![root.to_path_buf()];
    while let Some(dir) = todo.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            out.push(entry.path());
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                todo.push(entry.path());
            }
        }
    }
    out
}
