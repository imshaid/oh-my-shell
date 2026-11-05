//! The shell's variables: their values, and which of them child processes receive.

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;

use crate::path::DEFAULT_PATH;

/// One variable. Names and values are bytes, because the environment can hold any bytes
/// except NUL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Variable {
    name: Vec<u8>,
    /// Nothing for a name that was exported before it got a value (`export X`).
    value: Option<Vec<u8>>,
    /// True if child processes receive it.
    exported: bool,
}

/// A variable as `set` and `export -p` list it: name, value (none for an exported name
/// without one) and whether it is exported.
pub type Entry<'a> = (&'a [u8], Option<&'a [u8]>, bool);

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
        self.find(name.as_bytes()).and_then(|v| v.value.as_deref())
    }

    /// True if child processes receive the variable.
    #[must_use]
    pub fn is_exported(&self, name: &str) -> bool {
        self.find(name.as_bytes()).is_some_and(|v| v.exported)
    }

    /// Marks a variable for export. A name that is not set stays unset, and is exported if
    /// it gets a value later.
    pub fn export(&mut self, name: &str) {
        if let Some(var) = self.find_mut(name.as_bytes()) {
            var.exported = true;
        } else {
            self.vars.push(Variable {
                name: name.as_bytes().to_vec(),
                value: None,
                exported: true,
            });
        }
    }

    /// Removes a variable.
    pub fn unset(&mut self, name: &str) {
        self.vars.retain(|v| v.name != name.as_bytes());
    }

    /// The set variables, sorted by name, with whether each is exported. This is what `set`
    /// and `export -p` print. A name that is exported and has no value is included with
    /// `None`.
    #[must_use]
    pub fn sorted(&self) -> Vec<Entry<'_>> {
        let mut out: Vec<_> = self
            .vars
            .iter()
            .map(|v| (v.name.as_slice(), v.value.as_deref(), v.exported))
            .collect();
        out.sort_by_key(|(name, ..)| *name);
        out
    }

    /// A copy of a variable as it is now, to put back with [`restore`](Self::restore).
    #[must_use]
    pub fn snapshot(&self, name: &str) -> Option<Variable> {
        self.find(name.as_bytes()).cloned()
    }

    /// Puts a variable back as `snapshot` saw it: with its old value and export flag, or
    /// removed if it did not exist.
    pub fn restore(&mut self, name: &str, saved: Option<Variable>) {
        match saved {
            Some(var) => {
                if let Some(slot) = self.find_mut(name.as_bytes()) {
                    *slot = var;
                } else {
                    self.vars.push(var);
                }
            }
            None => self.unset(name),
        }
    }

    /// Sets a variable. One that is exported stays exported; a new one is not exported.
    pub fn set(&mut self, name: &str, value: Vec<u8>) {
        self.set_bytes(name.as_bytes(), value);
    }

    fn set_bytes(&mut self, name: &[u8], value: Vec<u8>) {
        if let Some(var) = self.find_mut(name) {
            var.value = Some(value);
        } else {
            self.vars.push(Variable {
                name: name.to_vec(),
                value: Some(value),
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
            if let (false, Some(value)) = (replaced, &var.value) {
                push_entry(&mut out, &var.name, value);
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
    fn export_marks_a_variable_and_keeps_a_name_without_a_value() {
        let mut vars = Variables::default();
        vars.export("X");
        assert_eq!(vars.get("X"), None);
        assert!(vars.is_exported("X"));
        assert!(vars.envp(&[]).is_empty());
        vars.set("X", b"1".to_vec());
        assert_eq!(strings(&vars.envp(&[])), ["X=1"]);
    }

    #[test]
    fn unset_removes_a_variable() {
        let mut vars = Variables::from_pairs([("A", "1"), ("B", "2")]);
        vars.unset("A");
        assert_eq!(vars.get("A"), None);
        assert_eq!(strings(&vars.envp(&[])), ["B=2"]);
    }

    #[test]
    fn restore_puts_back_the_value_and_the_export_flag() {
        let mut vars = Variables::from_pairs([("A", "1")]);
        let old_a = vars.snapshot("A");
        let old_b = vars.snapshot("B");
        vars.set("A", b"2".to_vec());
        vars.set("B", b"3".to_vec());
        vars.unset("A");
        vars.restore("A", old_a);
        vars.restore("B", old_b);
        assert_eq!(vars.get("A"), Some(&b"1"[..]));
        assert!(vars.is_exported("A"));
        assert_eq!(vars.get("B"), None);
    }

    #[test]
    fn sorted_lists_names_in_order() {
        let mut vars = Variables::from_pairs([("B", "2"), ("A", "1")]);
        vars.export("C");
        let names: Vec<_> = vars.sorted().iter().map(|(n, ..)| n.to_vec()).collect();
        assert_eq!(names, [b"A".to_vec(), b"B".to_vec(), b"C".to_vec()]);
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
