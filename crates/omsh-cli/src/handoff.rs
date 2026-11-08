//! Handing the work to `/bin/sh`.
//!
//! `-c`, a script and piped input are not for this shell's interpreter: tools such as `ssh`,
//! `scp`, editors and cron run `$SHELL -c ...` and expect plain POSIX `sh` behavior, quickly
//! and with nothing extra on stdout or stderr. So the process replaces itself with `/bin/sh`
//! (`execv`) before it reads any configuration or sets anything up. The same process id
//! continues as `sh`, signal dispositions and open files are as the caller left them, and
//! `SHLVL` is not touched.

use std::ffi::OsString;
use std::io::{self, Write};
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode};

/// The shell that runs the work.
const SH: &str = "/bin/sh";

/// Replaces this process with `/bin/sh` run with `args` (not including the program name,
/// which is `sh`). Returns only if that failed, with the status to exit with.
pub fn exec_sh(args: &[OsString]) -> ExitCode {
    // `exec` returns only on failure. std puts the signal mask and SIGPIPE in order first.
    let error = Command::new(SH).arg0("sh").args(args).exec();
    let _ = writeln!(io::stderr(), "oh-my-shell: cannot run {SH}: {error}");
    ExitCode::from(status_for(&error))
}

/// 126 if the file exists but cannot be run, 127 if it is not there (the shell convention).
fn status_for(error: &io::Error) -> u8 {
    match error.kind() {
        io::ErrorKind::NotFound => 127,
        _ => 126,
    }
}

/// The arguments of `sh` for `-c`: `[-l] -c command args...`.
pub fn command_args(login: bool, operands: Vec<OsString>) -> Vec<OsString> {
    let mut args = login_flag(login);
    args.push(OsString::from("-c"));
    args.extend(operands);
    args
}

/// The arguments of `sh` for a script: `[-l] script args...`.
pub fn script_args(login: bool, operands: Vec<OsString>) -> Vec<OsString> {
    let mut args = login_flag(login);
    args.extend(operands);
    args
}

/// The arguments of `sh` for piped input: `[-l]`.
pub fn stdin_args(login: bool) -> Vec<OsString> {
    login_flag(login)
}

fn login_flag(login: bool) -> Vec<OsString> {
    if login {
        vec![OsString::from("-l")]
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::ffi::OsStringExt;

    use super::*;

    fn os(text: &str) -> OsString {
        OsString::from(text)
    }

    #[test]
    fn a_command_string_is_passed_with_dash_c() {
        let args = command_args(false, vec![os("echo $0"), os("zero"), os("one")]);
        assert_eq!(args, [os("-c"), os("echo $0"), os("zero"), os("one")]);
    }

    #[test]
    fn a_login_adds_dash_l_in_front_and_nothing_else() {
        assert_eq!(
            command_args(true, vec![os("true")]),
            [os("-l"), os("-c"), os("true")]
        );
        assert_eq!(
            script_args(true, vec![os("s.sh"), os("a")]),
            [os("-l"), os("s.sh"), os("a")]
        );
        assert_eq!(stdin_args(true), [os("-l")]);
        assert!(stdin_args(false).is_empty());
    }

    #[test]
    fn operands_that_are_not_utf8_are_passed_on_unchanged() {
        let odd = OsString::from_vec(vec![b'a', 0xff, b'b']);
        let args = script_args(false, vec![odd.clone()]);
        assert_eq!(args, [odd]);
    }

    #[test]
    fn a_missing_shell_is_127_and_a_shell_that_cannot_run_is_126() {
        assert_eq!(status_for(&io::Error::from(io::ErrorKind::NotFound)), 127);
        assert_eq!(
            status_for(&io::Error::from(io::ErrorKind::PermissionDenied)),
            126
        );
    }
}
