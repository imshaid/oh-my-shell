//! A program running on a pseudo-terminal, and a test that types at it.
//!
//! `setsid -c` (util-linux) starts the program as the leader of a new session and makes the
//! pseudo-terminal its controlling terminal, which is what a terminal emulator does.
//!
//! Nothing here can wait for ever. The master side of the terminal is non-blocking, reads and
//! writes have deadlines, and a failed wait panics with everything the terminal showed, which is
//! what one needs to find out why. [`crate::pty_test`] adds a limit for the whole test.

use std::fs::File;
use std::io::{self, ErrorKind, Read, Write};
use std::os::fd::AsFd;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use nix::fcntl::{FcntlArg, FdFlag, OFlag, fcntl};
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::pty::openpty;
use nix::sys::signal::{Signal, kill};
use nix::sys::termios::{Termios, tcgetattr};
use nix::unistd::{Pid, tcgetpgrp};

use crate::procfs::{self, Member};

/// How long to wait for something the shell should do at once.
pub const SHORT: Duration = Duration::from_secs(10);

/// The session ids of every terminal a test has started, so that the watchdog can clean up after
/// a test whose thread is stuck.
pub type Registry = Arc<Mutex<Vec<u32>>>;

/// How to start a program on a terminal.
#[derive(Debug, Clone)]
pub struct Launch {
    bin: String,
    args: Vec<String>,
    under_sh: bool,
    env: Vec<(String, String)>,
    cwd: Option<PathBuf>,
    registry: Option<Registry>,
}

impl Launch {
    pub fn new(bin: &str) -> Self {
        Self {
            bin: bin.to_owned(),
            args: Vec::new(),
            under_sh: false,
            env: vec![
                ("PATH".into(), "/usr/bin:/bin".into()),
                ("HOME".into(), "/tmp".into()),
                ("LANG".into(), "C".into()),
                ("TERM".into(), "dumb".into()),
            ],
            cwd: None,
            registry: None,
        }
    }

    pub(crate) fn registered(mut self, registry: Registry) -> Self {
        self.registry = Some(registry);
        self
    }

    pub fn arg(mut self, arg: &str) -> Self {
        self.args.push(arg.to_owned());
        self
    }

    /// Runs the program as a child of `sh -c`. When the program ends the parent prints
    /// `parent-saw-<status>`, then reads one line from the terminal and prints
    /// `parent-read-ok`. So the program is neither the leader of the session nor of its process
    /// group, as when a user starts it from another shell, and the parent shows what it saw.
    ///
    /// The parent does no job control, so it reads from a process group that must own the
    /// terminal: if the program left the terminal to a group that is gone, the read stops with
    /// `SIGTTIN` and `parent-read-ok` never comes. The parent waits for that line; type one.
    pub fn under_sh(mut self) -> Self {
        self.under_sh = true;
        self
    }

    /// Sets (or replaces) an environment variable. The environment starts with only `PATH`,
    /// `HOME`, `LANG` and `TERM`.
    pub fn env(mut self, key: &str, value: &str) -> Self {
        self.env.retain(|(k, _)| k != key);
        self.env.push((key.to_owned(), value.to_owned()));
        self
    }

    pub fn cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }

    /// Starts the program and waits for the shell's first prompt, `$ `.
    pub fn start(self) -> io::Result<Session> {
        let mut session = self.start_raw()?;
        session.expect("$ ", SHORT);
        Ok(session)
    }

    /// Starts the program and does not wait for anything: for programs that are not the shell.
    pub fn start_raw(self) -> io::Result<Session> {
        let pty = openpty(None, None).map_err(io::Error::from)?;
        // The program must not inherit the master side, or the terminal would never hang up.
        fcntl(&pty.master, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC)).map_err(io::Error::from)?;
        // Only our side is non-blocking (it is a separate open file from the slave).
        fcntl(&pty.master, FcntlArg::F_SETFL(OFlag::O_NONBLOCK)).map_err(io::Error::from)?;

        let mut command = Command::new("setsid");
        command.arg("-c");
        if self.under_sh {
            // `$0` is the program, `$@` its arguments: nothing needs quoting.
            command.args([
                "sh",
                "-c",
                "\"$0\" \"$@\"; echo parent-saw-$?; read _ack; echo parent-read-ok",
            ]);
        }
        command.arg(&self.bin).args(&self.args);
        command
            .env_clear()
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::from(pty.slave.try_clone()?))
            .stdout(Stdio::from(pty.slave.try_clone()?))
            .stderr(Stdio::from(pty.slave));
        if let Some(dir) = &self.cwd {
            command.current_dir(dir);
        }
        let shell = command.spawn()?;
        // Drop the command now: it holds the slave side, which must not stay open here.
        drop(command);
        if let Some(registry) = &self.registry {
            registry
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(shell.id());
        }
        Ok(Session {
            master: File::from(pty.master),
            shell,
            under_sh: self.under_sh,
            transcript: String::new(),
            cursor: 0,
        })
    }
}

pub struct Session {
    master: File,
    shell: Child,
    under_sh: bool,
    /// Everything the terminal has shown so far.
    transcript: String,
    /// Where the next search starts, so that old text cannot match.
    cursor: usize,
}

impl Session {
    /// Starts the program with the usual environment and waits for the shell's prompt.
    pub fn start(bin: &str) -> io::Result<Self> {
        Launch::new(bin).start()
    }

    /// The process that `setsid` started: the program itself, or the `sh` around it for
    /// [`Launch::under_sh`]. Its pid is the id of the session.
    pub fn parent_pid(&self) -> u32 {
        self.shell.id()
    }

    /// The pid of the shell under test. (With [`Launch::under_sh`] it is a child of the parent;
    /// it is found by name, so ask while it is the shell and not after an `exec`.)
    pub fn shell_pid(&self) -> u32 {
        if self.under_sh {
            self.in_session("omsh-pty-shell")
                .first()
                .map_or_else(|| self.shell.id(), |pid| pid.as_raw() as u32)
        } else {
            self.shell.id()
        }
    }

    /// Everything the terminal has shown so far.
    pub fn transcript(&self) -> &str {
        &self.transcript
    }

    /// The text that was shown after the last successful `expect`.
    pub fn since_cursor(&self) -> &str {
        &self.transcript[self.cursor..]
    }

    /// Up to `before` bytes of what was shown before the cursor, and everything after it.
    pub fn recent(&self, before: usize) -> &str {
        let mut start = self.cursor.saturating_sub(before);
        while !self.transcript.is_char_boundary(start) {
            start -= 1;
        }
        &self.transcript[start..]
    }

    /// Reads whatever the terminal shows for up to `wait`. Also what other methods use as a
    /// pause, so a pause never leaves the output unread.
    pub fn pump(&mut self, wait: Duration) {
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
            if poll(&mut fds, step).is_ok_and(|n| n > 0) {
                match self.master.read(&mut buffer) {
                    Ok(n) if n > 0 => self
                        .transcript
                        .push_str(&String::from_utf8_lossy(&buffer[..n])),
                    Err(e)
                        if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {}
                    // The other side hung up: nothing more will come. Poll would return at once
                    // from now on, so wait here instead of spinning.
                    _ => thread::sleep(left.min(Duration::from_millis(10))),
                }
            }
        }
    }

    /// Waits until `needle` has appeared after the cursor. Panics, with everything shown so far,
    /// if it does not within `limit`.
    pub fn expect(&mut self, needle: &str, limit: Duration) {
        if !self.try_expect(needle, limit) {
            panic!(
                "never saw {needle:?}; the terminal showed:\n{}",
                self.transcript
            );
        }
    }

    pub fn try_expect(&mut self, needle: &str, limit: Duration) -> bool {
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
    pub fn line(&mut self, text: &str) -> io::Result<()> {
        self.write_bytes(text.as_bytes())?;
        self.write_bytes(b"\n")
    }

    /// Types one raw byte, such as Ctrl+C (3).
    pub fn key(&mut self, byte: u8) -> io::Result<()> {
        self.write_bytes(&[byte])
    }

    /// Types raw bytes. The terminal can refuse input while nobody reads it (its buffer is
    /// small). Then this keeps reading the output, which may be what blocks the program, and
    /// panics after [`SHORT`] instead of waiting for ever.
    pub fn write_bytes(&mut self, bytes: &[u8]) -> io::Result<()> {
        let end = Instant::now() + SHORT;
        let mut sent = 0;
        while sent < bytes.len() {
            match self.master.write(&bytes[sent..]) {
                Ok(n) => sent += n,
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < end,
                        "the terminal took no input for {SHORT:?}; it showed:\n{}",
                        self.transcript
                    );
                    self.pump(Duration::from_millis(20));
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Runs `echo <tag>-$((1+1))` and waits for its output `<tag>-2`. The echoed command line
    /// holds `$((1+1))` and not `2`, so only the output matches.
    pub fn sync(&mut self, tag: &str) -> io::Result<()> {
        self.line(&format!("echo {tag}-$((1+1))"))?;
        self.expect(&format!("{tag}-2"), SHORT);
        Ok(())
    }

    /// Sends `command` again and again until the terminal shows `needle`. For things that happen
    /// a moment after the command that causes them.
    pub fn repeat_until(&mut self, command: &str, needle: &str) -> io::Result<()> {
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

    /// The processes of the session with this name (zombies too: a process nobody collected is
    /// something the shell got wrong).
    pub fn in_session(&self, name: &str) -> Vec<Pid> {
        procfs::session_members(self.shell.id())
            .into_iter()
            .filter(|m| m.name == name)
            .map(|m| m.pid)
            .collect()
    }

    /// Every process of the session.
    pub fn members(&self) -> Vec<Member> {
        procfs::session_members(self.shell.id())
    }

    /// Waits until no process of the session has this name.
    pub fn expect_none(&mut self, name: &str) {
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

    /// True if the process that `setsid` started has ended.
    pub fn has_exited(&mut self) -> io::Result<bool> {
        Ok(self.shell.try_wait()?.is_some())
    }

    /// Waits for the process that `setsid` started to end. Panics after `limit`.
    pub fn wait_exit(&mut self, limit: Duration) -> ExitStatus {
        let end = Instant::now() + limit;
        loop {
            match self.shell.try_wait() {
                Ok(Some(status)) => return status,
                Ok(None) => {}
                Err(e) => panic!("cannot wait for the process: {e}"),
            }
            assert!(
                Instant::now() < end,
                "the process did not exit within {limit:?}; the terminal showed:\n{}",
                self.transcript
            );
            self.pump(Duration::from_millis(50));
        }
    }

    /// Types `exit` and waits for the shell to end.
    pub fn quit(&mut self) -> io::Result<()> {
        self.line("exit")?;
        self.wait_exit(SHORT);
        Ok(())
    }

    /// The terminal's current settings (echo, line editing, raw mode), as the program on the
    /// other side sees them.
    pub fn termios(&self) -> io::Result<Termios> {
        tcgetattr(&self.master).map_err(io::Error::from)
    }

    /// The process group that owns the terminal now.
    pub fn foreground_pgrp(&self) -> io::Result<i32> {
        tcgetpgrp(&self.master)
            .map(Pid::as_raw)
            .map_err(io::Error::from)
    }

    /// Sends a signal to the process that `setsid` started.
    pub fn signal_parent(&self, signal: Signal) -> io::Result<()> {
        kill(Pid::from_raw(self.shell.id() as i32), signal).map_err(io::Error::from)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Twice, in case a process was forked while the first pass ran.
        for _ in 0..2 {
            for member in procfs::session_members(self.shell.id()) {
                let _ = kill(member.pid, Signal::SIGKILL);
            }
        }
        let _ = self.shell.kill();
        let _ = self.shell.wait();
    }
}
