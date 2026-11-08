//! The shell's own signal settings.

#![allow(
    unsafe_code,
    reason = "changing a signal disposition is an unsafe call in `nix`"
)]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use nix::libc;
use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};

/// Set by the SIGINT handler. The executor looks at it before each command and each pass of
/// a loop, and stops the line when it is set.
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// Set by the SIGHUP handler of an interactive shell: the terminal is gone, or someone sent
/// the signal. The shell ends, and takes its jobs with it.
static HUNG_UP: AtomicBool = AtomicBool::new(false);

/// Bit `n` is set when signal `n` arrived and has a `trap` command that has not run yet.
static PENDING: AtomicU64 = AtomicU64::new(0);

/// True once [`enter_interactive`] has made the shell survive Ctrl+C, which only an
/// interactive shell does.
static SURVIVES_INTERRUPTS: AtomicBool = AtomicBool::new(false);

/// Bit `n` is set if signal `n` was ignored when the shell started (see
/// [`note_inherited_signals`]).
static INHERITED_IGNORED: AtomicU64 = AtomicU64::new(0);

extern "C" fn on_trapped(signal: i32) {
    // Storing to an atomic is async-signal-safe. The command runs later, from the executor.
    if (0..64).contains(&signal) {
        PENDING.fetch_or(1_u64 << signal, Ordering::SeqCst);
    }
}

extern "C" fn on_hangup(_signal: i32) {
    // Storing to an atomic is async-signal-safe. Nothing else happens in the handler.
    HUNG_UP.store(true, Ordering::SeqCst);
}

extern "C" fn on_interrupt(_signal: i32) {
    // Storing to an atomic is async-signal-safe. Nothing else happens in the handler.
    INTERRUPTED.store(true, Ordering::SeqCst);
}

/// Remembers which signals were ignored when the shell started. Call it once, first thing,
/// before anything changes a signal setting.
///
/// An ignored signal stays ignored in everything the shell starts, and a non-interactive
/// shell cannot `trap` it (POSIX). That is how `nohup` and a background `&` work. `SIGPIPE` is
/// left out: Rust's runtime ignores it before `main`, so it says nothing about the parent.
pub fn note_inherited_signals() {
    let mut mask = 0_u64;
    for signal in 1..32_i32 {
        if matches!(signal, libc::SIGKILL | libc::SIGSTOP | libc::SIGPIPE) {
            continue;
        }
        // SAFETY: a zeroed `sigaction` is a valid value to be filled in. Passing a null new
        // action only reads the current setting; nothing is changed.
        let ignored = unsafe {
            let mut current: libc::sigaction = std::mem::zeroed();
            libc::sigaction(signal, std::ptr::null(), &raw mut current) == 0
                && current.sa_sigaction == libc::SIG_IGN
        };
        if ignored {
            mask |= 1_u64 << signal;
        }
    }
    INHERITED_IGNORED.store(mask, Ordering::SeqCst);
}

/// Bit `n` is set for each signal `n` that was ignored when the shell started.
pub(crate) fn inherited_ignored() -> u64 {
    INHERITED_IGNORED.load(Ordering::SeqCst)
}

/// Sets the signals an interactive shell has: it survives what the terminal and other
/// programs send to it, as POSIX asks.
///
/// - `SIGINT` (Ctrl+C) is caught: the handler only sets a flag, which the executor reads with
///   [`interrupted`]. Ignoring it would not do, because loops and functions run inside the
///   shell, and Ctrl+C has to stop them.
/// - `SIGQUIT` (Ctrl+\), `SIGTERM`, `SIGTSTP` (Ctrl+Z), `SIGTTIN` and `SIGTTOU` are ignored.
///   The last one matters most: without it, the shell would stop itself when it takes the
///   terminal back from a job.
///
/// Programs get the default behavior back before they run (see `spawn`).
pub fn enter_interactive() {
    SURVIVES_INTERRUPTS.store(true, Ordering::SeqCst);
    let catch = SigAction::new(
        SigHandler::Handler(on_interrupt),
        SaFlags::SA_RESTART,
        SigSet::empty(),
    );
    // SAFETY: the handler only stores to an atomic, which is async-signal-safe.
    let _ = unsafe { sigaction(Signal::SIGINT, &catch) };
    // A hangup ends the shell (and its jobs), unless it was told to ignore it (`nohup`).
    if inherited_ignored() & (1_u64 << libc::SIGHUP) == 0 {
        let hangup = SigAction::new(
            SigHandler::Handler(on_hangup),
            SaFlags::SA_RESTART,
            SigSet::empty(),
        );
        // SAFETY: the handler only stores to an atomic, which is async-signal-safe.
        let _ = unsafe { sigaction(Signal::SIGHUP, &hangup) };
    }
    let ignore = SigAction::new(SigHandler::SigIgn, SaFlags::empty(), SigSet::empty());
    for signal in [
        Signal::SIGQUIT,
        Signal::SIGTERM,
        Signal::SIGTSTP,
        Signal::SIGTTIN,
        Signal::SIGTTOU,
    ] {
        // SAFETY: `SigIgn` installs no handler, so there is no code that could run at a bad
        // moment. The call only changes what the kernel does with the signal.
        let _ = unsafe { sigaction(signal, &ignore) };
    }
}

/// Records that a program died of Ctrl+C. With job control the shell is not in the
/// foreground group, so it never receives the `SIGINT` itself; the program's death is how it
/// learns that the user pressed Ctrl+C.
pub(crate) fn note_interrupt() {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

/// True if Ctrl+C was pressed since the flag was last cleared.
#[must_use]
pub fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}

/// Forgets a Ctrl+C that was seen. The top of the executor calls this when it starts a line
/// and after it has stopped one.
pub fn clear_interrupt() {
    INTERRUPTED.store(false, Ordering::SeqCst);
}

/// True if the shell has been hung up: the terminal closed, or `SIGHUP` was sent to it. Only an
/// interactive shell catches the signal, so this is false in any other.
#[must_use]
pub fn hung_up() -> bool {
    HUNG_UP.load(Ordering::SeqCst)
}

/// What the shell asks the system to do with a signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Disposition {
    /// What the shell does when no `trap` is set.
    Default,
    /// `trap '' SIG`: the signal does nothing, also to the programs the shell starts.
    Ignore,
    /// `trap 'command' SIG`: the signal is noted and the command runs between commands.
    Catch,
}

/// Sets what happens when `signal` arrives. Only the main shell may call this, because a
/// signal setting belongs to the whole process.
pub(crate) fn set_disposition(signal: i32, disposition: Disposition) {
    let Ok(signal) = Signal::try_from(signal) else {
        return;
    };
    let interactive = SURVIVES_INTERRUPTS.load(Ordering::SeqCst);
    let inherited = inherited_ignored() & (1_u64 << (signal as i32)) != 0;
    if inherited && !interactive {
        // POSIX: a signal ignored on entry to a non-interactive shell cannot be trapped or
        // reset.
        return;
    }
    let the_shell_ignores = matches!(
        signal,
        Signal::SIGQUIT | Signal::SIGTERM | Signal::SIGTSTP | Signal::SIGTTIN | Signal::SIGTTOU
    );
    let handler = match (disposition, signal) {
        (Disposition::Ignore, _) => SigHandler::SigIgn,
        (Disposition::Catch, _) => SigHandler::Handler(on_trapped),
        (Disposition::Default, _) if inherited => SigHandler::SigIgn,
        (Disposition::Default, Signal::SIGHUP) if interactive => SigHandler::Handler(on_hangup),
        (Disposition::Default, _) if interactive && the_shell_ignores => SigHandler::SigIgn,
        (Disposition::Default, Signal::SIGINT) if interactive => SigHandler::Handler(on_interrupt),
        (Disposition::Default, _) => SigHandler::SigDfl,
    };
    let action = SigAction::new(handler, SaFlags::SA_RESTART, SigSet::empty());
    // SAFETY: the handlers only store to an atomic, which is async-signal-safe, and `SigIgn`
    // and `SigDfl` install no code at all.
    let _ = unsafe { sigaction(signal, &action) };
}

/// True if a signal with a `trap` command has arrived and its command has not run.
#[must_use]
pub(crate) fn trap_pending() -> bool {
    PENDING.load(Ordering::SeqCst) != 0
}

/// Takes the set of signals that arrived since the last call: bit `n` is signal `n`.
pub(crate) fn take_pending() -> u64 {
    PENDING.swap(0, Ordering::SeqCst)
}
