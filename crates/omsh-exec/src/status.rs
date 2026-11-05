//! Exit statuses, and how a finished child is described.

use nix::sys::signal::Signal;
use nix::sys::wait::WaitStatus;

/// The exit status of a command: 0 is success. A process that was killed by signal `n`
/// has status `128 + n`, the same as in other shells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExitStatus(pub u8);

impl ExitStatus {
    /// The command succeeded.
    pub const SUCCESS: Self = Self(0);
    /// A syntax error, or a line this shell cannot run yet.
    pub const USAGE: Self = Self(2);
    /// The file exists, but could not be run.
    pub const NOT_EXECUTABLE: Self = Self(126);
    /// The command was not found.
    pub const NOT_FOUND: Self = Self(127);

    /// The status of a process killed by a signal.
    #[must_use]
    pub fn from_signal(signal: i32) -> Self {
        // Signal numbers are small (1..=64 on Linux), so this cannot overflow in practice.
        Self(u8::try_from(128 + signal).unwrap_or(255))
    }

    /// The status as a process exit code.
    #[must_use]
    pub const fn code(self) -> u8 {
        self.0
    }
}

/// How a child process ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Termination {
    /// It called `exit` or returned from `main` with this code.
    Exited(u8),
    /// A signal killed it.
    Signaled {
        /// The signal number.
        signal: i32,
        /// True if the kernel wrote a core dump.
        core_dumped: bool,
    },
}

impl Termination {
    /// Reads the result of `waitpid`. Returns nothing for states where the child is still
    /// alive (stopped or continued), which S09 handles.
    #[must_use]
    pub const fn from_wait(status: WaitStatus) -> Option<Self> {
        match status {
            WaitStatus::Exited(_, code) => {
                // `exit(256)` is `exit(0)`: the kernel keeps only the low 8 bits.
                Some(Self::Exited((code & 0xff) as u8))
            }
            WaitStatus::Signaled(_, signal, core_dumped) => Some(Self::Signaled {
                signal: signal as i32,
                core_dumped,
            }),
            _ => None,
        }
    }

    /// The exit status that a shell reports for this ending.
    #[must_use]
    pub fn status(self) -> ExitStatus {
        match self {
            Self::Exited(code) => ExitStatus(code),
            Self::Signaled { signal, .. } => ExitStatus::from_signal(signal),
        }
    }

    /// A line telling the user what happened, for `command`. Nothing for a normal exit,
    /// and nothing for `SIGINT` and `SIGPIPE`: the user pressed Ctrl+C, or the reader of a
    /// pipe went away, and other shells stay quiet about both.
    #[must_use]
    pub fn describe(self, command: &str) -> Option<String> {
        let Self::Signaled {
            signal,
            core_dumped,
        } = self
        else {
            return None;
        };
        let known = Signal::try_from(signal).ok();
        if matches!(known, Some(Signal::SIGINT | Signal::SIGPIPE)) {
            return None;
        }
        let name = known.map_or_else(|| format!("signal {signal}"), |s| s.as_str().to_owned());
        let meaning = known.and_then(meaning);
        let mut line = format!("oh-my-shell: {command}: killed by {name}");
        if let Some(meaning) = meaning {
            line.push_str(" (");
            line.push_str(meaning);
            line.push(')');
        }
        if core_dumped {
            line.push_str(", core dumped");
        }
        Some(line)
    }
}

/// A plain-language name for the signals people meet most often.
const fn meaning(signal: Signal) -> Option<&'static str> {
    Some(match signal {
        Signal::SIGSEGV => "segmentation fault",
        Signal::SIGABRT => "aborted",
        Signal::SIGKILL => "killed",
        Signal::SIGTERM => "terminated",
        Signal::SIGBUS => "bus error",
        Signal::SIGFPE => "arithmetic error",
        Signal::SIGILL => "illegal instruction",
        Signal::SIGHUP => "hangup",
        Signal::SIGQUIT => "quit",
        Signal::SIGALRM => "alarm clock",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signal_gives_128_plus_its_number() {
        assert_eq!(ExitStatus::from_signal(15), ExitStatus(143));
        assert_eq!(ExitStatus::from_signal(11), ExitStatus(139));
    }

    #[test]
    fn exit_and_signal_endings_give_their_statuses() {
        assert_eq!(Termination::Exited(7).status(), ExitStatus(7));
        let killed = Termination::Signaled {
            signal: 9,
            core_dumped: false,
        };
        assert_eq!(killed.status(), ExitStatus(137));
    }

    #[test]
    fn a_normal_exit_has_no_message() {
        assert_eq!(Termination::Exited(1).describe("false"), None);
    }

    #[test]
    fn a_crash_names_the_signal_and_the_core_dump() {
        let crash = Termination::Signaled {
            signal: Signal::SIGSEGV as i32,
            core_dumped: true,
        };
        assert_eq!(
            crash.describe("prog").as_deref(),
            Some("oh-my-shell: prog: killed by SIGSEGV (segmentation fault), core dumped"),
        );
    }

    #[test]
    fn a_signal_without_a_plain_name_is_still_named() {
        let usr = Termination::Signaled {
            signal: Signal::SIGUSR1 as i32,
            core_dumped: false,
        };
        assert_eq!(
            usr.describe("prog").as_deref(),
            Some("oh-my-shell: prog: killed by SIGUSR1"),
        );
    }

    #[test]
    fn ctrl_c_and_broken_pipes_are_not_reported() {
        for signal in [Signal::SIGINT, Signal::SIGPIPE] {
            let ending = Termination::Signaled {
                signal: signal as i32,
                core_dumped: false,
            };
            assert_eq!(ending.describe("prog"), None);
        }
    }

    #[test]
    fn a_stopped_child_is_not_an_ending() {
        let stopped = WaitStatus::StillAlive;
        assert_eq!(Termination::from_wait(stopped), None);
    }
}
