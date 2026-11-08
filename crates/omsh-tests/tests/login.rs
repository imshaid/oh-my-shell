//! The login shell (S11) on a real terminal: `-l`, and a program name that starts with `-`.
//!
//! A login shell imports the environment that `/bin/sh -l` builds from `/etc/profile` and
//! `~/.profile`, before the first prompt.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use omsh_tests::{SHORT, Session, pty_test};

const BIN: &str = env!("CARGO_BIN_EXE_omsh-pty-shell");

const CTRL_C: u8 = 3;

/// A home directory with the given `.profile`, removed at the end of the test.
struct Home(PathBuf);

impl Home {
    fn new(profile: &str) -> io::Result<Self> {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("login-{}-{n}", std::process::id()));
        fs::create_dir_all(&dir)?;
        fs::write(dir.join(".profile"), profile)?;
        Ok(Self(dir))
    }

    fn dir(&self) -> &str {
        self.0.to_str().unwrap_or_default()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

const PROFILE: &str = "echo WELCOME-FROM-PROFILE\nFROM_PROFILE=yes\nexport FROM_PROFILE\n";

/// What a login shell must show: the greeting from the profile on the terminal, the variables
/// it set, `$0` as the program was named, and its own `SHLVL`.
fn check_login(term: &mut Session) -> io::Result<()> {
    assert!(
        term.transcript().contains("WELCOME-FROM-PROFILE"),
        "the profile's output was not shown\n{}",
        term.transcript()
    );
    // The typed text is echoed, so each check looks for text that only the answer has.
    term.line("echo v-$FROM_PROFILE")?;
    term.expect("v-yes", SHORT);
    term.line("echo n-$0")?;
    term.expect("n--oh-my-shell", SHORT);
    // `SHLVL` is the shell's own: not what `sh` computed while it read the profile.
    term.line("echo l-$SHLVL")?;
    term.expect("l-1", SHORT);
    Ok(())
}

#[test]
fn a_program_name_that_starts_with_a_hyphen_is_a_login_shell() {
    pty_test(BIN, |ctx| {
        let home = Home::new(PROFILE)?;
        let mut term = ctx
            .launch()
            .as_login(&home.0)?
            .env("HOME", home.dir())
            .start()?;
        check_login(&mut term)?;
        term.quit()
    });
}

#[test]
fn dash_l_is_a_login_shell() {
    pty_test(BIN, |ctx| {
        let home = Home::new(PROFILE)?;
        let mut term = ctx.launch().arg("-l").env("HOME", home.dir()).start()?;
        assert!(term.transcript().contains("WELCOME-FROM-PROFILE"));
        term.line("echo v-$FROM_PROFILE")?;
        term.expect("v-yes", SHORT);
        // The name is the shell's usual one: only a name with a hyphen changes `$0`.
        term.line("echo n-$0")?;
        term.expect("n-oh-my-shell", SHORT);
        term.quit()
    });
}

#[test]
fn a_shell_that_is_not_a_login_does_not_read_the_profile() {
    pty_test(BIN, |ctx| {
        let home = Home::new(PROFILE)?;
        let mut term = ctx.launch().env("HOME", home.dir()).start()?;
        assert!(!term.transcript().contains("WELCOME-FROM-PROFILE"));
        term.line("echo v-${FROM_PROFILE-none}")?;
        term.expect("v-none", SHORT);
        term.quit()
    });
}

/// Ctrl+C during a slow profile stops the profile and not the shell: a prompt comes, with a
/// message, and the shell works.
#[test]
fn ctrl_c_during_a_slow_profile_gives_a_prompt_without_the_environment() {
    pty_test(BIN, |ctx| {
        let home = Home::new("FROM_PROFILE=yes\nexport FROM_PROFILE\nsleep 30\n")?;
        let mut term = ctx.launch().arg("-l").env("HOME", home.dir()).start_raw()?;
        // Wait until the profile is really running, then interrupt it.
        let mut waited = 0;
        while term.in_session("sleep").is_empty() {
            assert!(
                waited < 100,
                "the profile never started\n{}",
                term.transcript()
            );
            term.pump(Duration::from_millis(50));
            waited += 1;
        }
        term.key(CTRL_C)?;
        term.expect("not imported", SHORT);
        term.expect("$ ", SHORT);
        term.expect_none("sleep");

        // The shell works, and nothing from the half-run profile was taken.
        term.line("echo v-${FROM_PROFILE-none}")?;
        term.expect("v-none", SHORT);
        term.quit()
    });
}

#[test]
fn a_profile_that_fails_gives_one_message_and_a_prompt() {
    pty_test(BIN, |ctx| {
        let home = Home::new("FROM_PROFILE=yes\nexport FROM_PROFILE\nexit 1\n")?;
        let mut term = ctx.launch().arg("-l").env("HOME", home.dir()).start()?;
        let transcript = term.transcript().to_owned();
        assert_eq!(
            transcript.matches("not imported").count(),
            1,
            "{transcript}"
        );
        term.line("echo v-${FROM_PROFILE-none}")?;
        term.expect("v-none", SHORT);
        term.quit()
    });
}
