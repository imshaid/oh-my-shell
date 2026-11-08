//! `eval` and `.` (`source`): run text as shell code in the current shell.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use omsh_syntax::parse;

use crate::check::check_list;
use crate::exec::{Flow, Unwind};
use crate::io::Io;
use crate::shell::{Outcome, Shell};
use crate::signals;
use crate::status::ExitStatus;

impl Shell {
    /// Runs `src` one complete command at a time, so that what an earlier command defines (an
    /// alias, a function) is there for the later ones, as it is for lines typed one by one. A
    /// mistake is reported as coming from `what` and gives status 2; what ran before it stays
    /// done and nothing after it runs.
    pub(crate) fn run_text(&mut self, src: &str, what: &str, io: &Io) -> Flow {
        self.run_text_with(src, what, io, false)
    }

    /// Like [`Shell::run_text`], for the text of a file: a mistake is reported as
    /// `what:line:column: message`.
    pub(crate) fn run_file_text(&mut self, src: &str, what: &str, io: &Io) -> Flow {
        self.run_text_with(src, what, io, true)
    }

    fn run_text_with(&mut self, src: &str, what: &str, io: &Io, located: bool) -> Flow {
        let mut status = self.status;
        let mut pending = String::new();
        // The line of `src` where `pending` starts, and how many lines were taken so far.
        let (mut first_line, mut taken) = (1, 0);
        let mut lines = src.split_inclusive('\n').peekable();
        while let Some(line) = lines.next() {
            if pending.is_empty() {
                first_line = taken + 1;
            }
            taken += 1;
            pending.push_str(line);
            let expanded = self.expand_aliases(&pending).into_owned();
            // Where a mistake is, for the message. (After an alias was replaced, the column is
            // that of the replaced text.)
            let source = |span: omsh_syntax::Span| {
                if located {
                    let (line, column) = place(&expanded, span.start);
                    format!("{what}:{}:{column}", first_line + line - 1)
                } else {
                    what.to_owned()
                }
            };
            let list = match parse(&expanded) {
                Ok(list) => list,
                // More lines may finish it.
                Err(failure) if failure.is_incomplete() && lines.peek().is_some() => continue,
                Err(failure) => {
                    let report = failure.to_diagnostic(&expanded);
                    io.say(format!(
                        "oh-my-shell: {}: {}",
                        source(report.span()),
                        report.message()
                    ));
                    return Ok(ExitStatus::USAGE);
                }
            };
            pending.clear();
            if let Err(not_yet) = check_list(&list) {
                io.say(format!(
                    "oh-my-shell: {}: {} cannot run yet",
                    source(not_yet.span),
                    not_yet.what
                ));
                return Ok(ExitStatus::USAGE);
            }
            // The spans of this list index `expanded`, not the line that ran `eval` or `.`.
            let saved = self.source.replace(std::sync::Arc::from(expanded.as_str()));
            let result = self.run_list(&list, io);
            self.source = saved;
            status = result?;
            self.status = status;
        }
        Ok(status)
    }

    /// Runs the startup file (`init.omsh`) in this shell, so that its aliases, functions and
    /// exports stay. It runs like a file read with `.`: `return` ends it, and a mistake in it
    /// is reported as `file:line:column: message`; what ran before the mistake stays done.
    /// A missing file does nothing. A file that cannot be read gives one line.
    ///
    /// `exit` in the file ends the shell, as it does in the startup files of other shells. The
    /// way back in is to start the shell without reading it. Afterwards `$?` is 0, so that the
    /// first prompt does not carry the status of the last line of the file.
    pub fn run_startup_file(&mut self, path: &Path, io: &mut Io) -> Outcome {
        let shown = path.display().to_string();
        let text = match read_startup_file(path) {
            Ok(Some(text)) => text,
            Ok(None) => return Outcome::Continue(self.status),
            Err(reason) => {
                io.say(format!("oh-my-shell: {shown}: {reason}"));
                return Outcome::Continue(self.status);
            }
        };
        signals::clear_interrupt();
        self.func_depth += 1;
        let result = self.run_file_text(&text, &shown, io);
        self.func_depth -= 1;
        // `exec 3>file` in the file changes the files of everything that comes after it.
        io.apply(&self.fd_changes);
        self.fd_changes.clear();
        match result {
            Err(Unwind::Exit(status)) => return Outcome::Exit(status),
            Err(Unwind::Interrupted) => {
                signals::clear_interrupt();
                if self.interactive {
                    let _ = io.write_all(2, b"\n");
                }
            }
            _ => {}
        }
        self.status = ExitStatus::SUCCESS;
        Outcome::Continue(ExitStatus::SUCCESS)
    }
}

/// A startup file is not read if it is larger than this.
const MAX_STARTUP_BYTES: u64 = 1024 * 1024;

/// Reads the startup file. `None` if there is none. It never blocks (a FIFO is refused) and
/// never reads without a limit.
fn read_startup_file(path: &Path) -> Result<Option<String>, String> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::fcntl::OFlag::O_NONBLOCK.bits())
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let meta = file.metadata().map_err(|error| error.to_string())?;
    if !meta.is_file() {
        return Err("not a regular file; skipped".to_owned());
    }
    if meta.len() > MAX_STARTUP_BYTES {
        return Err("larger than 1024 KiB; skipped".to_owned());
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_STARTUP_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| "the file is not valid UTF-8; skipped".to_owned())
}

/// Line and column (both from 1) of a byte offset. The column counts characters.
fn place(text: &str, offset: usize) -> (usize, usize) {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    let before = &text[..offset];
    let line = before.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let start = before.rfind('\n').map_or(0, |index| index + 1);
    (line, before[start..].chars().count() + 1)
}

/// `eval [arg...]`: joins the arguments with spaces and runs the result.
pub(super) fn eval(shell: &mut Shell, args: &[Vec<u8>], io: &Io) -> Flow {
    let joined = args.join(&b' ');
    let Ok(src) = String::from_utf8(joined) else {
        io.say("oh-my-shell: eval: the text is not valid UTF-8");
        return Ok(ExitStatus::USAGE);
    };
    if src.trim().is_empty() {
        return Ok(ExitStatus::SUCCESS);
    }
    shell.run_text(&src, "eval", io)
}

/// `. file [arg...]`: runs the commands of a file. A name without a `/` is searched for in
/// `$PATH`. With arguments, they are `$1`, `$2`, ... while the file runs. `return` ends the
/// file.
pub(super) fn dot(shell: &mut Shell, args: &[Vec<u8>], io: &Io) -> Flow {
    let Some(name) = args.first() else {
        io.say("oh-my-shell: .: filename argument required");
        return Ok(ExitStatus::USAGE);
    };
    let shown = String::from_utf8_lossy(name).into_owned();
    let Some(path) = find_sourced(shell, name) else {
        io.say(format!("oh-my-shell: .: {shown}: not found"));
        return Ok(ExitStatus(1));
    };
    let bytes = match std::fs::read(shell.resolve(path.as_os_str().as_bytes())) {
        Ok(bytes) => bytes,
        Err(error) => {
            io.say(format!("oh-my-shell: .: {shown}: {error}"));
            return Ok(ExitStatus(1));
        }
    };
    let Ok(src) = String::from_utf8(bytes) else {
        io.say(format!(
            "oh-my-shell: .: {shown}: the file is not valid UTF-8"
        ));
        return Ok(ExitStatus::USAGE);
    };

    let saved =
        (args.len() > 1).then(|| std::mem::replace(&mut shell.positional, args[1..].to_vec()));
    shell.func_depth += 1;
    let result = shell.run_file_text(&src, &shown, io);
    shell.func_depth -= 1;
    if let Some(saved) = saved {
        shell.positional = saved;
    }
    match result {
        Err(Unwind::Return(status)) => Ok(status),
        other => other,
    }
}

/// Where `.` reads from: the name itself if it has a `/`, otherwise the first readable
/// regular file with that name in `$PATH`.
fn find_sourced(shell: &Shell, name: &[u8]) -> Option<PathBuf> {
    let readable = |path: &PathBuf| {
        let real = shell.resolve(path.as_os_str().as_bytes());
        std::fs::metadata(real).is_ok_and(|m| m.is_file())
    };
    if name.contains(&b'/') {
        return Some(PathBuf::from(OsStr::from_bytes(name)));
    }
    let search = shell.vars.search_path(&[]);
    for dir in search.split(|b| *b == b':') {
        let dir = if dir.is_empty() { b".".as_slice() } else { dir };
        let candidate = PathBuf::from(OsStr::from_bytes(dir)).join(OsStr::from_bytes(name));
        if readable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A FIFO that no one writes to: opening it for reading would block for ever. The startup
    /// file must be refused at once, so the call runs on a thread that the test can give up on.
    #[test]
    fn a_fifo_is_refused_and_does_not_block_the_shell() {
        let path = std::env::temp_dir().join(format!("omsh-fifo-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        if !std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .is_ok_and(|status| status.success())
        {
            return;
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        let for_thread = path.clone();
        std::thread::spawn(move || drop(sender.send(read_startup_file(&for_thread))));
        let result = receiver.recv_timeout(std::time::Duration::from_secs(10));
        let _ = std::fs::remove_file(&path);
        let result = result.expect("read_startup_file blocked on a FIFO");
        assert_eq!(result, Err("not a regular file; skipped".to_owned()));
    }

    #[test]
    fn a_missing_file_is_none_and_a_big_file_is_refused() {
        let path = std::env::temp_dir().join(format!("omsh-big-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        assert_eq!(read_startup_file(&path), Ok(None));
        std::fs::write(&path, vec![b'#'; 2 * 1024 * 1024]).unwrap();
        let result = read_startup_file(&path);
        let _ = std::fs::remove_file(&path);
        assert_eq!(result, Err("larger than 1024 KiB; skipped".to_owned()));
    }
}
