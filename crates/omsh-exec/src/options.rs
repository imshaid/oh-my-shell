//! The shell options that `set` changes: `-e`, `-u`, `-x`, `-f` and `-C`.

/// Which options are on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Options {
    /// `-e`, `errexit`: a command that fails ends the shell.
    pub(crate) errexit: bool,
    /// `-u`, `nounset`: expanding a parameter that is not set is an error.
    pub(crate) nounset: bool,
    /// `-x`, `xtrace`: each command is written to standard error before it runs.
    pub(crate) xtrace: bool,
    /// `-f`, `noglob`: no pathname expansion.
    pub(crate) noglob: bool,
    /// `-C`, `noclobber`: `>` does not overwrite an existing file.
    pub(crate) noclobber: bool,
    /// `-o pipefail`: a pipeline fails if any of its commands failed. There is no letter.
    pub(crate) pipefail: bool,
}

/// The names of `set -o`, in the order dash lists them. Those that this shell does not have
/// are listed as off and refused when someone sets them.
pub(crate) const NAMES: &[&str] = &[
    "errexit",
    "noglob",
    "ignoreeof",
    "interactive",
    "monitor",
    "noexec",
    "stdin",
    "xtrace",
    "verbose",
    "vi",
    "emacs",
    "noclobber",
    "allexport",
    "notify",
    "nounset",
    "privileged",
    "nolog",
    "pipefail",
    "debug",
];

impl Options {
    /// Turns the option with this letter on or off. Returns false for a letter that is not
    /// an option of this shell.
    pub(crate) fn set_letter(&mut self, letter: char, on: bool) -> bool {
        match letter {
            'e' => self.errexit = on,
            'u' => self.nounset = on,
            'x' => self.xtrace = on,
            'f' => self.noglob = on,
            'C' => self.noclobber = on,
            _ => return false,
        }
        true
    }

    /// The letter of an option name, if it has one.
    const fn letter_of(name: &str) -> Option<char> {
        match name.as_bytes() {
            b"errexit" => Some('e'),
            b"noglob" => Some('f'),
            b"noclobber" => Some('C'),
            b"nounset" => Some('u'),
            b"xtrace" => Some('x'),
            _ => None,
        }
    }

    /// Turns the option with this `set -o` name on or off. Returns false for a name that this
    /// shell has no option for.
    pub(crate) fn set_named(&mut self, name: &str, on: bool) -> bool {
        if name == "pipefail" {
            self.pipefail = on;
            return true;
        }
        Self::letter_of(name).is_some_and(|letter| self.set_letter(letter, on))
    }

    /// Whether the option with this `set -o` name is on.
    pub(crate) fn named(&self, name: &str) -> bool {
        if name == "pipefail" {
            return self.pipefail;
        }
        Self::letter_of(name).is_some_and(|letter| self.letter(letter))
    }

    /// Whether the option with this letter is on.
    pub(crate) const fn letter(&self, letter: char) -> bool {
        match letter {
            'e' => self.errexit,
            'u' => self.nounset,
            'x' => self.xtrace,
            'f' => self.noglob,
            'C' => self.noclobber,
            _ => false,
        }
    }

    /// The letters of the options that are on, in the order dash shows them in `$-`.
    /// `interactive` and `stdin` are not options of `Options`; the caller says how they stand.
    pub(crate) fn dollar_dash(&self, interactive: bool) -> String {
        let mut out = String::new();
        for letter in ['u', 'C', 'x'] {
            if self.letter(letter) {
                out.push(letter);
            }
        }
        // The shell reads its commands from standard input.
        out.push('s');
        if interactive {
            out.push('i');
        }
        for letter in ['f', 'e'] {
            if self.letter(letter) {
                out.push(letter);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_letters_come_in_the_order_dash_uses() {
        let mut options = Options::default();
        assert_eq!(options.dollar_dash(false), "s");
        for letter in ['e', 'f', 'u', 'x', 'C'] {
            assert!(options.set_letter(letter, true));
        }
        assert_eq!(options.dollar_dash(false), "uCxsfe");
        assert!(!options.set_letter('m', true));
    }
}
