//! Where the shell keeps its files (blueprint §9): the XDG base directories.
//!
//! Linux only (D10), so this is a few lines of its own and not a crate. The rules come from
//! the XDG Base Directory specification: a `XDG_*_HOME` variable counts only if it is set,
//! not empty and an absolute path; otherwise the default under `$HOME` is used.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The directory name of this program under each base directory.
const APP: &str = "oh-my-shell";

/// The three directories of the program. Each is missing when neither its `XDG_*_HOME`
/// variable nor `HOME` gives a usable path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    config: Option<PathBuf>,
    data: Option<PathBuf>,
    state: Option<PathBuf>,
}

impl Paths {
    /// Works the directories out from environment variables. `get` returns the value of a
    /// variable: the shell passes its own variables (after the login environment was
    /// imported), tests pass a table.
    pub fn from_lookup(get: impl Fn(&str) -> Option<OsString>) -> Self {
        let home = usable(get("HOME"));
        let dir = |variable: &str, under_home: &str| {
            usable(get(variable))
                .or_else(|| home.as_ref().map(|home| home.join(under_home)))
                .map(|base| base.join(APP))
        };
        Self {
            config: dir("XDG_CONFIG_HOME", ".config"),
            data: dir("XDG_DATA_HOME", ".local/share"),
            state: dir("XDG_STATE_HOME", ".local/state"),
        }
    }

    /// `~/.config/oh-my-shell`: `config.toml`, `init.omsh`, `themes/`.
    #[must_use]
    pub fn config_dir(&self) -> Option<&Path> {
        self.config.as_deref()
    }

    /// `~/.local/share/oh-my-shell`: history, trash.
    #[must_use]
    pub fn data_dir(&self) -> Option<&Path> {
        self.data.as_deref()
    }

    /// `~/.local/state/oh-my-shell`: logs and caches.
    #[must_use]
    pub fn state_dir(&self) -> Option<&Path> {
        self.state.as_deref()
    }

    /// The configuration file.
    #[must_use]
    pub fn config_file(&self) -> Option<PathBuf> {
        self.config.as_ref().map(|dir| dir.join("config.toml"))
    }

    /// The startup file, in the shell's own language.
    #[must_use]
    pub fn init_file(&self) -> Option<PathBuf> {
        self.config.as_ref().map(|dir| dir.join("init.omsh"))
    }
}

/// A value the specification accepts: not empty and absolute.
fn usable(value: Option<OsString>) -> Option<PathBuf> {
    value
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty() && path.is_absolute())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(vars: &[(&str, &str)]) -> Paths {
        Paths::from_lookup(|name| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(value))
        })
    }

    #[test]
    fn the_defaults_are_under_home() {
        let paths = paths(&[("HOME", "/home/u")]);
        assert_eq!(
            paths.config_dir(),
            Some(Path::new("/home/u/.config/oh-my-shell"))
        );
        assert_eq!(
            paths.data_dir(),
            Some(Path::new("/home/u/.local/share/oh-my-shell"))
        );
        assert_eq!(
            paths.state_dir(),
            Some(Path::new("/home/u/.local/state/oh-my-shell"))
        );
        assert_eq!(
            paths.config_file(),
            Some(PathBuf::from("/home/u/.config/oh-my-shell/config.toml"))
        );
        assert_eq!(
            paths.init_file(),
            Some(PathBuf::from("/home/u/.config/oh-my-shell/init.omsh"))
        );
    }

    #[test]
    fn an_xdg_variable_wins_over_home() {
        let paths = paths(&[
            ("HOME", "/home/u"),
            ("XDG_CONFIG_HOME", "/cfg"),
            ("XDG_DATA_HOME", "/data"),
            ("XDG_STATE_HOME", "/state"),
        ]);
        assert_eq!(paths.config_dir(), Some(Path::new("/cfg/oh-my-shell")));
        assert_eq!(paths.data_dir(), Some(Path::new("/data/oh-my-shell")));
        assert_eq!(paths.state_dir(), Some(Path::new("/state/oh-my-shell")));
    }

    #[test]
    fn an_empty_or_relative_xdg_variable_is_ignored() {
        let paths = paths(&[
            ("HOME", "/home/u"),
            ("XDG_CONFIG_HOME", ""),
            ("XDG_DATA_HOME", "relative/dir"),
        ]);
        assert_eq!(
            paths.config_dir(),
            Some(Path::new("/home/u/.config/oh-my-shell"))
        );
        assert_eq!(
            paths.data_dir(),
            Some(Path::new("/home/u/.local/share/oh-my-shell"))
        );
    }

    #[test]
    fn without_a_usable_home_only_the_xdg_directories_exist() {
        let none = paths(&[]);
        assert_eq!(none.config_dir(), None);
        assert_eq!(none.config_file(), None);
        assert_eq!(none.init_file(), None);
        let relative_home = paths(&[("HOME", "home/u"), ("XDG_STATE_HOME", "/state")]);
        assert_eq!(relative_home.config_dir(), None);
        assert_eq!(relative_home.data_dir(), None);
        assert_eq!(
            relative_home.state_dir(),
            Some(Path::new("/state/oh-my-shell"))
        );
    }
}
