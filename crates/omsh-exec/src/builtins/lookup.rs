//! `type`, `command` and `hash`: finding out what a command name stands for.
//!
//! `command name args` (running a command while skipping functions) is handled where commands
//! are run, in `exec/mod.rs`, because it has to choose what to run. What is here is the part
//! that only looks things up: `command -v`, `command -V`.

use std::path::PathBuf;

use omsh_syntax::ReservedWord;

use super::{Class, classify, quote};
use crate::exec::Flow;
use crate::io::Io;
use crate::path::{DEFAULT_PATH, Lookup};
use crate::shell::Shell;
use crate::status::ExitStatus;

/// What a command name stands for, in the order the shell looks.
enum Found {
    Keyword,
    Alias(String),
    Function,
    Builtin(Class),
    File(PathBuf),
    Missing,
}

fn find(shell: &mut Shell, name: &[u8], default_path: bool) -> Found {
    let Ok(text) = std::str::from_utf8(name) else {
        return Found::Missing;
    };
    if ReservedWord::from_text(text).is_some() {
        return Found::Keyword;
    }
    if let Some(value) = shell.aliases.get(text) {
        return Found::Alias(value.clone());
    }
    // A special builtin is found before a function of the same name.
    if let Some(class @ Class::Special) = classify(name) {
        return Found::Builtin(class);
    }
    if shell.functions.contains_key(text) {
        return Found::Function;
    }
    if let Some(class) = classify(name) {
        return Found::Builtin(class);
    }
    let search = if default_path {
        DEFAULT_PATH.as_bytes().to_vec()
    } else {
        shell.vars.search_path(&[])
    };
    match shell
        .paths
        .lookup(name, &search, shell.cwd_override.as_deref())
    {
        Lookup::Found(path) => Found::File(path),
        Lookup::Denied(_) | Lookup::NotFound => Found::Missing,
    }
}

fn shown(name: &[u8]) -> String {
    String::from_utf8_lossy(name).into_owned()
}

/// The sentence `type` and `command -V` give for a name.
fn sentence(name: &[u8], found: &Found) -> Option<String> {
    let name = shown(name);
    Some(match found {
        Found::Keyword => format!("{name} is a shell keyword"),
        Found::Alias(value) => format!("{name} is an alias for {value}"),
        Found::Function => format!("{name} is a shell function"),
        Found::Builtin(Class::Special) => format!("{name} is a special shell builtin"),
        Found::Builtin(Class::Regular) => format!("{name} is a shell builtin"),
        Found::File(path) => format!("{name} is {}", path.display()),
        Found::Missing => return None,
    })
}

/// `type name...`: says what each name is. A name that is nothing gives 127.
pub(super) fn type_(shell: &mut Shell, args: &[Vec<u8>], io: &Io) -> Flow {
    let mut out = String::new();
    let mut status = ExitStatus::SUCCESS;
    for name in args {
        let found = find(shell, name, false);
        match sentence(name, &found) {
            Some(line) => {
                out.push_str(&line);
                out.push('\n');
            }
            None => {
                // dash says this on standard output, and scripts may read it there.
                out.push_str(&format!("{}: not found\n", shown(name)));
                status = ExitStatus::NOT_FOUND;
            }
        }
    }
    shell.emit(io, 1, out.as_bytes())?;
    Ok(status)
}

/// `command -v name` and `command -V name`. The forms that run a command never get here.
pub(super) fn command(shell: &mut Shell, args: &[Vec<u8>], io: &Io) -> Flow {
    let mut default_path = false;
    let mut mode = None;
    let mut rest = args;
    while let Some(arg) = rest.first() {
        match arg.as_slice() {
            b"--" => {
                rest = &rest[1..];
                break;
            }
            other if other.len() > 1 && other[0] == b'-' => {
                // Letters can be combined: `-pv`.
                for &letter in &other[1..] {
                    match letter {
                        b'p' => default_path = true,
                        b'v' => mode = Some(false),
                        b'V' => mode = Some(true),
                        _ => {
                            io.say(format!(
                                "oh-my-shell: command: {}: unknown option",
                                shown(other)
                            ));
                            return Ok(ExitStatus::USAGE);
                        }
                    }
                }
            }
            _ => break,
        }
        rest = &rest[1..];
    }
    // `command` alone, or with options and no name, does nothing.
    let (Some(verbose), Some(name)) = (mode, rest.first()) else {
        return Ok(ExitStatus::SUCCESS);
    };
    let found = find(shell, name, default_path);
    let line = if verbose {
        sentence(name, &found)
    } else {
        match &found {
            Found::Missing => None,
            Found::Alias(value) => {
                let mut line = format!("alias {}=", shown(name));
                line.push_str(&String::from_utf8_lossy(&quote(value.as_bytes())));
                Some(line)
            }
            Found::File(path) => Some(path.display().to_string()),
            _ => Some(shown(name)),
        }
    };
    match line {
        Some(mut line) => {
            line.push('\n');
            shell.emit(io, 1, line.as_bytes())
        }
        None => {
            // Like dash: "not found" on standard output for `-V`, and 127 for both.
            if verbose {
                shell.emit(io, 1, format!("{}: not found\n", shown(name)).as_bytes())?;
            }
            Ok(ExitStatus::NOT_FOUND)
        }
    }
}

/// `hash [-r] [name...]`: with names, finds them and remembers where; with `-r`, forgets
/// everything; with nothing, lists what is remembered.
pub(super) fn hash(shell: &mut Shell, args: &[Vec<u8>], io: &Io) -> Flow {
    let mut rest = args;
    while let Some(arg) = rest.first() {
        match arg.as_slice() {
            b"-r" => shell.paths.clear(),
            b"--" => {
                rest = &rest[1..];
                break;
            }
            other if other.len() > 1 && other[0] == b'-' => {
                io.say(format!(
                    "oh-my-shell: hash: {}: unknown option",
                    shown(other)
                ));
                return Ok(ExitStatus::USAGE);
            }
            _ => break,
        }
        rest = &rest[1..];
    }
    if rest.is_empty() {
        if args.iter().any(|a| a == b"-r") {
            return Ok(ExitStatus::SUCCESS);
        }
        let mut lines: Vec<_> = shell
            .paths
            .entries()
            .map(|(name, path)| (name.to_vec(), path.display().to_string()))
            .collect();
        lines.sort();
        let out: String = lines.iter().map(|(_, path)| format!("{path}\n")).collect();
        return shell.emit(io, 1, out.as_bytes());
    }
    let mut status = ExitStatus::SUCCESS;
    for name in rest {
        // Builtins, functions and aliases are not remembered, and are not an error either.
        if !matches!(find(shell, name, false), Found::File(_) | Found::Missing) {
            continue;
        }
        let search = shell.vars.search_path(&[]);
        if !matches!(
            shell
                .paths
                .lookup(name, &search, shell.cwd_override.as_deref()),
            Lookup::Found(_)
        ) {
            io.say(format!("oh-my-shell: hash: {}: not found", shown(name)));
            status = ExitStatus(1);
        }
    }
    Ok(status)
}
