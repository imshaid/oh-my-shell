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
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use nix::fcntl::{FcntlArg, FdFlag, OFlag, fcntl};
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::pty::{Winsize, openpty};
use nix::sys::signal::{Signal, kill};
use nix::sys::termios::{Termios, tcgetattr};
use nix::unistd::{Pid, tcgetpgrp};

use crate::procfs::{self, Member};

/// Held from opening a terminal until the program is started and the descriptors are closed
/// again, so that a program that another test starts at the same time cannot inherit this
/// test's terminal.
static START: Mutex<()> = Mutex::new(());

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
    answer_cursor_queries: bool,
}

/// The terminal asks where its cursor is with this; the answer is `ESC [ row ; column R`.
const CURSOR_QUERY: &[u8] = b"\x1b[6n";
/// What the test terminal answers. It does not draw anything, so the cursor is always "at the
/// top left"; the editor only needs *an* answer.
const CURSOR_REPLY: &[u8] = b"\x1b[1;1R";

/// The window of the test terminal. The editor lays the prompt out for it.
const WINDOW: Winsize = Winsize {
    ws_row: 24,
    ws_col: 80,
    ws_xpixel: 0,
    ws_ypixel: 0,
};

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
                // A terminal that takes escape codes, so the shell uses its line editor. Colors
                // are off to keep the transcript readable; a test that wants them removes this.
                ("TERM".into(), "xterm-256color".into()),
                ("NO_COLOR".into(), "1".into()),
            ],
            cwd: None,
            registry: None,
            answer_cursor_queries: true,
        }
    }

    /// Starts with `TERM=dumb`: the shell uses its plain line reader, with no escape codes.
    pub fn dumb(self) -> Self {
        self.env("TERM", "dumb")
    }

    /// The terminal never answers the editor's "where is the cursor?" question, like a terminal
    /// that does not support it.
    pub fn no_cursor_replies(mut self) -> Self {
        self.answer_cursor_queries = false;
        self
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

    /// Starts the program under the name `-oh-my-shell`, as `login`, `sshd` and `su -` start a
    /// login shell. The program's argument zero is what the caller passed to `execvp`, so this
    /// makes a symbolic link with that name in `dir`, puts `dir` first in `PATH` and runs the
    /// link. `dir` must exist and stay until the session ends.
    pub fn as_login(mut self, dir: &Path) -> io::Result<Self> {
        let name = "-oh-my-shell";
        let link = dir.join(name);
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(std::fs::canonicalize(&self.bin)?, &link)?;
        let path = self
            .env
            .iter()
            .find(|(k, _)| k == "PATH")
            .map_or("/usr/bin:/bin", |(_, v)| v.as_str())
            .to_owned();
        self = self.env("PATH", &format!("{}:{path}", dir.display()));
        self.bin = name.to_owned();
        Ok(self)
    }

    /// Starts the program with `SIGHUP` ignored, as `nohup` does. (`sh` ignores it and then
    /// replaces itself with the program, and an ignored signal stays ignored.)
    pub fn with_hangup_ignored(mut self) -> Self {
        let mut args = vec![
            "-c".to_owned(),
            "trap '' HUP; exec \"$0\" \"$@\"".to_owned(),
            self.bin.clone(),
        ];
        args.append(&mut self.args);
        self.args = args;
        "sh".clone_into(&mut self.bin);
        self
    }

    /// Sets `HOME` to `dir`. The shell writes its first-run `config.toml` under the home
    /// directory, so a test must not use a real one.
    pub fn home(self, dir: &Path) -> Self {
        self.env("HOME", &dir.to_string_lossy())
    }

    /// Sets (or replaces) an environment variable. The environment starts with only `PATH`,
    /// `HOME`, `LANG`, `TERM` and `NO_COLOR`. The three `XDG_*_HOME` variables are not in it: unless a
    /// test sets them, they are made from `HOME` when the program starts, so the shell's
    /// files always land under the home the test chose.
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
        let _starting = START.lock().unwrap_or_else(PoisonError::into_inner);
        let pty = openpty(Some(&WINDOW), None).map_err(io::Error::from)?;
        // The program must not inherit the master side, or the terminal would never hang up.
        // Nor may it inherit a second copy of the slave side: it gets the slave as 0, 1 and 2
        // (the copies made below are close-on-exec, and `dup2` in the child clears that flag).
        fcntl(&pty.master, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC)).map_err(io::Error::from)?;
        fcntl(&pty.slave, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC)).map_err(io::Error::from)?;
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
        if self.bin.starts_with('-') && !self.under_sh {
            // Otherwise `setsid` would read the program name as one of its own options.
            command.arg("--");
        }
        command.arg(&self.bin).args(&self.args);
        let mut env = self.env.clone();
        if let Some((_, home)) = env.iter().find(|(k, _)| k == "HOME").cloned() {
            for (name, under_home) in [
                ("XDG_CONFIG_HOME", ".config"),
                ("XDG_DATA_HOME", ".local/share"),
                ("XDG_STATE_HOME", ".local/state"),
            ] {
                if !env.iter().any(|(k, _)| k == name) {
                    env.push((name.to_owned(), format!("{home}/{under_home}")));
                }
            }
        }
        command
            .env_clear()
            .envs(env.iter().map(|(k, v)| (k, v)))
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
            answer_cursor_queries: self.answer_cursor_queries,
            scanned: 0,
            submitted: None,
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
    answer_cursor_queries: bool,
    /// How much of the transcript has been searched for cursor queries.
    scanned: usize,
    /// The last line that was submitted (Enter was typed), if any.
    submitted: Option<Submitted>,
}

/// A line that was submitted: how much the terminal had shown by then, and its text if the
/// harness typed it (a recalled line has none).
#[derive(Clone, Debug)]
struct Submitted {
    length: usize,
    text: Option<String>,
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
                    Ok(n) if n > 0 => {
                        self.transcript
                            .push_str(&String::from_utf8_lossy(&buffer[..n]));
                        self.answer_cursor_queries();
                    }
                    Err(e)
                        if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {}
                    // The other side hung up: nothing more will come. Poll would return at once
                    // from now on, so wait here instead of spinning.
                    _ => thread::sleep(left.min(Duration::from_millis(10))),
                }
            }
        }
    }

    /// Answers each new "where is the cursor?" question in the output, as a terminal does.
    fn answer_cursor_queries(&mut self) {
        let bytes = self.transcript.as_bytes();
        while let Some(at) = bytes[self.scanned..]
            .windows(CURSOR_QUERY.len())
            .position(|window| window == CURSOR_QUERY)
        {
            self.scanned += at + CURSOR_QUERY.len();
            if self.answer_cursor_queries {
                // Best effort: if the terminal is full, the question is asked again.
                let _ = self.master.write(CURSOR_REPLY);
            }
        }
        // A question may be cut in two reads: keep the last bytes for the next search.
        self.scanned = self
            .scanned
            .max(bytes.len().saturating_sub(CURSOR_QUERY.len() - 1));
    }

    /// Like [`Session::expect`], but escape sequences in the output are ignored, so that a
    /// prompt drawn in pieces (`~/dir ` + color change + `$ `) is found as the text the user
    /// sees. The cursor moves to just after the match.
    pub fn expect_plain(&mut self, needle: &str, limit: Duration) {
        let end = Instant::now() + limit;
        loop {
            let (plain, origin) = strip_escapes(&self.transcript[self.cursor..]);
            if let Some(at) = plain.find(needle) {
                let last = at + needle.len() - 1;
                self.cursor += origin[last] + 1;
                return;
            }
            assert!(
                Instant::now() < end,
                "never saw {needle:?} (ignoring escape sequences); the terminal showed:\n{}",
                self.transcript
            );
            self.pump(Duration::from_millis(50));
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

    /// Types `text` and presses Enter. The Enter key of a terminal sends a carriage return: a
    /// terminal in line mode turns it into a line feed, and the line editor (raw mode) reads it
    /// as Enter. This is what a person's keyboard does, so it is what the tests send.
    pub fn line(&mut self, text: &str) -> io::Result<()> {
        self.write_bytes(text.as_bytes())?;
        self.write_bytes(b"\r")?;
        if let Some(submitted) = &mut self.submitted {
            submitted.text = Some(text.to_owned());
        }
        Ok(())
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
        if bytes.iter().any(|b| matches!(b, b'\r' | b'\n')) {
            self.submitted = Some(Submitted {
                length: self.transcript.len(),
                text: None,
            });
        }
        Ok(())
    }

    /// True if the shell is back at its prompt after the last line that was submitted.
    ///
    /// Seen from the text on the screen (escape sequences, and what is drawn away from the
    /// cursor, taken out; see `without_side_drawing`): the submitted line has
    /// been shown, and what the terminal showed last is the prompt `$ ` with nothing typed
    /// after it. The first part matters: a prompt that is drawn again before the keys are
    /// processed (the editor does that once it has the cursor position) is not the next one.
    fn prompt_is_up(&self) -> bool {
        // Before any line is submitted, the first prompt is all there is to wait for.
        let (from, text) = match &self.submitted {
            Some(submitted) => (submitted.length, submitted.text.as_deref()),
            None => (0, Some("")),
        };
        let tail = &self.transcript[from..];
        let plain_of = |text: &str| strip_escapes(&without_side_drawing(text)).0;
        let shown = match text {
            Some(text) => plain_of(tail).contains(text),
            None => plain_of(tail).contains('\n'),
        };
        shown && last_paint_is_the_empty_prompt(tail, &plain_of)
    }

    /// Waits until the shell is back at its prompt after the last line that was typed.
    ///
    /// Use it between two lines typed to the shell. A line that is typed before the prompt is
    /// up is read by the editor together with the next one, and only the first of two lines
    /// that arrive together is run. That is a known limit of the editor (bracketed paste, S14b,
    /// is what helps a person who pastes). Without this call a test passes only while the
    /// shell is quick. It needs the main prompt, `$ `: not the continuation prompt, and not a
    /// program that is still running.
    pub fn prompt(&mut self) {
        let end = Instant::now() + SHORT;
        while !self.prompt_is_up() {
            assert!(
                Instant::now() < end,
                "never saw the prompt after the last line; the terminal showed:\n{}",
                self.transcript
            );
            self.pump(Duration::from_millis(20));
        }
    }

    /// Types a command to the shell: waits for the prompt (see [`Session::prompt`]), then types
    /// the line. Use it for every command typed to the shell; [`Session::line`] is for input to
    /// a program that is running, for the lines of an unfinished command, and for typing ahead
    /// on purpose.
    pub fn command(&mut self, text: &str) -> io::Result<()> {
        self.prompt();
        self.line(text)
    }

    /// Runs `echo <tag>-$((1+1))` and waits for its output `<tag>-2`. The echoed command line
    /// holds `$((1+1))` and not `2`, so only the output matches.
    ///
    /// It waits for the prompt before it types, and again after the output, so that no line is
    /// typed ahead of the prompt (see [`Session::prompt`]).
    pub fn sync(&mut self, tag: &str) -> io::Result<()> {
        self.command(&format!("echo {tag}-$((1+1))"))?;
        self.expect(&format!("{tag}-2"), SHORT);
        self.prompt();
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

    /// Closes the terminal from the outside, as closing a terminal window or dropping an ssh
    /// connection does: the kernel hangs up the session, and reading the terminal gives end of
    /// input from then on. Afterwards the terminal shows nothing more and takes no input.
    pub fn hang_up_terminal(&mut self) -> io::Result<()> {
        // Replacing the file closes the master side of the pseudo-terminal (nothing else holds it:
        // it is close-on-exec).
        self.master = File::open("/dev/null")?;
        Ok(())
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

/// True if the last thing the editor painted is the prompt with nothing typed after it, whatever
/// was printed since (a job's message can arrive after the prompt). The editor hides the cursor
/// (`ESC [ ? 25 l`) when it starts a paint and shows it (`ESC [ ? 25 h`) when it is done; a
/// reader that paints nothing (the plain one) is judged by the end of the output.
fn last_paint_is_the_empty_prompt(output: &str, plain_of: &dyn Fn(&str) -> String) -> bool {
    const HIDE: &str = "\x1b[?25l";
    const SHOW: &str = "\x1b[?25h";
    let Some(start) = output.rfind(HIDE) else {
        return plain_of(output).ends_with("$ ");
    };
    output[start..]
        .find(SHOW)
        .is_some_and(|end| plain_of(&output[start..start + end]).ends_with("$ "))
}

/// The output without what the editor draws away from the cursor: it saves the cursor
/// position (`ESC 7`), draws somewhere else (the right side of the prompt: the status of the last
/// command) and goes back (`ESC 8`). That text is not part of the line the user types in.
fn without_side_drawing(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("\x1b7") {
        out.push_str(&rest[..start]);
        match rest[start..].find("\x1b8") {
            Some(end) => rest = &rest[start + end + 2..],
            // Drawn only in part so far: the rest is not output that can be read yet.
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// The text without escape sequences (`ESC [ ... final`, and `ESC` plus one character), and for
/// each byte of it the position of that byte in `text`.
pub fn strip_escapes(text: &str) -> (String, Vec<usize>) {
    let mut plain = String::new();
    let mut origin = Vec::new();
    let mut chars = text.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        if c != '\x1b' {
            plain.push(c);
            origin.extend((0..c.len_utf8()).map(|k| at + k));
            continue;
        }
        if chars.peek().is_some_and(|&(_, next)| next == '[') {
            chars.next();
            // Parameters and intermediates, then one final byte in `@`..=`~`.
            for (_, c) in chars.by_ref() {
                if ('\u{40}'..='\u{7e}').contains(&c) {
                    break;
                }
            }
        } else {
            chars.next();
        }
    }
    (plain, origin)
}
