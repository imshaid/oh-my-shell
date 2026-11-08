//! Default colors for `ls`, `grep`, `egrep`, `fgrep` and `diff` in the interactive shell.
//!
//! The shell defines five functions (see [`WRAPPERS`]). Each one calls the internal word
//! `__omsh_color`, which turns `ls -l` into `ls --color=auto -l` when three things are true:
//! the program accepts the flag (asked once, by running it), `NO_COLOR` is not set to
//! something non-empty, and nothing else decided first. Because the wrappers are ordinary
//! functions, the usual rules give the overrides: an alias is replaced before the lookup, a
//! function of the same name in `init.omsh` replaces the wrapper, `unset -f ls` removes it and
//! `command ls` skips it.
//!
//! The shell never sets `LS_COLORS`, `GREP_COLOR` or `GREP_COLORS`: the programs' own defaults
//! (or the user's variables) decide the colors.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use omsh_syntax::parse;

use crate::exec::{Flow, Unwind};
use crate::io::Io;
use crate::path::Lookup;
use crate::shell::Shell;
use crate::status::ExitStatus;

/// The word the wrapper functions call. It is not a builtin the user is meant to type.
pub(crate) const INTERNAL: &str = "__omsh_color";

/// The functions that are defined before `init.omsh` runs.
const WRAPPERS: &str = "\
ls() { __omsh_color ls \"$@\"; }
grep() { __omsh_color grep \"$@\"; }
egrep() { __omsh_color egrep \"$@\"; }
fgrep() { __omsh_color fgrep \"$@\"; }
diff() { __omsh_color diff \"$@\"; }
";

/// For each wrapped program, the command that tells whether it takes `--color=auto`. It must
/// succeed when the flag is understood: GNU `grep` prints its help and exits 0 for a *bad*
/// color value, so the probe has to be a real search that matches.
const PROBES: &[(&str, &str)] = &[
    ("ls", "command ls --color=auto -d / >/dev/null 2>&1"),
    (
        "grep",
        "command grep --color=auto -q x <<<x >/dev/null 2>&1",
    ),
    (
        "egrep",
        "command egrep --color=auto -q x <<<x >/dev/null 2>&1",
    ),
    (
        "fgrep",
        "command fgrep --color=auto -q x <<<x >/dev/null 2>&1",
    ),
    (
        "diff",
        "command diff --color=auto /dev/null /dev/null >/dev/null 2>&1",
    ),
];

/// What was learned about each program file: does it take `--color=auto`? The key is the
/// file that PATH lookup found, made absolute. The cache is shared by every copy of the shell
/// (pipeline stages, `$(...)`, background jobs), so `ps | grep x` asks once per session and not
/// on every call.
pub(crate) type ProbeCache = Arc<Mutex<HashMap<PathBuf, bool>>>;

fn probe_for(name: &[u8]) -> Option<&'static str> {
    PROBES
        .iter()
        .find(|(known, _)| known.as_bytes() == name)
        .map(|(_, probe)| *probe)
}

impl Shell {
    /// Defines the color wrappers. The interactive shell calls this before `init.omsh`, so that
    /// the file can replace or remove them. Nothing is run and nothing is probed here.
    pub fn define_color_wrappers(&mut self, io: &Io) {
        if let Err(unwind) = self.run_internal(WRAPPERS, io) {
            // The text is a constant; this cannot happen unless the definitions are broken.
            io.say(format!(
                "oh-my-shell: could not define the color wrappers ({unwind:?})"
            ));
        }
    }

    /// Parses and runs shell text that the shell itself wrote. Aliases are not replaced in it,
    /// so a user's alias cannot change what it means.
    fn run_internal(&mut self, text: &str, io: &Io) -> Flow {
        let Ok(list) = parse(text) else {
            // The text is a constant of this crate, so this is a bug here and not the user's.
            io.say(format!("oh-my-shell: internal text does not parse: {text}"));
            return Err(Unwind::Error(ExitStatus::USAGE));
        };
        let saved = self.source.replace(Arc::from(text));
        let result = self.run_list(&list, io);
        self.source = saved;
        result
    }

    /// True if `NO_COLOR` is set to something that is not empty.
    fn no_color(&self) -> bool {
        self.vars.get("NO_COLOR").is_some_and(|v| !v.is_empty())
    }

    /// Handles `__omsh_color name args...`: returns the words of the command to run, which are
    /// `name args...` with `--color=auto` after the name if it should be there.
    pub(crate) fn colored_words(
        &mut self,
        words: &[Vec<u8>],
        io: &Io,
    ) -> Result<Vec<Vec<u8>>, Unwind> {
        let Some(name) = words.get(1) else {
            io.say(format!("oh-my-shell: {INTERNAL}: needs a command name"));
            return Err(Unwind::Error(ExitStatus::USAGE));
        };
        let mut result = words[1..].to_vec();
        let Some(probe) = probe_for(name) else {
            return Ok(result);
        };
        if self.no_color() {
            return Ok(result);
        }
        let search_path = self
            .path_override
            .clone()
            .unwrap_or_else(|| self.vars.search_path(&[]));
        let Lookup::Found(file) =
            self.paths
                .lookup(name, &search_path, self.cwd_override.as_deref())
        else {
            // Not found or not executable: the normal error comes from running it.
            return Ok(result);
        };
        let key = if file.is_relative() {
            match self
                .cwd_override
                .clone()
                .or_else(|| std::env::current_dir().ok())
            {
                Some(base) => base.join(file),
                None => file,
            }
        } else {
            file
        };
        let known = self
            .color_probes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&key)
            .copied();
        let supported = match known {
            Some(answer) => answer,
            None => match self.run_probe(probe, io)? {
                Some(answer) => {
                    self.color_probes
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .insert(key, answer);
                    answer
                }
                None => false,
            },
        };
        if supported {
            result.insert(1, b"--color=auto".to_vec());
        }
        Ok(result)
    }

    /// Runs a probe. `Some(true)` if it succeeded, `Some(false)` if the program ran and said no,
    /// `None` if the answer is not trustworthy (killed by a signal, or not startable): that is
    /// not remembered. Ctrl+C is passed on. The probe does not show up in `set -x`, cannot end
    /// the shell under `set -e`, and leaves `$?` as it was.
    fn run_probe(&mut self, probe: &str, io: &Io) -> Result<Option<bool>, Unwind> {
        let saved_status = self.status;
        let saved_xtrace = std::mem::replace(&mut self.options.xtrace, false);
        let result = self.ignoring_errexit(true, |shell| shell.run_internal(probe, io));
        self.options.xtrace = saved_xtrace;
        self.status = saved_status;
        let status = result?;
        if self.interrupted() {
            return Err(Unwind::Interrupted);
        }
        Ok(match status.code() {
            0 => Some(true),
            1..=125 => Some(false),
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests;
