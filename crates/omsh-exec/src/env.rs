//! The environment that child processes receive.
//!
//! For now this is a copy of the process environment taken at startup. S07 replaces it with
//! the shell's own variable table.

use std::ffi::{CString, OsString};
use std::os::unix::ffi::OsStrExt;

use crate::path::DEFAULT_PATH;

/// Environment variables, in the order the process started with them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Environment {
    vars: Vec<(OsString, OsString)>,
}

impl Environment {
    /// The environment of this process.
    #[must_use]
    pub fn from_process() -> Self {
        Self {
            vars: std::env::vars_os().collect(),
        }
    }

    /// Builds an environment from name and value pairs.
    #[must_use]
    pub fn from_pairs<N, V>(pairs: impl IntoIterator<Item = (N, V)>) -> Self
    where
        N: Into<OsString>,
        V: Into<OsString>,
    {
        Self {
            vars: pairs
                .into_iter()
                .map(|(n, v)| (n.into(), v.into()))
                .collect(),
        }
    }

    /// The value of a variable.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&OsString> {
        self.vars
            .iter()
            .find(|(n, _)| n.as_bytes() == name.as_bytes())
            .map(|(_, v)| v)
    }

    /// The value of `PATH` to search with: `overrides` first (the `PATH=... cmd` form), then
    /// the environment, then the default. A value that is not UTF-8 cannot name a directory
    /// that this shell's text can refer to, so it is read lossily.
    #[must_use]
    pub fn search_path(&self, overrides: &[(String, String)]) -> String {
        if let Some((_, value)) = overrides.iter().rev().find(|(n, _)| n == "PATH") {
            return value.clone();
        }
        self.get("PATH").map_or_else(
            || DEFAULT_PATH.to_owned(),
            |value| value.to_string_lossy().into_owned(),
        )
    }

    /// The `NAME=value` strings for `execve`: this environment, with `overrides` replacing
    /// or adding variables for one command. Variables whose text holds a NUL byte are left
    /// out, because `execve` cannot pass them.
    #[must_use]
    pub fn envp(&self, overrides: &[(String, String)]) -> Vec<CString> {
        let mut out = Vec::with_capacity(self.vars.len() + overrides.len());
        for (name, value) in &self.vars {
            let replaced = overrides
                .iter()
                .any(|(n, _)| n.as_bytes() == name.as_bytes());
            if !replaced {
                push_entry(&mut out, name.as_bytes(), value.as_bytes());
            }
        }
        for (i, (name, value)) in overrides.iter().enumerate() {
            // If a name is assigned twice (`A=1 A=2 cmd`), the last one wins.
            let shadowed = overrides[i + 1..].iter().any(|(n, _)| n == name);
            if !shadowed {
                push_entry(&mut out, name.as_bytes(), value.as_bytes());
            }
        }
        out
    }
}

fn push_entry(out: &mut Vec<CString>, name: &[u8], value: &[u8]) {
    let mut entry = Vec::with_capacity(name.len() + 1 + value.len());
    entry.extend_from_slice(name);
    entry.push(b'=');
    entry.extend_from_slice(value);
    if let Ok(entry) = CString::new(entry) {
        out.push(entry);
    }
}

impl From<Vec<(OsString, OsString)>> for Environment {
    fn from(vars: Vec<(OsString, OsString)>) -> Self {
        Self { vars }
    }
}

impl From<Environment> for Vec<(OsString, OsString)> {
    fn from(env: Environment) -> Self {
        env.vars
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::ffi::OsStringExt;

    use super::*;

    fn strings(entries: &[CString]) -> Vec<String> {
        entries
            .iter()
            .map(|e| {
                OsString::from_vec(e.as_bytes().to_vec())
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    }

    fn pair(name: &str, value: &str) -> (String, String) {
        (name.to_owned(), value.to_owned())
    }

    #[test]
    fn envp_keeps_the_variables_in_order() {
        let env = Environment::from_pairs([("A", "1"), ("B", "2")]);
        assert_eq!(strings(&env.envp(&[])), ["A=1", "B=2"]);
    }

    #[test]
    fn an_override_replaces_a_variable_and_adds_new_ones() {
        let env = Environment::from_pairs([("A", "1"), ("B", "2")]);
        let envp = env.envp(&[pair("A", "x"), pair("C", "3")]);
        assert_eq!(strings(&envp), ["B=2", "A=x", "C=3"]);
    }

    #[test]
    fn the_last_of_two_assignments_to_one_name_wins() {
        let env = Environment::from_pairs([("A", "1")]);
        let envp = env.envp(&[pair("A", "x"), pair("A", "y")]);
        assert_eq!(strings(&envp), ["A=y"]);
    }

    #[test]
    fn a_value_with_a_nul_byte_is_left_out() {
        let env = Environment::from_pairs([("A", "1")]);
        let envp = env.envp(&[pair("B", "x\0y")]);
        assert_eq!(strings(&envp), ["A=1"]);
    }

    #[test]
    fn the_search_path_comes_from_an_override_then_the_environment_then_the_default() {
        let env = Environment::from_pairs([("PATH", "/env")]);
        assert_eq!(env.search_path(&[pair("PATH", "/over")]), "/over");
        assert_eq!(env.search_path(&[]), "/env");
        assert_eq!(Environment::default().search_path(&[]), DEFAULT_PATH);
    }

    #[test]
    fn an_empty_path_is_kept_empty() {
        let env = Environment::from_pairs([("PATH", "")]);
        assert_eq!(env.search_path(&[]), "");
    }
}
