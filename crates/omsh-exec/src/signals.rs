//! The shell's own signal settings.

#![allow(
    unsafe_code,
    reason = "changing a signal disposition is an unsafe call in `nix`"
)]

use nix::sys::signal::{SigHandler, Signal, signal};

/// Makes the shell itself ignore Ctrl+C (`SIGINT`) and Ctrl+\ (`SIGQUIT`), as POSIX asks of
/// an interactive shell. The terminal sends those signals to every process in the
/// foreground, and the shell must outlive the program it started. Programs started by the
/// shell get the default behavior back before they run (see `spawn`).
///
/// S08 replaces this with the full set of dispositions for job control.
pub fn ignore_interrupts() {
    for sig in [Signal::SIGINT, Signal::SIGQUIT] {
        // SAFETY: `SigIgn` installs no handler, so there is no code that could run at a bad
        // moment. The call only changes what the kernel does with the signal.
        let _ = unsafe { signal(sig, SigHandler::SigIgn) };
    }
}
