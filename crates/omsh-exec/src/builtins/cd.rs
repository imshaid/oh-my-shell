//! `cd` and `pwd`.
//!
//! The shell keeps the directory it believes it is in as `$PWD`, with symbolic links as the
//! user wrote them (the *logical* directory). The kernel knows the *physical* one, with every
//! link resolved. `-L` (the default) works on the logical path and `-P` on the physical one.
//!
//! Only the main shell changes the directory of the process. A copy of the shell (a subshell,
//! a pipeline stage, a `$(...)`) records the new directory for itself and its children;
//! see `Shell::cwd_override`.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

use nix::errno::Errno;
use nix::unistd::{AccessFlags, access, chdir};

use crate::exec::Flow;
use crate::io::Io;
use crate::shell::Shell;
use crate::status::ExitStatus;

impl Shell {
    /// The directory the shell believes it is in.
    pub(crate) fn logical_cwd(&self) -> PathBuf {
        if let Some(pwd) = self.vars.get("PWD")
            && pwd.starts_with(b"/")
        {
            return PathBuf::from(OsStr::from_bytes(pwd));
        }
        self.cwd_override
            .clone()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/"))
    }
}

/// Removes `.` and `..` from an absolute path by reading it as text. `a/b/..` becomes `a`,
/// whatever `b` is.
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::from("/");
    for component in path.components() {
        match component {
            Component::Normal(name) => out.push(name),
            Component::ParentDir => {
                out.pop();
            }
            Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
        }
    }
    out
}

/// Parses the options `-L` and `-P`. Returns whether `-P` is in effect and the rest.
fn options(args: &[Vec<u8>]) -> (bool, &[Vec<u8>]) {
    let mut physical = false;
    let mut rest = args;
    while let Some(arg) = rest.first() {
        match arg.as_slice() {
            b"-L" => physical = false,
            b"-P" => physical = true,
            b"--" => {
                rest = &rest[1..];
                break;
            }
            _ => break,
        }
        rest = &rest[1..];
    }
    (physical, rest)
}

/// `cd [-L|-P] [dir|-]`.
pub(super) fn cd(shell: &mut Shell, args: &[Vec<u8>], io: &Io) -> Flow {
    let fail = |message: String| {
        io.say(format!("oh-my-shell: cd: {message}"));
        Ok(ExitStatus(1))
    };
    let (physical, rest) = options(args);
    if rest.len() > 1 {
        return fail("too many arguments".to_owned());
    }
    let mut print = false;
    let target = match rest.first().map(Vec::as_slice) {
        None => match shell.vars.get("HOME") {
            Some(home) if !home.is_empty() => home.to_vec(),
            _ => return fail("HOME not set".to_owned()),
        },
        Some(b"-") => match shell.vars.get("OLDPWD") {
            Some(old) if !old.is_empty() => {
                print = true;
                old.to_vec()
            }
            _ => return fail("OLDPWD not set".to_owned()),
        },
        Some(dir) => dir.to_vec(),
    };
    if target.is_empty() {
        return Ok(ExitStatus::SUCCESS);
    }

    let old = shell.logical_cwd();
    let joined = old.join(OsStr::from_bytes(&target));
    let mut new = lexical(&joined);
    let shown = String::from_utf8_lossy(&target).into_owned();
    if physical {
        match std::fs::canonicalize(&new) {
            Ok(real) => new = real,
            Err(error) => {
                let errno = Errno::from_raw(error.raw_os_error().unwrap_or(0));
                return fail(format!("{shown}: {}", errno.desc()));
            }
        }
    }

    if shell.in_subshell {
        if let Err(errno) = check_directory(&new) {
            return fail(format!("{shown}: {}", errno.desc()));
        }
        shell.cwd_override = Some(new.clone());
    } else if let Err(errno) = chdir(&new) {
        return fail(format!("{shown}: {}", errno.desc()));
    }

    shell
        .vars
        .set("OLDPWD", old.as_os_str().as_bytes().to_vec());
    shell.vars.set("PWD", new.as_os_str().as_bytes().to_vec());
    if print {
        let mut line = new.as_os_str().as_bytes().to_vec();
        line.push(b'\n');
        return shell.emit(io, 1, &line);
    }
    Ok(ExitStatus::SUCCESS)
}

/// What `chdir` would check, without changing the directory of the process.
fn check_directory(path: &Path) -> Result<(), Errno> {
    let meta = std::fs::metadata(path)
        .map_err(|e| Errno::from_raw(e.raw_os_error().unwrap_or(Errno::ENOENT as i32)))?;
    if !meta.is_dir() {
        return Err(Errno::ENOTDIR);
    }
    access(path, AccessFlags::X_OK)
}

/// `pwd [-L|-P]`.
pub(super) fn pwd(shell: &Shell, args: &[Vec<u8>], io: &Io) -> Flow {
    let (physical, rest) = options(args);
    if !rest.is_empty() {
        io.say("oh-my-shell: pwd: too many arguments");
        return Ok(ExitStatus(1));
    }
    let logical = shell.logical_cwd();
    let dir = if physical {
        match std::fs::canonicalize(&logical) {
            Ok(real) => real,
            Err(error) => {
                io.say(format!("oh-my-shell: pwd: {error}"));
                return Ok(ExitStatus(1));
            }
        }
    } else {
        logical
    };
    let mut line = dir.as_os_str().as_bytes().to_vec();
    line.push(b'\n');
    shell.emit(io, 1, &line)
}
