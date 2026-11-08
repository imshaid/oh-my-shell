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
use nix::sys::wait::{Id, WaitPidFlag, WaitStatus, waitid, waitpid};
use nix::unistd::{ForkResult, Pid, fork, pipe2, read, setpgid};

use crate::io::FdPlan;
use crate::status::Termination;

/// The shell used to run a file that has no `#!` line, like other shells do.
const SH: &CStr = c"/bin/sh";

/// The process group a program joins, and whether it takes the terminal. See `jobs.rs`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChildGroup {
    /// The group to join. 0 means a new group with the program as its leader.
    pub pgid: i32,
    /// A descriptor of the terminal to give to the group, or -1 to leave the terminal alone.
    pub tty: RawFd,
}

/// A program to start.
#[derive(Clone, Copy)]
pub struct ExecRequest<'a> {
    /// The file to run.
    pub path: &'a CStr,
    /// The arguments, starting with the program's own name.
    pub argv: &'a [CString],
    /// The environment, as `NAME=value` strings.
    pub envp: &'a [CString],
    /// The directory to run in, if it is not the shell's own (a subshell that used `cd`).
    pub cwd: Option<&'a CStr>,
    /// The file creation mask to run with, if it is not the process's own (a subshell that
    /// used `umask`).
    pub umask: Option<u32>,
    /// Bit `n` is set for each signal `n` that is ignored by a `trap`, or was ignored when the
    /// shell started. The shell resets the signals it changes for itself (SIGINT, SIGQUIT,
    /// SIGTERM, SIGTSTP, SIGTTIN, SIGTTOU and SIGPIPE) to their defaults for a program, except
    /// for those: a signal that is ignored stays ignored in what the shell starts.
    pub ignored: u64,
    /// The process group to put the program in, if it is part of a job.
    pub group: Option<ChildGroup>,
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
/// # Errors
///
/// Returns [`SpawnFailure`] if the child could not be created or the program could not be
/// started.
pub fn run(request: &ExecRequest<'_>, plan: &mut FdPlan) -> Result<Termination, SpawnFailure> {
    let child = start(request, plan)?;
    Ok(wait(child))
}

/// Starts a program in a child process and returns once it is running (`execve` worked).
/// Call [`wait`] to collect it.
///
/// The child first puts the file descriptors in place (`plan`), then changes to the directory
/// in `request.cwd`, if any.
///
/// # Errors
///
/// Returns [`SpawnFailure`] if the child could not be created or the program could not be
/// started. In the second case the child has already been reaped.
pub fn start(request: &ExecRequest<'_>, plan: &mut FdPlan) -> Result<Pid, SpawnFailure> {
    // Everything the child needs is made here, before the fork.
    let argv = pointers(request.argv);
    let envp = pointers(request.envp);
    let fallback: Vec<CString> = fallback_argv(request.path, request.argv);
    let fallback = pointers(&fallback);

    let (report_read, report_write) = pipe2(OFlag::O_CLOEXEC).map_err(SpawnFailure::Fork)?;
    let report_fd = report_write.as_raw_fd();

    // SAFETY: the child only runs `exec_child`, which makes raw system calls that are
    // async-signal-safe and never returns. Nothing that could hold a lock or allocate runs
    // between `fork` and `execve`.
    let forked = unsafe { fork() }.map_err(SpawnFailure::Fork)?;
    match forked {
        ForkResult::Child => exec_child(
            &Prepared {
                path: request.path,
                argv: &argv,
                envp: &envp,
                fallback: &fallback,
                cwd: request.cwd,
                umask: request.umask,
                ignored: request.ignored,
                group: request.group,
            },
            report_fd,
            plan,
        ),
        ForkResult::Parent { child } => {
            // The child does this too. Whoever gets there first wins, and the other call is
            // harmless (it fails once the child has run `execve`, which we ignore). Doing it
            // on both sides is what makes the group exist before anything uses it (§20.1).
            if let Some(group) = request.group {
                let _ = setpgid(child, Pid::from_raw(group.pgid));
            }
            // Our copy of the write end must close, or the read below would never see the end.
            drop(report_write);
            match read_report(&report_read) {
                Some(errno) => {
                    wait(child);
                    Err(SpawnFailure::Exec(errno))
                }
                None => Ok(child),
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

/// Everything `execve` needs, made before the `fork`: the pointer arrays borrow from strings
/// that must stay alive.
struct Prepared<'a> {
    path: &'a CStr,
    argv: &'a [*const c_char],
    envp: &'a [*const c_char],
    /// The arguments for `/bin/sh` if the file is not a binary.
    fallback: &'a [*const c_char],
    cwd: Option<&'a CStr>,
    umask: Option<u32>,
    ignored: u64,
    group: Option<ChildGroup>,
}

/// The code that runs in the child. It never returns.
fn exec_child(prepared: &Prepared<'_>, report_fd: RawFd, plan: &mut FdPlan) -> ! {
    // SAFETY: `become_program` makes only async-signal-safe calls (see there), and `fail` only
    // `write` and `_exit`, on numbers and pointers made before the fork.
    unsafe {
        let errno = become_program(prepared, plan);
        fail(report_fd, errno)
    }
}

/// Puts the process in the state a program starts in, then runs the program. Returns the error
/// number if that failed; on success it does not return.
///
/// # Safety
///
/// The caller must be the child after `fork`, or the shell replacing itself with `exec`. The
/// pointers must stay valid. The calls made are `signal`, `sigemptyset`, `sigprocmask`,
/// `dup2`, `fcntl`, `close`, `chdir`, `umask`, `setpgid`, `getpgrp`, `tcsetpgrp` and
/// `execve`, all async-signal-safe, and the
/// only memory written is the plan's scratch array, which already has its final size.
unsafe fn become_program(prepared: &Prepared<'_>, plan: &mut FdPlan) -> i32 {
    // SAFETY: see the function's contract.
    unsafe {
        // First the process group and the terminal, while SIGTTOU is still ignored: a process
        // in the background group that calls `tcsetpgrp` would be stopped by it otherwise.
        // This comes before the file descriptors, which may overwrite the terminal's number.
        if let Some(group) = prepared.group {
            libc::setpgid(0, group.pgid);
            if group.tty >= 0 {
                libc::tcsetpgrp(group.tty, libc::getpgrp());
            }
        }
        // The shell ignores SIGINT, SIGQUIT, SIGTERM, SIGTSTP, SIGTTIN, SIGTTOU and (through
        // Rust's runtime) SIGPIPE. Ignored signals stay ignored across `execve`, so a program
        // would not die on Ctrl+C or in `yes | head`, or could not be stopped. Give them back
        // their default meaning. (The shell catches SIGINT; a caught signal is reset by
        // `execve` anyway, but it must not run our handler first.)
        for signal in [
            libc::SIGINT,
            libc::SIGQUIT,
            libc::SIGTERM,
            libc::SIGTSTP,
            libc::SIGTTIN,
            libc::SIGTTOU,
            libc::SIGPIPE,
        ] {
            if prepared.ignored & (1_u64 << signal) == 0 {
                libc::signal(signal, libc::SIG_DFL);
            }
        }
        let mut none: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&raw mut none);
        libc::sigprocmask(libc::SIG_SETMASK, &raw const none, std::ptr::null_mut());

        if let Err(errno) = apply_plan(plan) {
            return errno;
        }
        if let Some(dir) = prepared.cwd
            && libc::chdir(dir.as_ptr()) == -1
        {
            return Errno::last_raw();
        }
        if let Some(mask) = prepared.umask {
            libc::umask(mask as libc::mode_t);
        }

        libc::execve(
            prepared.path.as_ptr(),
            prepared.argv.as_ptr(),
            prepared.envp.as_ptr(),
        );
        let mut errno = Errno::last_raw();
        if errno == libc::ENOEXEC {
            // A text file without `#!`: let the system shell run it.
            libc::execve(
                SH.as_ptr(),
                prepared.fallback.as_ptr(),
                prepared.envp.as_ptr(),
            );
            errno = Errno::last_raw();
        }
        errno
    }
}

/// Replaces the running process with the program (`exec`), after putting the files of `plan`
/// in place. Returns the error number if that failed. The shell goes on in that case, with
/// whatever the plan had already changed.
///
/// Only the main shell may call this, when no other thread is running a command: `execve`
/// ends every other thread of the process.
pub fn replace_process(request: &ExecRequest<'_>, plan: &mut FdPlan) -> Errno {
    let argv = pointers(request.argv);
    let envp = pointers(request.envp);
    let fallback: Vec<CString> = fallback_argv(request.path, request.argv);
    let fallback = pointers(&fallback);
    // SAFETY: the pointers were made just above from strings that outlive the call, and the
    // calls inside are plain system calls, which are fine in a process of any number of threads.
    let errno = unsafe {
        become_program(
            &Prepared {
                path: request.path,
                argv: &argv,
                envp: &envp,
                fallback: &fallback,
                cwd: request.cwd,
                umask: request.umask,
                ignored: request.ignored,
                group: request.group,
            },
            plan,
        )
    };
    Errno::from_raw(errno)
}

/// Tells the parent why the program did not start, and ends the child.
///
/// # Safety
///
/// Only for the child after `fork`: it calls `write` and `_exit`.
unsafe fn fail(report_fd: RawFd, errno: i32) -> ! {
    let bytes = errno.to_ne_bytes();
    // SAFETY: `write` and `_exit` are async-signal-safe, and `bytes` lives until the call
    // returns.
    unsafe {
        libc::write(report_fd, bytes.as_ptr().cast::<c_void>(), bytes.len());
        let code = if errno == libc::ENOENT || errno == libc::ENOTDIR {
            127
        } else {
            126
        };
        libc::_exit(code)
    }
}

/// Puts the file descriptors of `plan` in place, in the child. Returns the error number of
/// the first call that failed.
///
/// # Safety
///
/// Only for the child after `fork`: it calls `fcntl`, `dup2` and `close` on the numbers in the
/// plan.
unsafe fn apply_plan(plan: &mut FdPlan) -> Result<(), i32> {
    // SAFETY: those calls are async-signal-safe and take plain numbers. The only memory
    // written is the plan's scratch array.
    unsafe {
        for (slot, (_, source)) in plan.scratch.iter_mut().zip(&plan.pairs) {
            let copy = libc::fcntl(*source, libc::F_DUPFD_CLOEXEC, plan.floor);
            if copy == -1 {
                return Err(Errno::last_raw());
            }
            *slot = copy;
        }
        for (copy, (target, _)) in plan.scratch.iter().zip(&plan.pairs) {
            if libc::dup2(*copy, *target) == -1 {
                return Err(Errno::last_raw());
            }
        }
        for target in &plan.closes {
            libc::close(*target);
        }
    }
    Ok(())
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
pub fn wait(child: Pid) -> Termination {
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

/// How a member of a job changed state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Waited {
    /// It is over.
    Ended(Termination),
    /// It was stopped (Ctrl+Z, `SIGSTOP`, or it read the terminal from the background).
    Stopped,
}

/// Waits until `child`, a member of a job, ends or stops.
///
/// With `anchor` set, an ending is only looked at (`WNOWAIT`): the process stays a zombie, so
/// its pid stays in use and the group it leads keeps existing. Later members of the job may
/// still have to join that group; see `jobs.rs`.
pub(crate) fn wait_member(child: Pid, anchor: bool) -> Waited {
    loop {
        let result = if anchor {
            waitid(
                Id::Pid(child),
                WaitPidFlag::WEXITED | WaitPidFlag::WSTOPPED | WaitPidFlag::WNOWAIT,
            )
        } else {
            waitpid(child, Some(WaitPidFlag::WUNTRACED))
        };
        match result {
            Ok(status) => {
                if let Some(ending) = Termination::from_wait(status) {
                    return Waited::Ended(ending);
                }
                if matches!(status, WaitStatus::Stopped(..)) {
                    if anchor {
                        // `WNOWAIT` left the report of the stop in place; take it, or the
                        // next wait would see the same stop again.
                        let _ =
                            waitid(Id::Pid(child), WaitPidFlag::WSTOPPED | WaitPidFlag::WNOHANG);
                    }
                    return Waited::Stopped;
                }
                // Continued: keep waiting.
            }
            Err(Errno::EINTR) => {}
            // The child is already gone (for example, reaped by someone else).
            Err(_) => return Waited::Ended(Termination::Exited(255)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::capture::Captured;

    fn cstrings(items: &[&str]) -> Vec<CString> {
        items.iter().map(|s| CString::new(*s).unwrap()).collect()
    }

    /// Runs a program with its output captured. Returns how it ended and what it printed.
    fn run_program(
        path: &str,
        args: &[&str],
        envp: &[&str],
    ) -> (Result<Termination, SpawnFailure>, String, String) {
        let path = CString::new(path).unwrap();
        let argv = cstrings(args);
        let envp = cstrings(envp);
        let captured = Captured::new();
        let mut plan = captured.io.plan();
        let ending = run(
            &ExecRequest {
                path: &path,
                argv: &argv,
                envp: &envp,
                cwd: None,
                umask: None,
                ignored: 0,
                group: None,
            },
            &mut plan,
        );
        (ending, captured.stdout(), captured.stderr())
    }

    const PATH: &[&str] = &["PATH=/usr/bin:/bin"];

    #[test]
    fn a_program_that_succeeds_ends_with_zero() {
        let (ending, ..) = run_program("/bin/sh", &["sh", "-c", "exit 0"], PATH);
        assert_eq!(ending, Ok(Termination::Exited(0)));
    }

    #[test]
    fn the_exit_code_is_passed_on() {
        let (ending, ..) = run_program("/bin/sh", &["sh", "-c", "exit 42"], PATH);
        assert_eq!(ending, Ok(Termination::Exited(42)));
    }

    #[test]
    fn a_killed_program_reports_the_signal() {
        let (ending, ..) = run_program("/bin/sh", &["sh", "-c", "kill -TERM $$"], PATH);
        assert_eq!(
            ending,
            Ok(Termination::Signaled {
                signal: libc::SIGTERM,
                core_dumped: false
            })
        );
    }

    #[test]
    fn a_missing_file_reports_enoent() {
        let (ending, ..) = run_program("/nonexistent/omsh-test", &["x"], PATH);
        assert_eq!(ending, Err(SpawnFailure::Exec(Errno::ENOENT)));
    }

    #[test]
    fn a_directory_reports_eacces() {
        let (ending, ..) = run_program("/tmp", &["x"], PATH);
        assert_eq!(ending, Err(SpawnFailure::Exec(Errno::EACCES)));
    }

    #[test]
    fn the_environment_reaches_the_program() {
        let (ending, ..) = run_program(
            "/bin/sh",
            &["sh", "-c", "test \"$OMSH_TEST\" = yes"],
            &["OMSH_TEST=yes"],
        );
        assert_eq!(ending, Ok(Termination::Exited(0)));
    }

    #[test]
    fn the_output_of_a_program_goes_where_the_table_says() {
        let (ending, out, err) = run_program(
            "/bin/sh",
            &["sh", "-c", "printf 'one\ntwo'; echo err >&2"],
            PATH,
        );
        assert_eq!(ending, Ok(Termination::Exited(0)));
        assert_eq!(out, "one\ntwo");
        assert_eq!(err, "err\n");
    }

    #[test]
    fn a_lot_of_output_does_not_block_the_program() {
        // More than a pipe holds (64 KiB). A memory file has no such limit, so this checks
        // that a big write works at all.
        let (ending, out, _) = run_program(
            "/bin/sh",
            &["sh", "-c", "head -c 300000 /dev/zero | tr '\\0' x"],
            PATH,
        );
        assert_eq!(ending, Ok(Termination::Exited(0)));
        assert_eq!(out.len(), 300_000);
    }

    #[test]
    fn a_closed_descriptor_is_closed_in_the_program() {
        let path = CString::new("/bin/sh").unwrap();
        let argv = cstrings(&["sh", "-c", "echo hi >&2"]);
        let envp = cstrings(&["PATH=/usr/bin:/bin"]);
        let mut captured = Captured::new();
        captured.io.close(2);
        let mut plan = captured.io.plan();
        let ending = run(
            &ExecRequest {
                path: &path,
                argv: &argv,
                envp: &envp,
                cwd: None,
                umask: None,
                ignored: 0,
                group: None,
            },
            &mut plan,
        );
        // The write to the closed descriptor fails, so `echo` reports a failure.
        assert_ne!(ending, Ok(Termination::Exited(0)));
    }

    #[test]
    fn swapping_two_descriptors_works() {
        // `3>&1 1>&2 2>&3` in the table: 1 and 2 trade places.
        let path = CString::new("/bin/sh").unwrap();
        let argv = cstrings(&["sh", "-c", "echo to-one; echo to-two >&2"]);
        let envp = cstrings(&["PATH=/usr/bin:/bin"]);
        let mut captured = Captured::new();
        let one = captured.io.get(1).unwrap().clone();
        let two = captured.io.get(2).unwrap().clone();
        captured.io.set(1, two);
        captured.io.set(2, one);
        let mut plan = captured.io.plan();
        let ending = run(
            &ExecRequest {
                path: &path,
                argv: &argv,
                envp: &envp,
                cwd: None,
                umask: None,
                ignored: 0,
                group: None,
            },
            &mut plan,
        );
        assert_eq!(ending, Ok(Termination::Exited(0)));
        assert_eq!(captured.stdout(), "to-two\n");
        assert_eq!(captured.stderr(), "to-one\n");
    }

    #[test]
    fn a_descriptor_above_two_reaches_the_program() {
        let path = CString::new("/bin/sh").unwrap();
        let argv = cstrings(&["sh", "-c", "echo via-three >&3"]);
        let envp = cstrings(&["PATH=/usr/bin:/bin"]);
        let mut captured = Captured::new();
        let one = captured.io.get(1).unwrap().clone();
        captured.io.set(3, one);
        let mut plan = captured.io.plan();
        let ending = run(
            &ExecRequest {
                path: &path,
                argv: &argv,
                envp: &envp,
                cwd: None,
                umask: None,
                ignored: 0,
                group: None,
            },
            &mut plan,
        );
        assert_eq!(ending, Ok(Termination::Exited(0)));
        assert_eq!(captured.stdout(), "via-three\n");
    }

    #[test]
    fn the_program_runs_in_the_given_directory() {
        let path = CString::new("/bin/sh").unwrap();
        let argv = cstrings(&["sh", "-c", "pwd -P"]);
        let envp = cstrings(&["PATH=/usr/bin:/bin"]);
        let captured = Captured::new();
        let mut plan = captured.io.plan();
        let ending = run(
            &ExecRequest {
                path: &path,
                argv: &argv,
                envp: &envp,
                cwd: Some(c"/usr"),
                umask: None,
                ignored: 0,
                group: None,
            },
            &mut plan,
        );
        assert_eq!(ending, Ok(Termination::Exited(0)));
        assert_eq!(captured.stdout(), "/usr\n");
    }

    #[test]
    fn a_missing_directory_is_reported_as_a_failure() {
        let path = CString::new("/bin/sh").unwrap();
        let argv = cstrings(&["sh"]);
        let captured = Captured::new();
        let mut plan = captured.io.plan();
        let ending = run(
            &ExecRequest {
                path: &path,
                argv: &argv,
                envp: &[],
                cwd: Some(c"/nonexistent-omsh-dir"),
                umask: None,
                ignored: 0,
                group: None,
            },
            &mut plan,
        );
        assert_eq!(ending, Err(SpawnFailure::Exec(Errno::ENOENT)));
    }

    #[test]
    fn a_program_runs_with_the_mask_it_is_given() {
        let path = CString::new("/bin/sh").unwrap();
        let argv = cstrings(&["sh", "-c", "umask"]);
        let envp = cstrings(&["PATH=/usr/bin:/bin"]);
        let captured = Captured::new();
        let mut plan = captured.io.plan();
        let ending = run(
            &ExecRequest {
                path: &path,
                argv: &argv,
                envp: &envp,
                cwd: None,
                umask: Some(0o027),
                ignored: 0,
                group: None,
            },
            &mut plan,
        );
        assert_eq!(ending, Ok(Termination::Exited(0)));
        assert_eq!(captured.stdout(), "0027\n");
    }

    /// The group of `/proc/self/stat` (field 5) as seen by the program, and the program's pid.
    const PRINT_GROUP: &str = "cut -d' ' -f5 /proc/self/stat";

    fn start_in(
        argv: &[&str],
        group: Option<ChildGroup>,
        ignored: u64,
        captured: &Captured,
    ) -> Pid {
        let path = CString::new("/bin/sh").unwrap();
        let argv = cstrings(argv);
        let envp = cstrings(&["PATH=/usr/bin:/bin"]);
        let mut plan = captured.io.plan();
        start(
            &ExecRequest {
                path: &path,
                argv: &argv,
                envp: &envp,
                cwd: None,
                umask: None,
                ignored,
                group,
            },
            &mut plan,
        )
        .unwrap()
    }

    #[test]
    fn a_program_with_group_zero_leads_a_group_of_its_own() {
        let captured = Captured::new();
        let child = start_in(
            &["sh", "-c", PRINT_GROUP],
            Some(ChildGroup { pgid: 0, tty: -1 }),
            0,
            &captured,
        );
        wait(child);
        assert_eq!(captured.stdout(), format!("{}\n", child.as_raw()));
        assert_ne!(child, nix::unistd::getpgrp());
    }

    #[test]
    fn a_program_can_join_the_group_of_another() {
        let captured = Captured::new();
        let leader = start_in(
            &["sh", "-c", "sleep 5"],
            Some(ChildGroup { pgid: 0, tty: -1 }),
            0,
            &captured,
        );
        let member = start_in(
            &["sh", "-c", PRINT_GROUP],
            Some(ChildGroup {
                pgid: leader.as_raw(),
                tty: -1,
            }),
            0,
            &captured,
        );
        wait(member);
        let _ = nix::sys::signal::kill(leader, nix::sys::signal::Signal::SIGKILL);
        wait(leader);
        assert_eq!(captured.stdout(), format!("{}\n", leader.as_raw()));
    }

    #[test]
    fn a_waiting_leader_stays_a_zombie_until_it_is_collected() {
        let captured = Captured::new();
        let leader = start_in(
            &["sh", "-c", "exit 3"],
            Some(ChildGroup { pgid: 0, tty: -1 }),
            0,
            &captured,
        );
        // Looked at, not collected: the pid is still in use and the group still exists.
        assert_eq!(
            wait_member(leader, true),
            Waited::Ended(Termination::Exited(3))
        );
        assert_eq!(
            wait_member(leader, true),
            Waited::Ended(Termination::Exited(3))
        );
        let group = nix::sys::signal::killpg(leader, None);
        assert!(group.is_ok(), "the group is gone: {group:?}");
        assert_eq!(wait(leader), Termination::Exited(3));
    }

    /// The signals a program ignores, as the bit mask in `/proc/self/status`.
    fn ignored_by_program(ignored: u64) -> u64 {
        let captured = Captured::new();
        let child = start_in(
            &["sh", "-c", "grep SigIgn /proc/self/status"],
            None,
            ignored,
            &captured,
        );
        wait(child);
        let text = captured.stdout();
        let hex = text.split_whitespace().last().unwrap_or("0");
        u64::from_str_radix(hex, 16).unwrap()
    }

    #[test]
    fn the_signals_the_shell_ignores_are_reset_for_a_program() {
        use nix::sys::signal::{SigHandler, Signal, signal};
        // SIGTTIN is the one signal no other test in this process looks at.
        // SAFETY: `SigIgn` installs no handler.
        let before = unsafe { signal(Signal::SIGTTIN, SigHandler::SigIgn) }.unwrap();
        let bit = 1_u64 << (libc::SIGTTIN - 1);
        let reset = ignored_by_program(0);
        let kept = ignored_by_program(1_u64 << libc::SIGTTIN);
        // SAFETY: puts back what was there before.
        unsafe { signal(Signal::SIGTTIN, before) }.unwrap();
        assert_eq!(reset & bit, 0, "SIGTTIN stayed ignored");
        assert_eq!(kept & bit, bit, "an ignored signal was reset");
    }
}
