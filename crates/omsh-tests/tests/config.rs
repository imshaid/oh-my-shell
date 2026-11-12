//! The configuration file and the startup file (S12), through the real startup path of the
//! shell on a terminal.
//!
//! Every test has a home directory of its own (`Ctx::home`); the shell writes its first-run
//! file there and never in a real one.

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use omsh_tests::{Ctx, SHORT, pty_test};

const BIN: &str = env!("CARGO_BIN_EXE_omsh-pty-shell");

/// `~/.config/oh-my-shell` of the test.
fn config_dir(ctx: &Ctx) -> PathBuf {
    ctx.home().join(".config/oh-my-shell")
}

fn write_config(ctx: &Ctx, name: &str, text: &str) -> io::Result<PathBuf> {
    let dir = config_dir(ctx);
    fs::create_dir_all(&dir)?;
    let file = dir.join(name);
    fs::write(&file, text)?;
    Ok(file)
}

/// Done-when: a broken config still starts, with defaults for the broken keys and a clear error.
#[test]
fn a_broken_config_still_starts_with_the_defaults_and_a_clear_error() {
    pty_test(BIN, |ctx| {
        let file = write_config(
            ctx,
            "config.toml",
            "[ui]\nicons = \"unicode\"\nmotion = \"fast\"\n\n[history]\nsize = \"lots\"\n\n[editor\nmode = \"vi\"\n",
        )?;
        // The plain reader (`dumb`): `start` waits for the first prompt, `$ `. The editor's own
        // test of a broken file is in `editor.rs`.
        let mut term = ctx.launch().dumb().start()?;
        let shown = term.transcript().to_owned();
        let name = file.display();
        assert!(
            shown.contains(&format!(
                "{name}:3:10: ui.motion must be \"full\", \"reduced\" or \"off\", not \"fast\""
            )),
            "{shown}"
        );
        assert!(
            shown.contains(&format!(
                "{name}:6:8: history.size must be an integer, not a string"
            )),
            "{shown}"
        );
        assert!(
            shown.contains(&format!("{name}:8:")),
            "the syntax error has its line\n{shown}"
        );

        // The shell works.
        term.line("echo alive-$((6*7))")?;
        term.expect("alive-42", SHORT);
        // The good key applies although others are wrong: `unicode` draws the box characters
        // of a report even on this `dumb` terminal with a C locale, where `auto` would not.
        term.line("echo )")?;
        term.expect("╭", SHORT);
        term.quit()
    });
}

#[test]
fn the_first_run_creates_the_commented_file_once() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        // The first start writes the file and says so once, in the three-line welcome (S28):
        // the file's place, and nothing about a problem with it.
        assert!(
            term.transcript()
                .contains("Settings: ~/.config/oh-my-shell/config.toml"),
            "{}",
            term.transcript()
        );
        assert!(
            !term.transcript().contains("oh-my-shell: "),
            "{}",
            term.transcript()
        );
        term.quit()?;

        let dir = config_dir(ctx);
        let file = dir.join("config.toml");
        let text = fs::read_to_string(&file)?;
        assert!(text.starts_with("# Oh My Shell configuration"), "{text}");
        assert!(text.contains("#theme = \"nord\""));
        assert_eq!(fs::metadata(&dir)?.permissions().mode() & 0o777, 0o700);
        assert_eq!(fs::metadata(&file)?.permissions().mode() & 0o022, 0);

        // A second start leaves the file as the user has it, and says nothing about it.
        fs::write(&file, format!("{text}\n# my own line\n"))?;
        let before = fs::read(&file)?;
        let mut term = ctx.start()?;
        assert!(
            !term.transcript().contains("config.toml"),
            "{}",
            term.transcript()
        );
        term.quit()?;
        assert_eq!(fs::read(&file)?, before);
        // And no temporary file was left in the directory.
        assert_eq!(fs::read_dir(&dir)?.count(), 1);
        Ok(())
    });
}

#[test]
fn init_omsh_runs_before_the_first_prompt() {
    pty_test(BIN, |ctx| {
        write_config(
            ctx,
            "init.omsh",
            "alias ll='echo long-listing'\ngreet() { echo hello-$1; }\nexport FROM_INIT=yes\nPLAIN=1\n",
        )?;
        let mut term = ctx.start()?;
        // What the typed line echoes never contains the answer, so each wait is for the answer.
        term.line("ll")?;
        term.expect("long-listing", SHORT);
        term.line("greet there")?;
        term.expect("hello-there", SHORT);
        term.line("echo v-$FROM_INIT-$PLAIN")?;
        term.expect("v-yes-1", SHORT);
        // The export reaches programs.
        term.line("sh -c 'echo e-$FROM_INIT'")?;
        term.expect("e-yes", SHORT);
        // And the status of the last line of the file does not leak into the first prompt.
        term.line("echo s-$?")?;
        term.expect("s-0", SHORT);
        term.quit()
    });
}

#[test]
fn a_mistake_in_init_omsh_has_a_place_and_the_shell_still_starts() {
    pty_test(BIN, |ctx| {
        let file = write_config(
            ctx,
            "init.omsh",
            "export BEFORE=1\n\nif then\nexport AFTER=1\n",
        )?;
        let mut term = ctx.start()?;
        let wanted = format!("{}:3:", file.display());
        assert!(term.transcript().contains(&wanted), "{}", term.transcript());
        term.line("echo v-$BEFORE-${AFTER-unset}")?;
        term.expect("v-1-unset", SHORT);
        term.quit()
    });
}

#[test]
fn exit_in_init_omsh_ends_the_shell_and_no_config_is_the_way_back() {
    pty_test(BIN, |ctx| {
        write_config(ctx, "init.omsh", "exit 4\n")?;
        let mut term = ctx.launch().start_raw()?;
        let status = term.wait_exit(SHORT);
        assert_eq!(status.code(), Some(4), "{}", term.transcript());

        let mut term = ctx.launch().arg("--no-config").start()?;
        term.line("echo back-$((1+1))")?;
        term.expect("back-2", SHORT);
        term.quit()
    });
}

#[test]
fn no_config_reads_nothing_and_writes_nothing() {
    pty_test(BIN, |ctx| {
        // No files at all: the directories are not created.
        let mut term = ctx.launch().arg("--no-config").start()?;
        term.quit()?;
        assert!(!ctx.home().join(".config").exists());

        // Broken files: neither is read, and neither is touched.
        let config = write_config(ctx, "config.toml", "[ui\n")?;
        let init = write_config(ctx, "init.omsh", "export FROM_INIT=yes\nif then\n")?;
        let mut term = ctx.launch().arg("--no-config").start()?;
        assert!(
            !term.transcript().contains("config.toml"),
            "{}",
            term.transcript()
        );
        assert!(
            !term.transcript().contains("init.omsh"),
            "{}",
            term.transcript()
        );
        term.line("echo v-${FROM_INIT-none}")?;
        term.expect("v-none", SHORT);
        term.quit()?;
        assert_eq!(fs::read_to_string(config)?, "[ui\n");
        assert_eq!(fs::read_to_string(init)?, "export FROM_INIT=yes\nif then\n");
        Ok(())
    });
}

/// The files are found through the shell's own variables, after the login profile ran: a
/// profile may move them.
#[test]
fn a_login_shell_reads_its_profile_before_it_looks_for_the_configuration() {
    pty_test(BIN, |ctx| {
        let moved = ctx.home().join("moved");
        fs::create_dir_all(moved.join("oh-my-shell"))?;
        fs::write(
            moved.join("oh-my-shell/init.omsh"),
            "export FROM_MOVED=yes\n",
        )?;
        fs::write(
            ctx.home().join(".profile"),
            format!(
                "XDG_CONFIG_HOME={}\nexport XDG_CONFIG_HOME\n",
                moved.display()
            ),
        )?;
        let mut term = ctx.launch().arg("-l").start()?;
        term.line("echo v-$FROM_MOVED")?;
        term.expect("v-yes", SHORT);
        term.quit()?;
        // The first-run file went to the moved directory, not to the default one.
        assert!(moved.join("oh-my-shell/config.toml").exists());
        assert!(!ctx.home().join(".config").exists());
        Ok(())
    });
}
