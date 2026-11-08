//! The environment of a login shell.
//!
//! `/etc/profile` and `~/.profile` are written in `sh` syntax, so this shell does not read
//! them itself. A login shell asks `/bin/sh` instead: `sh -l -c 'env -0 >&3'` runs the profile
//! files and then writes the resulting environment, as NUL-separated `NAME=value` entries, to
//! descriptor 3, which is a pipe the shell reads. What the profile prints (a greeting, say) goes
//! to the terminal as usual and cannot mix with the data.

use std::ffi::CString;
use std::os::fd::OwnedFd;
use std::sync::Arc;
use std::time::Duration;

use nix::errno::Errno;
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use nix::libc;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::{pipe2, read};

use crate::io::Io;
use crate::shell::Shell;
use crate::signals;
use crate::spawn::{self, ExecRequest};
use crate::status::Termination;

/// Names the login shell keeps for itself: they describe this shell and not the user's
/// environment, so what `sh` computed for them is wrong here.
const KEPT: [&str; 5] = ["_", "SHLVL", "PWD", "OLDPWD", "OMSH_VERSION"];

/// How often the loop looks at the child while the pipe is quiet.
const STEP: Duration = Duration::from_millis(50);

impl Shell {
    /// Imports the login environment (see the module documentation). Returns true if it was
    /// imported. Any failure is reported on descriptor 2 of `io` and the shell goes on with the
    /// environment it has: a broken profile must not lock anyone out.
    ///
    /// Variables are only added or changed, never removed.
    pub fn import_login_environment(&mut self, io: &Io) -> bool {
        match self.run_login_shell(io) {
            Ok(bytes) => {
                for (name, value) in parse_environment(&bytes) {
                    // A read only variable keeps its value; the environment came from outside.
                    if self.vars.set(&name, value).is_ok() {
                        self.vars.export(&name);
                    }
                }
                true
            }
            Err(reason) => {
                io.say(format!(
                    "oh-my-shell: the login environment was not imported: {reason}"
                ));
                false
            }
        }
    }

    /// Runs `sh -l -c 'env -0 >&3'` and returns what it wrote to descriptor 3.
    fn run_login_shell(&self, io: &Io) -> Result<Vec<u8>, String> {
        let (reader, writer) = pipe2(OFlag::O_CLOEXEC).map_err(|e| format!("no pipe: {e}"))?;
        // The reading end never blocks, so that the last bytes can be taken after the child
        // is gone even if something it started still holds the writing end open.
        fcntl(&reader, FcntlArg::F_SETFL(OFlag::O_NONBLOCK))
            .map_err(|e| format!("no pipe: {e}"))?;
        let mut table = io.clone();
        table.set(3, Arc::new(writer));
        let mut plan = table.plan();

        let argv: Vec<CString> = [c"sh", c"-l", c"-c", c"env -0 >&3"]
            .iter()
            .map(|arg| (*arg).to_owned())
            .collect();
        let envp = self.vars.envp(&[]);
        // The profile runs in the shell's own group and in the foreground, like a top-level
        // `$(...)`: nothing may stop it where the user cannot continue it.
        let ignored = self.traps.ignored_mask()
            | signals::inherited_ignored()
            | (1_u64 << libc::SIGTSTP)
            | (1_u64 << libc::SIGTTIN)
            | (1_u64 << libc::SIGTTOU);
        let request = ExecRequest {
            path: c"/bin/sh",
            argv: &argv,
            envp: &envp,
            cwd: None,
            umask: None,
            ignored,
            group: None,
        };
        let child =
            spawn::start(&request, &mut plan).map_err(|e| format!("cannot run sh: {e:?}"))?;
        // Let go of the writing end here, or the end of the output would never be seen.
        drop(plan);
        drop(table);

        let mut output = Vec::new();
        let termination = loop {
            drain(&reader, &mut output);
            match waitpid(child, Some(WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::StillAlive) => wait_for_data(&reader),
                Ok(status) => {
                    if let Some(ending) = Termination::from_wait(status) {
                        break ending;
                    }
                }
                Err(Errno::EINTR) => {}
                Err(_) => break Termination::Exited(255),
            }
        };
        // Whatever was written before the child ended is in the pipe by now.
        drain(&reader, &mut output);
        // A Ctrl+C aimed at a slow profile also reached the shell and set its flag; the next
        // line clears it (`run_source`), and nothing looks at it before that.
        match termination {
            Termination::Exited(0) => Ok(output),
            Termination::Exited(code) => Err(format!("`sh -l` exited with status {code}")),
            Termination::Signaled { signal, .. } => {
                Err(format!("`sh -l` was killed by signal {signal}"))
            }
        }
    }
}

/// Reads everything in the pipe right now.
fn drain(pipe: &OwnedFd, buffer: &mut Vec<u8>) {
    let mut chunk = [0_u8; 8192];
    loop {
        match read(pipe, &mut chunk) {
            Ok(0) => return,
            Ok(n) => buffer.extend_from_slice(&chunk[..n]),
            Err(Errno::EINTR) => {}
            Err(_) => return,
        }
    }
}

/// Sleeps until the pipe has data or a short time has passed.
fn wait_for_data(pipe: &OwnedFd) {
    let mut fds = [PollFd::new(pipe_fd(pipe), PollFlags::POLLIN)];
    let _ = poll(&mut fds, PollTimeout::from(STEP.as_millis() as u16));
}

fn pipe_fd(pipe: &OwnedFd) -> std::os::fd::BorrowedFd<'_> {
    std::os::fd::AsFd::as_fd(pipe)
}

/// Splits `env -0` output into `(name, value)` pairs that the shell should take over.
///
/// An entry without `=` or with a name that is not a shell variable name is skipped, and so are
/// the names in [`KEPT`]. An empty value is kept, and a value may hold a newline.
fn parse_environment(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
    bytes
        .split(|b| *b == 0)
        .filter_map(|entry| {
            let at = entry.iter().position(|b| *b == b'=')?;
            let name = std::str::from_utf8(&entry[..at]).ok()?;
            if !is_name(name) || KEPT.contains(&name) {
                return None;
            }
            Some((name.to_owned(), entry[at + 1..].to_vec()))
        })
        .collect()
}

fn is_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(pairs: &[(String, Vec<u8>)]) -> Vec<&str> {
        pairs.iter().map(|(n, _)| n.as_str()).collect()
    }

    #[test]
    fn entries_are_split_on_nul() {
        let got = parse_environment(b"A=1\0B=two words\0");
        assert_eq!(
            got,
            vec![
                ("A".to_owned(), b"1".to_vec()),
                ("B".to_owned(), b"two words".to_vec())
            ]
        );
    }

    #[test]
    fn the_shells_own_variables_are_not_taken() {
        let got = parse_environment(
            b"_=/usr/bin/env\0SHLVL=9\0PWD=/x\0OLDPWD=/y\0OMSH_VERSION=0\0KEEP=1\0",
        );
        assert_eq!(names(&got), ["KEEP"]);
    }

    #[test]
    fn bad_names_and_entries_without_equals_are_skipped() {
        let got = parse_environment(b"1A=x\0A-B=x\0=x\0NOEQUALS\0\0OK=1\0");
        assert_eq!(names(&got), ["OK"]);
    }

    #[test]
    fn an_empty_value_and_a_newline_in_a_value_are_kept() {
        let got = parse_environment(b"EMPTY=\0MULTI=a\nb\0");
        assert_eq!(
            got,
            vec![
                ("EMPTY".to_owned(), Vec::new()),
                ("MULTI".to_owned(), b"a\nb".to_vec())
            ]
        );
    }

    #[test]
    fn a_value_may_hold_an_equals_sign_and_bytes_that_are_not_utf8() {
        let got = parse_environment(b"A=b=c\0B=\xff\xfe\0");
        assert_eq!(got[0].1, b"b=c");
        assert_eq!(got[1].1, b"\xff\xfe");
    }

    /// A shell whose `HOME` holds the given `.profile`.
    fn shell_with_profile(profile: &str) -> (Shell, std::path::PathBuf) {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "omsh-login-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".profile"), profile).unwrap();
        let vars = crate::vars::Variables::from_pairs([
            ("HOME", dir.as_os_str().as_encoded_bytes().to_vec()),
            ("PATH", b"/usr/bin:/bin".to_vec()),
        ]);
        let mut shell = Shell::with_variables(vars);
        shell.vars.export("HOME");
        shell.vars.export("PATH");
        (shell, dir)
    }

    #[test]
    fn variables_from_the_profile_are_imported_and_exported() {
        let (mut shell, dir) = shell_with_profile("FROM_PROFILE=yes; export FROM_PROFILE\n");
        let out = crate::io::capture::Captured::new();
        assert!(shell.import_login_environment(&out.io), "{}", out.stderr());
        assert_eq!(shell.vars.get("FROM_PROFILE"), Some(&b"yes"[..]));
        assert!(shell.vars.is_exported("FROM_PROFILE"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn what_the_profile_prints_goes_to_the_terminal_and_not_into_the_data() {
        let (mut shell, dir) = shell_with_profile("echo hello-from-profile\nX=1; export X\n");
        let out = crate::io::capture::Captured::new();
        assert!(shell.import_login_environment(&out.io));
        assert_eq!(out.stdout(), "hello-from-profile\n");
        assert_eq!(shell.vars.get("X"), Some(&b"1"[..]));
        assert!(shell.vars.get("hello-from-profile").is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_failing_profile_imports_nothing_and_says_so() {
        let (mut shell, dir) = shell_with_profile("Y=1; export Y\nexit 3\n");
        let out = crate::io::capture::Captured::new();
        assert!(!shell.import_login_environment(&out.io));
        assert!(shell.vars.get("Y").is_none());
        assert!(out.stderr().contains("status 3"), "{}", out.stderr());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_shells_own_shlvl_survives_the_import() {
        let (mut shell, dir) = shell_with_profile("SHLVL=77; export SHLVL\n");
        let _ = shell.vars.set("SHLVL", b"2".to_vec());
        let out = crate::io::capture::Captured::new();
        assert!(shell.import_login_environment(&out.io));
        assert_eq!(shell.vars.get("SHLVL"), Some(&b"2"[..]));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_background_process_that_keeps_the_pipe_open_does_not_hang_the_import() {
        // `sleep` inherits descriptor 3; the import must not wait for it to end (30 s).
        let (mut shell, dir) =
            shell_with_profile("sleep 30 &\necho $! > \"$HOME/pid\"\nZ=1; export Z\n");
        let out = crate::io::capture::Captured::new();
        let started = std::time::Instant::now();
        let imported = shell.import_login_environment(&out.io);
        let took = started.elapsed();
        // Do not leave the sleep behind, whatever the checks say.
        if let Ok(pid) = std::fs::read_to_string(dir.join("pid"))
            && let Ok(pid) = pid.trim().parse::<i32>()
        {
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(pid),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
        assert!(imported, "{}", out.stderr());
        assert!(took < std::time::Duration::from_secs(10), "{took:?}");
        assert_eq!(shell.vars.get("Z"), Some(&b"1"[..]));
        let _ = std::fs::remove_dir_all(dir);
    }
}
