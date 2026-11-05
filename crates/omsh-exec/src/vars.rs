//! The shell's variables: their values, and which of them child processes receive.

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;

use crate::path::DEFAULT_PATH;

/// One variable. Names and values are bytes, because the environment can hold any bytes
/// except NUL.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Variable {
    name: Vec<u8>,
    value: Vec<u8>,
    /// True if child processes receive it.
    exported: bool,
}

/// The variable table, in the order the variables were created.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Variables {
    vars: Vec<Variable>,
}

impl Variables {
    /// The environment of this process. All of it is exported.
    #[must_use]
    pub fn from_process() -> Self {
        Self::from_pairs(
            std::env::vars_os().map(|(n, v)| (n.as_bytes().to_vec(), v.as_bytes().to_vec())),
        )
    }

    /// Builds an exported table from name and value pairs. If a name repeats, the last one
    /// wins.
    #[must_use]
    pub fn from_pairs<N, V>(pairs: impl IntoIterator<Item = (N, V)>) -> Self
    where
        N: AsRef<[u8]>,
        V: AsRef<[u8]>,
    {
        let mut out = Self::default();
        for (name, value) in pairs {
            out.set_bytes(name.as_ref(), value.as_ref().to_vec());
            if let Some(var) = out.find_mut(name.as_ref()) {
                var.exported = true;
            }
        }
        out
    }

    fn find(&self, name: &[u8]) -> Option<&Variable> {
        self.vars.iter().find(|v| v.name == name)
    }

    fn find_mut(&mut self, name: &[u8]) -> Option<&mut Variable> {
        self.vars.iter_mut().find(|v| v.name == name)
    }

    /// The value of a variable, or nothing if it is not set.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&[u8]> {
        self.find(name.as_bytes()).map(|v| v.value.as_slice())
    }

    /// Sets a variable. One that is exported stays exported; a new one is not exported.
    pub fn set(&mut self, name: &str, value: Vec<u8>) {
        self.set_bytes(name.as_bytes(), value);
    }

    fn set_bytes(&mut self, name: &[u8], value: Vec<u8>) {
        if let Some(var) = self.find_mut(name) {
            var.value = value;
        } else {
            self.vars.push(Variable {
                name: name.to_vec(),
                value,
                exported: false,
            });
        }
    }

    /// The value of `PATH` to search with: `overrides` first (the `PATH=... cmd` form), then
    /// the variable, then the default.
    #[must_use]
    pub fn search_path(&self, overrides: &[(String, Vec<u8>)]) -> Vec<u8> {
        if let Some((_, value)) = overrides.iter().rev().find(|(n, _)| n == "PATH") {
            return value.clone();
        }
        self.get("PATH")
            .map_or_else(|| DEFAULT_PATH.as_bytes().to_vec(), <[u8]>::to_vec)
    }

    /// The `NAME=value` strings for `execve`: the exported variables, with `overrides`
    /// replacing or adding variables for one command. Variables whose text holds a NUL byte
    /// are left out, because `execve` cannot pass them.
    #[must_use]
    pub fn envp(&self, overrides: &[(String, Vec<u8>)]) -> Vec<CString> {
        let mut out = Vec::with_capacity(self.vars.len() + overrides.len());
        for var in self.vars.iter().filter(|v| v.exported) {
            let replaced = overrides.iter().any(|(n, _)| n.as_bytes() == var.name);
            if !replaced {
                push_entry(&mut out, &var.name, &var.value);
            }
        }
        for (i, (name, value)) in overrides.iter().enumerate() {
            // If a name is assigned twice (`A=1 A=2 cmd`), the last one wins.
            let shadowed = overrides[i + 1..].iter().any(|(n, _)| n == name);
            if !shadowed {
                push_entry(&mut out, name.as_bytes(), value);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(entries: &[CString]) -> Vec<String> {
        entries
            .iter()
            .map(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
            .collect()
    }

    fn pair(name: &str, value: &str) -> (String, Vec<u8>) {
        (name.to_owned(), value.as_bytes().to_vec())
    }

    #[test]
    fn envp_keeps_the_variables_in_order() {
        let env = Variables::from_pairs([("A", "1"), ("B", "2")]);
        assert_eq!(strings(&env.envp(&[])), ["A=1", "B=2"]);
    }

    #[test]
    fn an_override_replaces_a_variable_and_adds_new_ones() {
        let env = Variables::from_pairs([("A", "1"), ("B", "2")]);
        let envp = env.envp(&[pair("A", "x"), pair("C", "3")]);
        assert_eq!(strings(&envp), ["B=2", "A=x", "C=3"]);
    }

    #[test]
    fn the_last_of_two_assignments_to_one_name_wins() {
        let env = Variables::from_pairs([("A", "1")]);
        let envp = env.envp(&[pair("A", "x"), pair("A", "y")]);
        assert_eq!(strings(&envp), ["A=y"]);
    }

    #[test]
    fn a_value_with_a_nul_byte_is_left_out() {
        let env = Variables::from_pairs([("A", "1")]);
        let envp = env.envp(&[pair("B", "x\0y")]);
        assert_eq!(strings(&envp), ["A=1"]);
    }

    #[test]
    fn the_search_path_comes_from_an_override_then_the_variable_then_the_default() {
        let env = Variables::from_pairs([("PATH", "/env")]);
        assert_eq!(env.search_path(&[pair("PATH", "/over")]), b"/over");
        assert_eq!(env.search_path(&[]), b"/env");
        assert_eq!(
            Variables::default().search_path(&[]),
            DEFAULT_PATH.as_bytes()
        );
    }

    #[test]
    fn an_empty_path_is_kept_empty() {
        let env = Variables::from_pairs([("PATH", "")]);
        assert_eq!(env.search_path(&[]), b"");
    }

    #[test]
    fn a_new_variable_is_not_exported_but_an_old_one_stays_exported() {
        let mut vars = Variables::from_pairs([("A", "1")]);
        vars.set("A", b"2".to_vec());
        vars.set("B", b"3".to_vec());
        assert_eq!(vars.get("A"), Some(&b"2"[..]));
        assert_eq!(vars.get("B"), Some(&b"3"[..]));
        assert_eq!(strings(&vars.envp(&[])), ["A=2"]);
    }

    #[test]
    fn a_name_that_repeats_in_the_input_is_set_once() {
        let vars = Variables::from_pairs([("A", "1"), ("A", "2")]);
        assert_eq!(strings(&vars.envp(&[])), ["A=2"]);
    }

    #[test]
    fn an_unset_variable_is_nothing_and_an_empty_one_is_something() {
        let vars = Variables::from_pairs([("E", "")]);
        assert_eq!(vars.get("U"), None);
        assert_eq!(vars.get("E"), Some(&b""[..]));
    }

    #[test]
    fn bytes_that_are_not_utf8_survive() {
        let mut vars = Variables::from_pairs([("A", "")]);
        vars.set("A", vec![0xff, b'x']);
        assert_eq!(vars.envp(&[])[0].as_bytes(), b"A=\xffx");
    }
}
