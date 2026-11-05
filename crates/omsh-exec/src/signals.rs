//! The shell's own signal settings.

#![allow(
    unsafe_code,
    reason = "changing a signal disposition is an unsafe call in `nix`"
)]

use std::sync::atomic::{AtomicBool, Ordering};

use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};

/// Set by the SIGINT handler. The executor looks at it before each command and each pass of
/// a loop, and stops the line when it is set.
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

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
