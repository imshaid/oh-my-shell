//! Who owns the terminal, and what state it is in.
//!
//! A terminal has one *foreground process group*. Only its members may read the keyboard, and
//! the terminal sends Ctrl+C (`SIGINT`) and Ctrl+Z (`SIGTSTP`) to that group alone. So an
//! interactive shell puts itself in a group of its own, and gives the terminal to each job it
//! starts (`tcsetpgrp`). Ctrl+C then reaches the job and not the shell.
//!
//! The terminal also has settings (termios: echo, line editing, raw mode). A full-screen
//! program changes them and puts them back when it ends. If it is killed, nobody does, and
//! the terminal stays broken. So the shell saves the settings and restores them after a job
//! that was killed or stopped (the rule bash uses). After a job that ended normally it keeps
//! whatever the job left, so `stty -echo` still has an effect.
//!
//! Nothing here needs `unsafe`.

use std::io::IsTerminal;
use std::os::fd::{AsFd, AsRawFd, OwnedFd, RawFd};
use std::panic;
use std::sync::{Mutex, OnceLock, PoisonError};
use std::thread::{self, ThreadId};

use nix::sys::signal::{Signal, killpg};
use nix::sys::termios::{SetArg, Termios, tcgetattr, tcsetattr};
use nix::unistd::{Pid, getpgrp, getpid, setpgid, tcgetpgrp, tcsetpgrp};

use crate::signals;

/// How many times the shell asks to be put in the foreground before it gives up. Each ask
/// stops it until a user brings it to the foreground, so reaching this means something is
/// wrong, such as `SIGTTIN` being ignored.
const FOREGROUND_TRIES: u32 = 1000;

/// The terminal the shell controls.
#[derive(Debug)]
pub(crate) struct Terminal {
    /// A private copy of descriptor 0, close-on-exec.
    fd: OwnedFd,
    /// The shell's own process group.
    shell_pgid: Pid,
    /// The group the shell was in when it started (and the terminal's foreground group then).
    original_pgid: Pid,
    /// True if the shell moved itself into a new group, which it must undo when it ends.
    changed_group: bool,
    /// The thread that started job control. Only a panic here ends the process.
    main_thread: ThreadId,
    /// The settings to put back.
    saved: Mutex<Termios>,
}

static TERMINAL: OnceLock<Terminal> = OnceLock::new();

/// The terminal, if job control is on.
pub(crate) fn get() -> Option<&'static Terminal> {
    TERMINAL.get()
}

/// Takes control of the terminal: waits until the shell is in the foreground, makes it ignore
/// the job control signals, puts it in a group of its own and gives that group the terminal.
/// Call it once, when standard input is a terminal.
///
/// The shell survives Ctrl+C and the other signals either way (see
/// [`signals::enter_interactive`]); on an error only job control is missing, and programs
/// then run in the shell's own group, as they did before.
///
/// # Errors
///
/// Returns a sentence saying why job control is not available.
pub fn start_interactive() -> Result<(), String> {
    let result = try_start();
    if result.is_err() {
        signals::enter_interactive();
    }
    result
}

fn try_start() -> Result<(), String> {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        return Err("standard input is not a terminal".to_owned());
    }
    let fd = stdin
        .as_fd()
        .try_clone_to_owned()
        .map_err(|e| format!("cannot copy the terminal descriptor: {e}"))?;
    let original_pgid = getpgrp();

    // A shell started in the background must wait: taking the terminal would steal it from
    // the foreground job. `SIGTTIN` (at its default here) stops the shell until someone
    // brings it to the foreground.
    let mut tries = 0;
    loop {
        let foreground =
            tcgetpgrp(&fd).map_err(|e| format!("cannot read the foreground group: {e}"))?;
        if foreground == getpgrp() {
            break;
        }
        tries += 1;
        if tries > FOREGROUND_TRIES {
            return Err("the shell is not in the foreground".to_owned());
        }
        let _ = killpg(getpgrp(), Signal::SIGTTIN);
    }

    // From here the shell ignores SIGTTOU, which it needs for `tcsetpgrp`.
    signals::enter_interactive();

    let shell_pgid = getpid();
    let mut changed_group = false;
    if getpgrp() != shell_pgid {
        setpgid(Pid::from_raw(0), Pid::from_raw(0))
            .map_err(|e| format!("cannot create a process group: {e}"))?;
        changed_group = true;
    }
    let undo = |message: String| {
        if changed_group {
            let _ = setpgid(Pid::from_raw(0), original_pgid);
        }
        message
    };
    tcsetpgrp(&fd, shell_pgid).map_err(|e| undo(format!("cannot take the terminal: {e}")))?;
    let saved = tcgetattr(&fd).map_err(|e| {
        let _ = tcsetpgrp(&fd, original_pgid);
        undo(format!("cannot read the terminal settings: {e}"))
    })?;

    let terminal = Terminal {
        fd,
        shell_pgid,
        original_pgid,
        changed_group,
        main_thread: thread::current().id(),
        saved: Mutex::new(saved),
    };
    if TERMINAL.set(terminal).is_err() {
        return Err("job control was already started".to_owned());
    }
    install_panic_hook();
    Ok(())
}

/// A panic on the main thread ends the shell, and must not leave the terminal in the state a
/// job put it in, or owned by a group that is about to disappear. A panic on another thread
/// (a pipeline stage) is caught by the shell, so nothing is done for it.
fn install_panic_hook() {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        if let Some(terminal) = get()
            && thread::current().id() == terminal.main_thread
        {
            terminal.restore_for_exit();
        }
        previous(info);
    }));
}

impl Terminal {
    /// The descriptor a child passes to `tcsetpgrp`.
    pub(crate) fn raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }

    fn saved(&self) -> std::sync::MutexGuard<'_, Termios> {
        self.saved.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The shell takes the terminal back from a job that is over. If the job was killed or
    /// stopped (`hurt`), the settings it may have left are replaced by the saved ones.
    /// Otherwise what the job left is kept: it becomes the saved state.
    pub(crate) fn take_back(&self, hurt: bool) {
        // The shell ignores SIGTTOU, so this works although the shell is in the background
        // group until it has finished.
        let _ = tcsetpgrp(&self.fd, self.shell_pgid);
        let mut saved = self.saved();
        if hurt {
            let _ = tcsetattr(&self.fd, SetArg::TCSADRAIN, &saved);
        } else if let Ok(now) = tcgetattr(&self.fd) {
            *saved = now;
        }
    }

    /// Hands the terminal and the process group back to where they were at the start.
    fn give_back(&self) {
        let _ = tcsetpgrp(&self.fd, self.original_pgid);
        if self.changed_group {
            let _ = setpgid(Pid::from_raw(0), self.original_pgid);
        }
    }

    /// For a panic: never waits for a lock that a panicking thread may hold.
    fn restore_for_exit(&self) {
        if let Ok(saved) = self.saved.try_lock() {
            let _ = tcsetattr(&self.fd, SetArg::TCSANOW, &saved);
        }
        self.give_back();
    }
}

/// Gives the terminal back to the group the shell started in. Call it when the shell ends,
/// and before `exec` replaces it: a process left in a group that is not the foreground one
/// would be stopped when it reads the keyboard.
pub fn release_terminal() {
    if let Some(terminal) = get() {
        terminal.give_back();
    }
}

/// Takes the terminal again after [`release_terminal`], when the `exec` it was for failed.
pub(crate) fn reacquire_terminal() {
    let Some(terminal) = get() else { return };
    if terminal.changed_group {
        let _ = setpgid(Pid::from_raw(0), Pid::from_raw(0));
    }
    let _ = tcsetpgrp(&terminal.fd, terminal.shell_pgid);
}
