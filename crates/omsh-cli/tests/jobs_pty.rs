//! Job control tests that need a real terminal: the shell runs on a pseudo-terminal that is its
//! controlling terminal, and the test types at it.
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
