//! The shell's own signal settings.

#![allow(
    unsafe_code,
    reason = "changing a signal disposition is an unsafe call in `nix`"
)]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};

/// Set by the SIGINT handler. The executor looks at it before each command and each pass of
/// a loop, and stops the line when it is set.
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// Bit `n` is set when signal `n` arrived and has a `trap` command that has not run yet.
static PENDING: AtomicU64 = AtomicU64::new(0);

/// True once [`catch_interrupts`] has made the shell survive Ctrl+C, which only an
/// interactive shell does.
static SURVIVES_INTERRUPTS: AtomicBool = AtomicBool::new(false);

extern "C" fn on_trapped(signal: i32) {
    // Storing to an atomic is async-signal-safe. The command runs later, from the executor.
    if (0..64).contains(&signal) {
        PENDING.fetch_or(1_u64 << signal, Ordering::SeqCst);
    }
}

extern "C" fn on_interrupt(_signal: i32) {
    // Storing to an atomic is async-signal-safe. Nothing else happens in the handler.
    INTERRUPTED.store(true, Ordering::SeqCst);
}

/// Makes the shell survive Ctrl+C (`SIGINT`) and Ctrl+\ (`SIGQUIT`), as POSIX asks of an
/// interactive shell. The terminal sends those signals to every process in the foreground,
/// and the shell must outlive the program it started.
///
/// `SIGQUIT` is ignored. `SIGINT` is caught: the handler only sets a flag, which the
/// executor reads with [`interrupted`]. Ignoring it would not do, because loops and
/// functions run inside the shell, and Ctrl+C has to stop them. Programs started by the shell
/// get the default behavior back before they run (see `spawn`).
///
/// S08 replaces this with the full set of dispositions for job control.
pub fn catch_interrupts() {
    SURVIVES_INTERRUPTS.store(true, Ordering::SeqCst);
    let catch = SigAction::new(
        SigHandler::Handler(on_interrupt),
        SaFlags::SA_RESTART,
        SigSet::empty(),
    );
    // SAFETY: the handler only stores to an atomic, which is async-signal-safe.
    let _ = unsafe { sigaction(Signal::SIGINT, &catch) };
    let ignore = SigAction::new(SigHandler::SigIgn, SaFlags::empty(), SigSet::empty());
    // SAFETY: `SigIgn` installs no handler, so there is no code that could run at a bad
    // moment. The call only changes what the kernel does with the signal.
    let _ = unsafe { sigaction(Signal::SIGQUIT, &ignore) };
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
    let handler = match (disposition, signal) {
        (Disposition::Ignore, _) | (Disposition::Default, Signal::SIGQUIT) if interactive => {
            SigHandler::SigIgn
        }
        (Disposition::Ignore, _) => SigHandler::SigIgn,
        (Disposition::Catch, _) => SigHandler::Handler(on_trapped),
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
