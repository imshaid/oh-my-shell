//! "Did you mean ...?" after `oh-my-shell: gti: not found` (blueprint §7.1).
//!
//! Knowing which names are close to a typo needs the list of every program in `PATH` and a
//! measure of distance between names; that lives in the editor crate, which already has the
//! list for Tab completion. The executor may not depend on it, so the program that owns the
//! shell hands over something that implements [`CommandSuggest`], as for `j` and `history`.
//!
//! The line is printed only by an interactive shell that was given such a hook, which happens
//! after the startup file has run. `-c`, scripts and piped input never see it. No network, no
//! process: it is a comparison of names.

use std::fmt;
use std::sync::Arc;

use crate::builtins::builtin_names;
use crate::io::Io;
use crate::shell::Shell;

/// Finds names close to one that was not found.
pub trait CommandSuggest: Send + Sync {
    /// The names that `name` may have been meant as, best first, at most three. `path` is the
    /// value of `PATH` the lookup used; `shell_names` are the shell's builtins, functions and
    /// aliases.
    fn suggest(&self, name: &str, path: &[u8], shell_names: &[String]) -> Vec<String>;
}

/// The shell's handle on its suggester. Shared by every copy of the shell.
#[derive(Clone, Default)]
pub(crate) struct SuggestHook(Option<Arc<dyn CommandSuggest>>);

impl fmt::Debug for SuggestHook {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if self.0.is_some() {
            "SuggestHook(set)"
        } else {
            "SuggestHook(none)"
        })
    }
}

/// `git`, `git or gist`, `git, gist or gitk`.
#[must_use]
pub fn name_list(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [only] => only.clone(),
        [init @ .., last] => format!("{} or {last}", init.join(", ")),
    }
}

/// True if `name` can be printed as it is: no control character, so that a file name in `PATH`
/// cannot write escape sequences to the terminal.
fn printable(name: &str) -> bool {
    !name.is_empty() && !name.chars().any(char::is_control)
}

impl Shell {
    /// Lets an interactive shell say what a name that was not found may have been meant as.
    pub fn set_suggest(&mut self, suggester: Arc<dyn CommandSuggest>) {
        self.suggest = SuggestHook(Some(suggester));
    }

    /// Names close to `name`, best first, at most three, from the shell's hook (none without
    /// one, so `-c` and scripts get nothing). A plan uses this to warn before a step runs.
    #[must_use]
    pub fn suggestions(&self, name: &str) -> Vec<String> {
        let search_path = self
            .path_override
            .clone()
            .unwrap_or_else(|| self.vars.search_path(&[]));
        self.suggestions_in(name, &search_path)
    }

    fn suggestions_in(&self, name: &str, path: &[u8]) -> Vec<String> {
        let Some(suggester) = self.suggest.0.as_ref() else {
            return Vec::new();
        };
        let mut shell_names: Vec<String> = builtin_names().into_iter().map(str::to_owned).collect();
        shell_names.extend(self.function_names());
        shell_names.extend(self.alias_names());
        suggester
            .suggest(name, path, &shell_names)
            .into_iter()
            .filter(|candidate| printable(candidate) && candidate != name)
            .take(3)
            .collect()
    }

    /// Prints the suggestion line after a `not found` line, if there is one to print.
    pub(crate) fn suggest_for(&self, name: &str, path: &[u8], io: &Io) {
        if !self.interactive {
            return;
        }
        let found = self.suggestions_in(name, path);
        if !found.is_empty() {
            io.say(format!("oh-my-shell: did you mean {}?", name_list(&found)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn names_are_joined_in_words() {
        assert_eq!(name_list(&names(&[])), "");
        assert_eq!(name_list(&names(&["git"])), "git");
        assert_eq!(name_list(&names(&["git", "gist"])), "git or gist");
        assert_eq!(
            name_list(&names(&["git", "gist", "gitk"])),
            "git, gist or gitk"
        );
    }

    #[test]
    fn a_name_with_a_control_character_is_not_printed() {
        assert!(printable("git"));
        assert!(!printable("g\x1b[31mit"));
        assert!(!printable("a\nb"));
        assert!(!printable(""));
    }
}
