//! `omsh uninstall` (S29b), end to end on a terminal. Every test runs through [`Sandbox`], which
//! checks that nothing can touch the real home directory; the program, its link, the settings,
//! the history and the trash are all files of the sandbox.

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use omsh_tests::sandbox::Sandbox;
use omsh_tests::{Ctx, SHORT, Session, pty_test};

const BIN: &str = env!("CARGO_BIN_EXE_omsh-pty-shell");

struct Files {
    config: PathBuf,
    init: PathBuf,
    credentials: PathBuf,
    history: PathBuf,
    undo: PathBuf,
    trashed: PathBuf,
    state: PathBuf,
}

/// Files of every kind the command asks about.
fn make_files(ctx: &Ctx) -> std::io::Result<Files> {
    let home = ctx.home();
    let put = |rel: &str, text: &str| -> std::io::Result<PathBuf> {
        let path = home.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, text)?;
        Ok(path)
    };
    Ok(Files {
        config: put(".config/oh-my-shell/config.toml", "# mine\n")?,
        init: put(".config/oh-my-shell/init.omsh", "alias a=b\n")?,
        credentials: put(".config/oh-my-shell/credentials", "openai=not-a-real-key\n")?,
        history: put(".local/share/oh-my-shell/history.db", "history")?,
        undo: put(".local/share/oh-my-shell/undo.db", "undo")?,
        trashed: put(".local/share/oh-my-shell/trash/1/thing", "kept for undo")?,
        state: put(".local/state/oh-my-shell/update-check", "checked=1\n")?,
    })
}

/// Waits for `question` and answers it with `key`.
fn answer(term: &mut Session, question: &str, key: u8) -> std::io::Result<()> {
    term.expect(question, SHORT);
    term.key(key)
}

fn finish(mut term: Session) -> (Option<i32>, String) {
    let status = term.wait_exit(SHORT).code();
    term.pump(Duration::from_millis(50));
    (status, term.transcript().to_owned())
}

#[test]
fn each_answer_is_for_its_own_question() {
    pty_test(BIN, |ctx| {
        let sandbox = Sandbox::new(ctx, BIN)?;
        let files = make_files(ctx)?;
        let mut term = sandbox.omsh()?.arg("uninstall").start_raw()?;
        // program: yes, settings: no, history: yes, trash: no, keys: yes
        answer(&mut term, "Remove the program", b'y')?;
        answer(&mut term, "Remove your settings", b'n')?;
        answer(&mut term, "Remove your command history", b'y')?;
        answer(&mut term, "Remove the trash and the undo journal", b'n')?;
        answer(&mut term, "Remove the stored AI keys", b'y')?;
        let (status, shown) = finish(term);
        assert_eq!(status, Some(0), "{shown}");
        assert!(!sandbox.program().exists(), "the program is gone: {shown}");
        assert!(!ctx.home().join("bin/omsh").exists(), "so is the link");
        assert!(!files.history.exists());
        assert!(!files.credentials.exists(), "keys were removed");
        assert!(
            !files.state.exists(),
            "the state folder goes with the program"
        );
        assert!(
            files.config.exists() && files.init.exists(),
            "settings kept"
        );
        assert!(files.undo.exists() && files.trashed.exists(), "trash kept");
        assert!(shown.contains("Kept:"), "{shown}");
        Ok(())
    });
}

#[test]
fn yes_to_everything_leaves_no_trace_of_ours() {
    pty_test(BIN, |ctx| {
        let sandbox = Sandbox::new(ctx, BIN)?;
        make_files(ctx)?;
        let mut term = sandbox.omsh()?.arg("uninstall").start_raw()?;
        for question in [
            "Remove the program",
            "Remove your settings",
            "Remove your command history",
            "Remove the trash and the undo journal",
            "Remove the stored AI keys",
        ] {
            answer(&mut term, question, b'y')?;
        }
        let (status, shown) = finish(term);
        assert_eq!(status, Some(0), "{shown}");
        for gone in [
            "bin/oh-my-shell",
            "bin/omsh",
            ".config/oh-my-shell",
            ".local/share/oh-my-shell",
            ".local/state/oh-my-shell",
        ] {
            assert!(
                !ctx.home().join(gone).exists(),
                "{gone} should be gone: {shown}"
            );
        }
        Ok(())
    });
}

#[test]
fn escape_at_a_later_question_removes_nothing_at_all() {
    pty_test(BIN, |ctx| {
        let sandbox = Sandbox::new(ctx, BIN)?;
        let files = make_files(ctx)?;
        let mut term = sandbox.omsh()?.arg("uninstall").start_raw()?;
        answer(&mut term, "Remove the program", b'y')?;
        answer(&mut term, "Remove your settings", b'y')?;
        answer(&mut term, "Remove your command history", 0x1b)?;
        let (status, shown) = finish(term);
        assert_eq!(status, Some(1), "{shown}");
        assert!(shown.contains("Nothing was removed"), "{shown}");
        assert!(sandbox.program().exists(), "the program stays");
        assert!(files.config.exists() && files.history.exists() && files.state.exists());
        Ok(())
    });
}

#[test]
fn ctrl_c_removes_nothing_either() {
    pty_test(BIN, |ctx| {
        let sandbox = Sandbox::new(ctx, BIN)?;
        let files = make_files(ctx)?;
        let mut term = sandbox.omsh()?.arg("uninstall").start_raw()?;
        answer(&mut term, "Remove the program", b'y')?;
        answer(&mut term, "Remove your settings", 3)?;
        let (status, shown) = finish(term);
        assert_eq!(status, Some(130), "{shown}");
        assert!(shown.contains("Nothing was removed"), "{shown}");
        assert!(sandbox.program().exists());
        assert!(files.config.exists());
        Ok(())
    });
}

#[test]
fn enter_means_no() {
    pty_test(BIN, |ctx| {
        let sandbox = Sandbox::new(ctx, BIN)?;
        let files = make_files(ctx)?;
        let mut term = sandbox.omsh()?.arg("uninstall").start_raw()?;
        for question in [
            "Remove the program",
            "Remove your settings",
            "Remove your command history",
            "Remove the trash and the undo journal",
            "Remove the stored AI keys",
        ] {
            answer(&mut term, question, b'\r')?;
        }
        let (status, shown) = finish(term);
        assert_eq!(status, Some(0), "{shown}");
        assert!(shown.contains("Nothing was removed"), "{shown}");
        assert!(sandbox.program().exists() && files.config.exists() && files.credentials.exists());
        Ok(())
    });
}

#[test]
fn inside_the_shell_it_is_refused() {
    pty_test(BIN, |ctx| {
        let sandbox = Sandbox::new(ctx, BIN)?;
        let files = make_files(ctx)?;
        let mut shell = sandbox.launch().start()?;
        shell.command("omsh uninstall")?;
        shell.expect("Exit this shell", SHORT);
        shell.sync("still-alive")?;
        shell.quit()?;
        assert!(sandbox.program().exists(), "the program stays");
        assert!(files.config.exists() && files.history.exists());
        Ok(())
    });
}

#[test]
fn without_a_terminal_it_is_refused() {
    pty_test(BIN, |ctx| {
        let sandbox = Sandbox::new(ctx, BIN)?;
        let files = make_files(ctx)?;
        let output = sandbox.command()?.arg("uninstall").output()?;
        let shown = String::from_utf8_lossy(&output.stderr).into_owned();
        assert_eq!(output.status.code(), Some(1), "{shown}");
        assert!(shown.contains("needs a terminal"), "{shown}");
        assert!(sandbox.program().exists() && files.config.exists());
        Ok(())
    });
}

#[test]
fn bad_arguments_are_a_usage_error_and_remove_nothing() {
    pty_test(BIN, |ctx| {
        let sandbox = Sandbox::new(ctx, BIN)?;
        make_files(ctx)?;
        let output = sandbox.command()?.args(["uninstall", "--yes"]).output()?;
        assert_eq!(output.status.code(), Some(2));
        assert!(sandbox.program().exists());
        Ok(())
    });
}

#[test]
fn a_file_of_someone_else_in_the_same_folders_stays() {
    pty_test(BIN, |ctx| {
        let sandbox = Sandbox::new(ctx, BIN)?;
        make_files(ctx)?;
        let other = ctx.home().join("bin/other-tool");
        fs::write(&other, "x")?;
        let theirs = ctx.home().join(".config/other/config");
        fs::create_dir_all(theirs.parent().expect("a parent"))?;
        fs::write(&theirs, "x")?;
        let mut term = sandbox.omsh()?.arg("uninstall").start_raw()?;
        for question in [
            "Remove the program",
            "Remove your settings",
            "Remove your command history",
            "Remove the trash and the undo journal",
            "Remove the stored AI keys",
        ] {
            answer(&mut term, question, b'y')?;
        }
        let (status, shown) = finish(term);
        assert_eq!(status, Some(0), "{shown}");
        assert!(other.exists() && theirs.exists());
        Ok(())
    });
}
