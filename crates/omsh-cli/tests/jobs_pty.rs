//! Job control tests that need a real terminal: the shell runs on a pseudo-terminal that is its
//! controlling terminal, and the test types at it.
//!
//! The tests for stopped jobs (Ctrl+Z, `fg`, `bg`, the warning at exit, `SIGHUP`) are here too.
//!
//! This is a small first piece of the PTY harness that S10 builds. Until S10 gives it timeouts
//! everywhere, this file is strict about its own: every read has a deadline, and the session
//! kills every process of the shell's session when it is dropped, whatever happened.
//!
//! `setsid -c` (util-linux) starts the shell as the leader of a new session and makes the
//! pseudo-terminal its controlling terminal, which is what a terminal emulator does.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::AsFd;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use nix::fcntl::{FcntlArg, FdFlag, fcntl};
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::pty::openpty;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;

const BINARY: &str = env!("CARGO_BIN_EXE_oh-my-shell");

/// How long to wait for something the shell should do at once.
const SHORT: Duration = Duration::from_secs(10);

struct Session {
    master: File,
    shell: Child,
    /// Everything the terminal has shown so far.
    transcript: String,
    /// Where the next search starts, so that old text cannot match.
    cursor: usize,
}

impl Session {
    fn start() -> io::Result<Self> {
        let pty = openpty(None, None).map_err(io::Error::from)?;
        // The shell must not inherit the master side, or the terminal would never hang up.
        fcntl(&pty.master, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC)).map_err(io::Error::from)?;
        let shell = Command::new("setsid")
            .arg("-c")
            .arg(BINARY)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", "/tmp")
            .env("LANG", "C")
            .env("TERM", "dumb")
            .stdin(Stdio::from(pty.slave.try_clone()?))
            .stdout(Stdio::from(pty.slave.try_clone()?))
            .stderr(Stdio::from(pty.slave))
            .spawn()?;
        let mut session = Self {
            master: File::from(pty.master),
            shell,
            transcript: String::new(),
            cursor: 0,
        };
        session.expect("$ ", SHORT);
        Ok(session)
    }

    /// Reads whatever the terminal shows for up to `wait`.
    fn pump(&mut self, wait: Duration) {
        let end = Instant::now() + wait;
        let mut buffer = [0_u8; 4096];
        loop {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return;
            }
            let step = PollTimeout::try_from(left.min(Duration::from_millis(50)))
                .unwrap_or(PollTimeout::from(50_u16));
            let mut fds = [PollFd::new(self.master.as_fd(), PollFlags::POLLIN)];
            match poll(&mut fds, step) {
                Ok(n) if n > 0 => match self.master.read(&mut buffer) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => self
                        .transcript
                        .push_str(&String::from_utf8_lossy(&buffer[..n])),
                },
                _ => {}
            }
        }
    }

    /// Waits until `needle` has appeared after the cursor. Panics, with everything shown so far,
    /// if it does not within `limit`.
    fn expect(&mut self, needle: &str, limit: Duration) {
        if !self.try_expect(needle, limit) {
            panic!(
                "never saw {needle:?}; the terminal showed:\n{}",
                self.transcript
            );
        }
    }

    fn try_expect(&mut self, needle: &str, limit: Duration) -> bool {
        let end = Instant::now() + limit;
        loop {
            if let Some(at) = self.transcript[self.cursor..].find(needle) {
                self.cursor += at + needle.len();
                return true;
            }
            if Instant::now() >= end {
                return false;
            }
            self.pump(Duration::from_millis(50));
        }
    }

    /// Types `text` and a line ending.
    fn line(&mut self, text: &str) -> io::Result<()> {
        self.master.write_all(text.as_bytes())?;
        self.master.write_all(b"\n")
    }

    /// Types one raw byte, such as Ctrl+C (3).
    fn key(&mut self, byte: u8) -> io::Result<()> {
        self.master.write_all(&[byte])
    }

    /// Runs `echo <tag>-$((1+1))` and waits for its output `<tag>-2`. The echoed command line
    /// holds `$((1+1))` and not `2`, so only the output matches.
    fn sync(&mut self, tag: &str) -> io::Result<()> {
        self.line(&format!("echo {tag}-$((1+1))"))?;
        self.expect(&format!("{tag}-2"), SHORT);
        Ok(())
    }

    /// Sends `command` again and again until the terminal shows `needle`. For things that happen
    /// a moment after the command that causes them.
    fn repeat_until(&mut self, command: &str, needle: &str) -> io::Result<()> {
        let end = Instant::now() + SHORT;
        while Instant::now() < end {
            self.line(command)?;
            if self.try_expect(needle, Duration::from_millis(300)) {
                return Ok(());
            }
        }
        panic!(
            "never saw {needle:?} after repeating {command:?}; the terminal showed:\n{}",
            self.transcript
        );
    }

    /// The processes that are in this session and have this name.
    fn in_session(&self, name: &str) -> Vec<Pid> {
        session_members(self.shell.id())
            .into_iter()
            .filter(|(_, comm)| comm == name)
            .map(|(pid, _)| pid)
            .collect()
    }
}

/// The pids and names of the processes whose session is `session`, from `/proc`.
fn session_members(session: u32) -> Vec<(Pid, String)> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return found;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<i32>().ok())
        else {
            continue;
        };
        let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        // "pid (name) state ppid pgrp session ...": the name may hold spaces and parentheses,
        // so the fields are counted from the last closing parenthesis.
        let (Some(open), Some(close)) = (stat.find('('), stat.rfind(')')) else {
            continue;
        };
        let name = stat[open + 1..close].to_owned();
        let fields: Vec<&str> = stat[close + 1..].split_whitespace().collect();
        if fields.get(3).and_then(|s| s.parse::<u32>().ok()) == Some(session) {
            found.push((Pid::from_raw(pid), name));
        }
    }
    found
}

impl Drop for Session {
    fn drop(&mut self) {
        // Twice, in case a process was forked while the first pass ran.
        for _ in 0..2 {
            for (pid, _) in session_members(self.shell.id()) {
                let _ = kill(pid, Signal::SIGKILL);
            }
        }
        let _ = self.shell.kill();
        let _ = self.shell.wait();
    }
}

/// `cat &` reads the terminal from the background and is stopped by `SIGTTIN`. The shell notes
/// it as stopped, and `kill %1` ends it although a stopped process would hold the `SIGTERM`.
#[test]
fn kill_ends_a_background_job_that_the_terminal_stopped() -> io::Result<()> {
    let mut term = Session::start()?;
    term.line("cat &")?;
    term.expect("[1] ", SHORT);

    // The notice before a prompt, and `jobs`, say that it stopped for the terminal.
    term.repeat_until("jobs", "Stopped (tty input)")?;
    assert_eq!(term.in_session("cat").len(), 1, "{}", term.transcript);

    term.line("kill %1")?;
    // The job's thread sees `cat` die and the next prompt tells the user.
    term.repeat_until("echo gone-$((1+1))", "Terminated")?;

    // Nothing is left: not the process, and not a zombie waiting to be collected.
    let end = Instant::now() + SHORT;
    while !term.in_session("cat").is_empty() && Instant::now() < end {
        term.pump(Duration::from_millis(50));
    }
    assert!(
        term.in_session("cat").is_empty(),
        "cat is still there\n{}",
        term.transcript
    );

    term.line("exit")?;
    let end = Instant::now() + SHORT;
    while term.shell.try_wait()?.is_none() {
        assert!(Instant::now() < end, "the shell did not exit");
        term.pump(Duration::from_millis(50));
    }
    Ok(())
}

/// Ctrl+C goes to the foreground job. A background program and the shell code of a background
/// job (a loop run on a thread of the shell) must not feel it.
#[test]
fn ctrl_c_stops_the_foreground_job_and_not_the_background_ones() -> io::Result<()> {
    let mut term = Session::start()?;
    term.line("sleep 30 &")?;
    term.expect("[1] ", SHORT);
    term.line("while :; do :; done &")?;
    term.expect("[2]", SHORT);

    term.line("sleep 20")?;
    // Give `sleep 20` time to start, then press Ctrl+C.
    term.pump(Duration::from_millis(400));
    term.key(3)?;
    // The shell is alive and ready for the next command... (The loop above checks the Ctrl+C
    // flag thousands of times a millisecond, so it would see it in the short moment it is set.)
    term.sync("alive")?;
    // ... and both background jobs are still running, also after the loop had time to notice a
    // flag that it should not have seen.
    term.pump(Duration::from_millis(400));
    term.line("jobs")?;
    term.expect("sleep 30 &", SHORT);
    term.expect("while :; do :; done &", SHORT);
    assert!(
        !term.transcript[term.cursor.saturating_sub(300)..].contains("Done"),
        "a background job ended\n{}",
        term.transcript
    );
    assert!(
        !term.transcript.contains("Exit 130"),
        "a background job was interrupted\n{}",
        term.transcript
    );

    // A background job that dies of `SIGINT` sent by `kill` is not a Ctrl+C: the commands after
    // it on the same line still run. (If its death set the Ctrl+C flag, `echo` would be skipped.)
    term.line("kill -INT %1; wait %1; echo after-$((1+1))")?;
    term.expect("after-2", SHORT);

    // The terminal is still owned by the shell: `kill` and `wait` work.
    term.line("kill %2")?;
    term.line("wait")?;
    term.sync("waited")?;
    term.line("exit")?;
    let end = Instant::now() + SHORT;
    while term.shell.try_wait()?.is_none() {
        assert!(Instant::now() < end, "the shell did not exit");
        term.pump(Duration::from_millis(50));
    }
    Ok(())
}

// ---------------------------------------------------------------------- stopped jobs

const CTRL_Z: u8 = 0x1a;

/// True if this test process ignores `SIGHUP` (it was started under `nohup`). The shell and its
/// programs inherit that, and an ignored hangup stays ignored, so the tests that send one cannot
/// work there.
fn hup_is_ignored() -> bool {
    fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find_map(|line| line.strip_prefix("SigIgn:"))
                .and_then(|mask| u64::from_str_radix(mask.trim(), 16).ok())
        })
        .is_some_and(|mask| mask & 1 != 0)
}

/// Fields of `/proc/<pid>/stat` after the name: state, parent, process group, session, tty,
/// foreground group of the tty.
fn stat_fields(pid: u32) -> Vec<String> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
    stat.rfind(')')
        .map(|close| {
            stat[close + 1..]
                .split_whitespace()
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// The process group that owns the terminal of the process `pid` (the shell).
fn foreground_group(pid: u32) -> i32 {
    stat_fields(pid)
        .get(5)
        .and_then(|field| field.parse().ok())
        .unwrap_or(-1)
}

fn process_group(pid: u32) -> i32 {
    stat_fields(pid)
        .get(2)
        .and_then(|field| field.parse().ok())
        .unwrap_or(-1)
}

/// `T` for a stopped process, `S` or `R` for one that runs.
fn state(pid: Pid) -> String {
    stat_fields(pid.as_raw() as u32)
        .first()
        .cloned()
        .unwrap_or_default()
}

impl Session {
    fn shell_pid(&self) -> u32 {
        self.shell.id()
    }

    /// Types `exit` and waits for the shell to end.
    fn quit(&mut self) -> io::Result<()> {
        self.line("exit")?;
        let end = Instant::now() + SHORT;
        while self.shell.try_wait()?.is_none() {
            assert!(
                Instant::now() < end,
                "the shell did not exit\n{}",
                self.transcript
            );
            self.pump(Duration::from_millis(50));
        }
        Ok(())
    }

    /// Waits until no process of the session has this name.
    fn expect_none(&mut self, name: &str) {
        let end = Instant::now() + SHORT;
        while !self.in_session(name).is_empty() && Instant::now() < end {
            self.pump(Duration::from_millis(50));
        }
        assert!(
            self.in_session(name).is_empty(),
            "{name} is still there\n{}",
            self.transcript
        );
    }
}

/// Ctrl+Z stops the foreground job and the shell gets the terminal back; `fg` gives it to the
/// job again; `bg` lets the job run without it.
#[test]
fn ctrl_z_stops_the_job_and_fg_and_bg_continue_it() -> io::Result<()> {
    let mut term = Session::start()?;
    let shell = term.shell_pid();
    term.line("sleep 30")?;
    term.pump(Duration::from_millis(400));
    let sleep = term.in_session("sleep");
    assert_eq!(sleep.len(), 1, "{}", term.transcript);
    let sleep = sleep[0];
    // While it runs, the job owns the terminal.
    assert_eq!(foreground_group(shell), sleep.as_raw());

    term.key(CTRL_Z)?;
    term.expect("Stopped", SHORT);
    term.expect("sleep 30", SHORT);
    term.expect("$ ", SHORT);
    // The shell is back in front, and the program is still there, stopped.
    term.sync("alive")?;
    assert_eq!(foreground_group(shell), process_group(shell));
    assert_eq!(state(sleep), "T");
    // The notice came once, with the Ctrl+Z; the next prompt did not repeat it.
    assert_eq!(
        term.transcript.matches("Stopped").count(),
        1,
        "{}",
        term.transcript
    );
    term.line("jobs")?;
    term.expect("Stopped", SHORT);

    // `fg`: the job runs, in front.
    term.line("fg")?;
    term.expect("sleep 30", SHORT);
    term.pump(Duration::from_millis(300));
    assert_ne!(state(sleep), "T");
    assert_eq!(foreground_group(shell), sleep.as_raw());

    // It can be stopped a second time, and `bg` makes it run in the background.
    term.key(CTRL_Z)?;
    term.expect("Stopped", SHORT);
    term.sync("again")?;
    term.line("bg")?;
    term.expect("[1]+ sleep 30 &", SHORT);
    term.sync("bg-done")?;
    assert_eq!(foreground_group(shell), process_group(shell));
    assert_ne!(state(sleep), "T");
    term.line("jobs")?;
    term.expect("Running", SHORT);

    term.line("kill %1")?;
    term.repeat_until("echo gone-$((1+1))", "Terminated")?;
    term.expect_none("sleep");
    term.quit()
}

/// Ctrl+C reaches a job that was stopped and continued with `fg`, and only it.
#[test]
fn ctrl_c_ends_a_job_that_was_continued_with_fg() -> io::Result<()> {
    let mut term = Session::start()?;
    term.line("sleep 30")?;
    term.pump(Duration::from_millis(400));
    term.key(CTRL_Z)?;
    term.expect("Stopped", SHORT);
    term.sync("alive")?;
    // Ctrl+C also ends the rest of the line, as it does for a job that never stopped.
    term.line("fg; echo printed-$((1+1))")?;
    term.pump(Duration::from_millis(300));
    term.key(3)?;
    term.sync("after-int")?;
    assert!(
        !term.transcript.contains("printed-2"),
        "the line went on after Ctrl+C\n{}",
        term.transcript
    );
    term.expect_none("sleep");
    term.line("jobs")?;
    term.sync("empty")?;
    assert!(!term.transcript[term.cursor.saturating_sub(40)..].contains("sleep"));
    term.quit()
}

/// A pipeline and a subshell are stopped as one job, with every program in it.
#[test]
fn a_pipeline_and_a_subshell_stop_as_one_job() -> io::Result<()> {
    let mut term = Session::start()?;
    term.line("sleep 30 | cat")?;
    term.pump(Duration::from_millis(500));
    term.key(CTRL_Z)?;
    term.expect("Stopped", SHORT);
    term.sync("one")?;
    assert_eq!(term.in_session("sleep").len(), 1);
    assert_eq!(term.in_session("cat").len(), 1);
    let (sleep, cat) = (term.in_session("sleep")[0], term.in_session("cat")[0]);
    let end = Instant::now() + SHORT;
    while (state(sleep) != "T" || state(cat) != "T") && Instant::now() < end {
        term.pump(Duration::from_millis(50));
    }
    assert_eq!((state(sleep), state(cat)), ("T".to_owned(), "T".to_owned()));
    term.line("jobs")?;
    term.expect("sleep 30 | cat", SHORT);
    term.line("fg")?;
    term.pump(Duration::from_millis(300));
    term.key(3)?;
    term.sync("two")?;
    term.expect_none("sleep");
    term.expect_none("cat");

    // `( ... )` is one job too; `fg` goes on inside it.
    term.line("( sleep 30; echo subshell-finished )")?;
    term.pump(Duration::from_millis(500));
    term.key(CTRL_Z)?;
    term.expect("Stopped", SHORT);
    term.sync("three")?;
    term.line("jobs")?;
    term.expect("( sleep 30; echo subshell-finished )", SHORT);
    term.line("kill -CONT %1")?;
    term.line("kill %1")?;
    term.line("wait %1")?;
    term.sync("four")?;
    term.expect_none("sleep");
    term.quit()
}

/// The terminal settings of a job are kept when it stops and given back by `fg`: the shell has
/// its own meanwhile.
#[test]
fn a_stopped_job_keeps_its_own_terminal_settings() -> io::Result<()> {
    // Prints `flag=echo` or `flag=-echo`: the state of the echo flag of the terminal. (The marker
    // keeps it apart from the command lines, which the terminal shows too.)
    const ECHO_FLAG: &str =
        "echo flag=$(stty -a | tr -s \" ;\" \"\\n\" | grep -x -e echo -e -echo)";
    let mut term = Session::start()?;
    // The job turns echo off, stops itself, and reports the flag when it is continued.
    term.line(&format!(
        "sh -c 'stty -echo; kill -STOP $$; {ECHO_FLAG}; sleep 0.2'"
    ))?;
    term.expect("Stopped", SHORT);
    term.expect("$ ", SHORT);
    // At the prompt the shell's own settings are back: echo is on.
    term.line(ECHO_FLAG)?;
    term.expect("flag=echo", SHORT);
    // `fg` gives the job its settings back: it sees echo off.
    term.line("fg")?;
    term.expect("flag=-echo", SHORT);
    term.sync("done")?;
    term.line("stty echo")?;
    term.quit()
}

/// `exit` with a stopped job warns once; the second `exit` goes through and the stopped job is
/// told to hang up, so nothing is left behind.
#[test]
fn exit_warns_about_stopped_jobs_once_and_then_hangs_them_up() -> io::Result<()> {
    if hup_is_ignored() {
        eprintln!("skipped: SIGHUP is ignored here (started under nohup?)");
        return Ok(());
    }
    let mut term = Session::start()?;
    term.line("sleep 30")?;
    term.pump(Duration::from_millis(400));
    term.key(CTRL_Z)?;
    term.expect("Stopped", SHORT);
    term.sync("ready")?;

    term.line("exit")?;
    term.expect("There are stopped jobs.", SHORT);
    assert!(term.shell.try_wait()?.is_none(), "the shell ended");
    assert_eq!(term.in_session("sleep").len(), 1);

    // Anything else in between brings the warning back.
    term.sync("between")?;
    term.line("exit")?;
    term.expect("There are stopped jobs.", SHORT);
    // End of input (Ctrl+D) at an empty prompt is the same attempt again.
    term.key(4)?;
    let end = Instant::now() + SHORT;
    while term.shell.try_wait()?.is_none() {
        assert!(
            Instant::now() < end,
            "the shell did not exit\n{}",
            term.transcript
        );
        term.pump(Duration::from_millis(50));
    }
    term.expect_none("sleep");
    Ok(())
}

/// `SIGHUP` sent to an idle shell ends it with status 129 and takes its jobs along, stopped or
/// not.
#[test]
fn sighup_ends_the_shell_and_its_jobs() -> io::Result<()> {
    if hup_is_ignored() {
        eprintln!("skipped: SIGHUP is ignored here (started under nohup?)");
        return Ok(());
    }
    let mut term = Session::start()?;
    term.line("sleep 100 &")?;
    term.expect("[1] ", SHORT);
    term.line("sleep 101")?;
    term.pump(Duration::from_millis(400));
    term.key(CTRL_Z)?;
    term.expect("Stopped", SHORT);
    term.sync("ready")?;
    assert_eq!(term.in_session("sleep").len(), 2);

    kill(Pid::from_raw(term.shell.id() as i32), Signal::SIGHUP).map_err(io::Error::from)?;
    let end = Instant::now() + SHORT;
    let status = loop {
        if let Some(status) = term.shell.try_wait()? {
            break status;
        }
        assert!(
            Instant::now() < end,
            "the shell did not end\n{}",
            term.transcript
        );
        term.pump(Duration::from_millis(50));
    };
    assert_eq!(status.code(), Some(129), "{}", term.transcript);
    term.expect_none("sleep");
    Ok(())
}

/// A job disowned with `-h` is left alone when the shell is hung up.
#[test]
fn disown_h_protects_a_job_from_the_hangup() -> io::Result<()> {
    if hup_is_ignored() {
        eprintln!("skipped: SIGHUP is ignored here (started under nohup?)");
        return Ok(());
    }
    let mut term = Session::start()?;
    term.line("sleep 100 &")?;
    term.expect("[1] ", SHORT);
    term.line("disown -h")?;
    term.sync("ready")?;
    kill(Pid::from_raw(term.shell.id() as i32), Signal::SIGHUP).map_err(io::Error::from)?;
    let end = Instant::now() + SHORT;
    while term.shell.try_wait()?.is_none() {
        assert!(Instant::now() < end, "the shell did not end");
        term.pump(Duration::from_millis(50));
    }
    term.pump(Duration::from_millis(300));
    assert_eq!(term.in_session("sleep").len(), 1, "{}", term.transcript);
    Ok(())
}
