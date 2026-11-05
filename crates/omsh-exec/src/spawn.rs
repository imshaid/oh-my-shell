//! Starting an external program and waiting for it. This is the one place that needs `unsafe`.
//!
//! The shell may have more than one thread (later segments add a runtime for AI and the
//! prompt). After `fork`, only the calling thread exists in the child, so a lock held by
//! another thread at that moment would never be released. The child therefore does nothing
//! except a few raw system calls that are safe in that situation ("async-signal-safe"), and
//! then `execve`: no allocation, no locks, no logging. The parent prepares every string and
//! pointer array before the fork.
//!
//! How does the parent learn that `execve` failed? A pipe whose write end closes by itself
//! when `execve` succeeds (`O_CLOEXEC`). If the exec fails, the child writes the error number
//! into the pipe and exits. The parent reads the pipe: end of file means the program started.

#![allow(unsafe_code, reason = "fork, execve and raw system calls need unsafe")]

use std::ffi::{CStr, CString, c_char, c_void};
use std::os::fd::{AsFd, AsRawFd, RawFd};

use nix::errno::Errno;
use nix::fcntl::OFlag;
use nix::libc;
use nix::sys::wait::{WaitStatus, waitpid};
use nix::unistd::{ForkResult, Pid, fork, pipe2, read};

use crate::status::Termination;

/// The shell used to run a file that has no `#!` line, like other shells do.
const SH: &CStr = c"/bin/sh";

/// A program to start.
pub struct ExecRequest<'a> {
    /// The file to run.
    pub path: &'a CStr,
    /// The arguments, starting with the program's own name.
    pub argv: &'a [CString],
    /// The environment, as `NAME=value` strings.
    pub envp: &'a [CString],
}

/// Why a program could not be run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpawnFailure {
    /// `fork` failed, for example because there are too many processes.
    Fork(Errno),
    /// `execve` failed in the child. The child has been reaped.
    Exec(Errno),
}

/// Runs a program in a child process and waits until it ends.
///
/// With `capture`, the program's standard output goes to a pipe that this function reads
/// while the program runs, and the bytes are added to the buffer. This is how `$(...)` gets
/// the output of its commands.
///
/// # Errors
///
/// Returns [`SpawnFailure`] if the child could not be created or the program could not be
/// started.
pub fn run(
    request: &ExecRequest<'_>,
    capture: Option<&mut Vec<u8>>,
) -> Result<Termination, SpawnFailure> {
    // Everything the child needs is made here, before the fork.
    let argv = pointers(request.argv);
    let envp = pointers(request.envp);
    let fallback: Vec<CString> = fallback_argv(request.path, request.argv);
    let fallback = pointers(&fallback);

    let (report_read, report_write) = pipe2(OFlag::O_CLOEXEC).map_err(SpawnFailure::Fork)?;
    let report_fd = report_write.as_raw_fd();
    let capture_pipe = if capture.is_some() {
        Some(pipe2(OFlag::O_CLOEXEC).map_err(SpawnFailure::Fork)?)
    } else {
        None
    };
    let stdout_fd = capture_pipe.as_ref().map_or(-1, |(_, w)| w.as_raw_fd());

    // SAFETY: the child only runs `exec_child`, which makes raw system calls that are
    // async-signal-safe and never returns. Nothing that could hold a lock or allocate runs
    // between `fork` and `execve`.
    let forked = unsafe { fork() }.map_err(SpawnFailure::Fork)?;
    match forked {
        ForkResult::Child => {
            exec_child(request.path, &argv, &envp, &fallback, report_fd, stdout_fd)
        }
        ForkResult::Parent { child } => {
            // Our copy of the write end must close, or the read below would never see the end.
            drop(report_write);
            let capture_read = capture_pipe.map(|(read_end, write_end)| {
                drop(write_end);
                read_end
            });
            let failure = read_report(&report_read);
            if let (Some(read_end), Some(buffer)) = (capture_read, capture) {
                drain(&read_end, buffer);
            }
            let ending = wait_for(child);
            match failure {
                Some(errno) => Err(SpawnFailure::Exec(errno)),
                None => Ok(ending),
            }
        }
    }
}

/// `["sh", path, args...]`: how `/bin/sh` is asked to run a file that is not a binary.
fn fallback_argv(path: &CStr, argv: &[CString]) -> Vec<CString> {
    let mut out = Vec::with_capacity(argv.len() + 1);
    out.push(CString::from(c"sh"));
    out.push(path.to_owned());
    out.extend(argv.iter().skip(1).cloned());
    out
}

/// A null-terminated array of pointers, the form that `execve` takes. The pointers borrow
/// from `strings`, which must outlive every use.
fn pointers(strings: &[CString]) -> Vec<*const c_char> {
    let mut out: Vec<*const c_char> = strings.iter().map(|s| s.as_ptr()).collect();
    out.push(std::ptr::null());
    out
}

/// The code that runs in the child. It never returns.
fn exec_child(
    path: &CStr,
    argv: &[*const c_char],
    envp: &[*const c_char],
    fallback: &[*const c_char],
    report_fd: RawFd,
    stdout_fd: RawFd,
) -> ! {
    // SAFETY: only async-signal-safe calls (`dup2`, `fcntl`, `signal`, `sigemptyset`,
    // `sigprocmask`, `execve`, `write`, `_exit`) on pointers that were made before the fork and stay valid, because the
    // parent's memory is copied into the child and nothing frees it here.
    unsafe {
        // The shell ignores SIGINT, SIGQUIT and (through Rust's runtime) SIGPIPE. Ignored
        // signals stay ignored across `execve`, so a program would not die on Ctrl+C or in
        // `yes | head`. Give them back their default meaning.
        libc::signal(libc::SIGINT, libc::SIG_DFL);
        libc::signal(libc::SIGQUIT, libc::SIG_DFL);
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
        let mut none: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&raw mut none);
        libc::sigprocmask(libc::SIG_SETMASK, &raw const none, std::ptr::null_mut());

        if stdout_fd >= 0 {
            // Put the pipe where the program writes its output. `dup2` clears close-on-exec
            // on the copy; if the pipe already is fd 1 there is no copy, so clear it by hand.
            let moved = if stdout_fd == 1 {
                libc::fcntl(1, libc::F_SETFD, 0)
            } else {
                libc::dup2(stdout_fd, 1)
            };
            if moved == -1 {
                let bytes = Errno::last_raw().to_ne_bytes();
                libc::write(report_fd, bytes.as_ptr().cast::<c_void>(), bytes.len());
                libc::_exit(126);
            }
        }

        libc::execve(path.as_ptr(), argv.as_ptr(), envp.as_ptr());
        let mut errno = Errno::last_raw();
        if errno == libc::ENOEXEC {
            // A text file without `#!`: let the system shell run it.
            libc::execve(SH.as_ptr(), fallback.as_ptr(), envp.as_ptr());
            errno = Errno::last_raw();
        }

        let bytes = errno.to_ne_bytes();
        libc::write(report_fd, bytes.as_ptr().cast::<c_void>(), bytes.len());
        let code = if errno == libc::ENOENT || errno == libc::ENOTDIR {
            127
        } else {
            126
        };
        libc::_exit(code)
    }
}

/// Reads from `pipe` until the other end is closed, adding everything to `buffer`.
fn drain(pipe: &impl AsFd, buffer: &mut Vec<u8>) {
    let mut chunk = [0_u8; 8192];
    loop {
        match read(pipe, &mut chunk) {
            Ok(0) | Err(Errno::EIO) => return,
            Ok(n) => buffer.extend_from_slice(&chunk[..n]),
            Err(Errno::EINTR) => {}
            Err(_) => return,
        }
    }
}

/// Reads what the child reported. Nothing means the program started.
fn read_report(pipe: &impl AsFd) -> Option<Errno> {
    let mut bytes = [0_u8; size_of::<i32>()];
    let mut filled = 0;
    while filled < bytes.len() {
        match read(pipe, &mut bytes[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(Errno::EINTR) => {}
            Err(_) => break,
        }
    }
    (filled == bytes.len()).then(|| Errno::from_raw(i32::from_ne_bytes(bytes)))
}

/// Waits until `child` is gone and says how it ended.
fn wait_for(child: Pid) -> Termination {
    loop {
        match waitpid(child, None) {
            Ok(status) => {
                if let Some(ending) = Termination::from_wait(status) {
                    return ending;
                }
                // Stopped or continued: keep waiting. Job control (S09) will handle these.
                debug_assert!(matches!(
                    status,
                    WaitStatus::Stopped(..) | WaitStatus::Continued(_) | WaitStatus::StillAlive
                ));
            }
            Err(Errno::EINTR) => {}
            // The child is already gone (for example, reaped by someone else).
            Err(_) => return Termination::Exited(255),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cstrings(items: &[&str]) -> Vec<CString> {
        items.iter().map(|s| CString::new(*s).unwrap()).collect()
    }

    fn run_program(path: &str, args: &[&str]) -> Result<Termination, SpawnFailure> {
        let path = CString::new(path).unwrap();
        let argv = cstrings(args);
        let envp = cstrings(&["PATH=/usr/bin:/bin"]);
        run(
            &ExecRequest {
                path: &path,
                argv: &argv,
                envp: &envp,
            },
            None,
        )
    }

    #[test]
    fn a_program_that_succeeds_ends_with_zero() {
        assert_eq!(
            run_program("/bin/sh", &["sh", "-c", "exit 0"]),
            Ok(Termination::Exited(0))
        );
    }

    #[test]
    fn the_exit_code_is_passed_on() {
        assert_eq!(
            run_program("/bin/sh", &["sh", "-c", "exit 42"]),
            Ok(Termination::Exited(42))
        );
    }

    #[test]
    fn a_killed_program_reports_the_signal() {
        assert_eq!(
            run_program("/bin/sh", &["sh", "-c", "kill -TERM $$"]),
            Ok(Termination::Signaled {
                signal: libc::SIGTERM,
                core_dumped: false
            })
        );
    }

    #[test]
    fn a_missing_file_reports_enoent() {
        assert_eq!(
            run_program("/nonexistent/omsh-test", &["x"]),
            Err(SpawnFailure::Exec(Errno::ENOENT))
        );
    }

    #[test]
    fn a_directory_reports_eacces() {
        assert_eq!(
            run_program("/tmp", &["x"]),
            Err(SpawnFailure::Exec(Errno::EACCES))
        );
    }

    #[test]
    fn the_environment_reaches_the_program() {
        let path = CString::new("/bin/sh").unwrap();
        let argv = cstrings(&["sh", "-c", "test \"$OMSH_TEST\" = yes"]);
        let envp = cstrings(&["OMSH_TEST=yes"]);
        let ending = run(
            &ExecRequest {
                path: &path,
                argv: &argv,
                envp: &envp,
            },
            None,
        );
        assert_eq!(ending, Ok(Termination::Exited(0)));
    }

    #[test]
    fn the_output_of_a_program_can_be_captured() {
        let path = CString::new("/bin/sh").unwrap();
        let argv = cstrings(&["sh", "-c", "printf 'one\ntwo'; echo err >&2"]);
        let envp = cstrings(&["PATH=/usr/bin:/bin"]);
        let mut out = Vec::new();
        let ending = run(
            &ExecRequest {
                path: &path,
                argv: &argv,
                envp: &envp,
            },
            Some(&mut out),
        );
        assert_eq!(ending, Ok(Termination::Exited(0)));
        assert_eq!(out, b"one\ntwo");
    }

    #[test]
    fn a_lot_of_captured_output_does_not_block_the_program() {
        // More than a pipe holds (64 KiB), so the program only finishes if we keep reading.
        let path = CString::new("/bin/sh").unwrap();
        let argv = cstrings(&["sh", "-c", "head -c 300000 /dev/zero"]);
        let envp = cstrings(&["PATH=/usr/bin:/bin"]);
        let mut out = Vec::new();
        let request = ExecRequest {
            path: &path,
            argv: &argv,
            envp: &envp,
        };
        assert_eq!(run(&request, Some(&mut out)), Ok(Termination::Exited(0)));
        assert_eq!(out.len(), 300_000);
    }

    #[test]
    fn a_program_that_cannot_start_captures_nothing() {
        let path = CString::new("/nonexistent/omsh-test").unwrap();
        let argv = cstrings(&["x"]);
        let mut out = Vec::new();
        let request = ExecRequest {
            path: &path,
            argv: &argv,
            envp: &[],
        };
        assert_eq!(
            run(&request, Some(&mut out)),
            Err(SpawnFailure::Exec(Errno::ENOENT))
        );
        assert!(out.is_empty());
    }
}
