//! `j`: jump to a folder the shell has been in (blueprint §10, smart jump).
//!
//! What it knows about folders lives outside this crate: the executor may not depend on the
//! history crate (the workspace's dependency direction), so the program that owns the shell
//! hands it something that implements [`JumpCommand`]. It answers with the folders that match
//! the words, best first. The builtin then picks the first one that is still a folder and is
//! not the one the shell is in, and changes into it with `cd`, so `PWD` and `OLDPWD` are kept
//! as for any `cd`, and `j -` is `cd -`. Nothing is printed on success.

use std::fmt;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::Arc;

use crate::exec::Flow;
use crate::io::Io;
use crate::shell::Shell;
use crate::status::ExitStatus;

use super::cd;

const USAGE: &str = "oh-my-shell: j: usage: j [--] WORD... (jump to a visited directory), j -";

/// The shell's memory of visited folders.
pub trait JumpCommand: Send + Sync {
    /// The folders that match `words`, the most promising first.
    fn candidates(&self, words: &[String]) -> Vec<String>;
}

/// The shell's handle on its jump command. Shared by every copy of the shell.
#[derive(Clone, Default)]
pub(crate) struct JumpHook(pub(crate) Option<Arc<dyn JumpCommand>>);

impl fmt::Debug for JumpHook {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if self.0.is_some() {
            "JumpHook(set)"
        } else {
            "JumpHook(none)"
        })
    }
}

impl Shell {
    /// Gives the shell its `j` builtin. Without one, `j` says that it is not available.
    pub fn set_jump(&mut self, command: Arc<dyn JumpCommand>) {
        self.jump = JumpHook(Some(command));
    }
}

pub(super) fn jump(shell: &mut Shell, args: &[Vec<u8>], io: &Io) -> Flow {
    let Some(command) = shell.jump.0.clone() else {
        io.say("oh-my-shell: j: not available in this shell");
        return Ok(ExitStatus(1));
    };
    // `j -` is `cd -`.
    if args == [b"-".to_vec()] {
        return cd::cd(shell, args, io);
    }
    let words = match args.split_first() {
        Some((first, rest)) if first == b"--" => rest,
        _ => args,
    };
    let usage = words.is_empty() || (words == args && words[0].starts_with(b"-"));
    if usage {
        io.say(USAGE);
        return Ok(ExitStatus::USAGE);
    }
    let words: Vec<String> = words
        .iter()
        .map(|word| String::from_utf8_lossy(word).into_owned())
        .collect();
    let here = shell.logical_cwd();
    let target = command.candidates(&words).into_iter().find(|path| {
        let path = Path::new(path);
        path != here && path.is_dir()
    });
    match target {
        Some(path) => cd::cd(
            shell,
            &[
                b"--".to_vec(),
                Path::new(&path).as_os_str().as_bytes().to_vec(),
            ],
            io,
        ),
        None => {
            io.say(format!(
                "oh-my-shell: j: no visited directory matches: {}",
                words.join(" ")
            ));
            Ok(ExitStatus(1))
        }
    }
}
