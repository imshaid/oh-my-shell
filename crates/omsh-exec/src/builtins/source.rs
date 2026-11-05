//! `eval` and `.` (`source`): run text as shell code in the current shell.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

use omsh_syntax::parse;

use crate::check::check_list;
use crate::exec::{Flow, Unwind};
use crate::io::Io;
use crate::shell::Shell;
use crate::status::ExitStatus;

impl Shell {
    /// Runs `src` one complete command at a time, so that what an earlier command defines (an
    /// alias, a function) is there for the later ones, as it is for lines typed one by one. A
    /// mistake is reported as coming from `what` and gives status 2; what ran before it stays
    /// done and nothing after it runs.
    fn run_text(&mut self, src: &str, what: &str, io: &Io) -> Flow {
        let mut status = self.status;
        let mut pending = String::new();
        let mut lines = src.split_inclusive('\n').peekable();
        while let Some(line) = lines.next() {
            pending.push_str(line);
            let expanded = self.expand_aliases(&pending).into_owned();
            let list = match parse(&expanded) {
                Ok(list) => list,
                // More lines may finish it.
                Err(failure) if failure.is_incomplete() && lines.peek().is_some() => continue,
                Err(failure) => {
                    let report = failure.to_diagnostic(&expanded);
                    io.say(format!("oh-my-shell: {what}: {}", report.message()));
                    return Ok(ExitStatus::USAGE);
                }
            };
            pending.clear();
            if let Err(not_yet) = check_list(&list) {
                io.say(format!(
                    "oh-my-shell: {what}: {} cannot run yet",
                    not_yet.what
                ));
                return Ok(ExitStatus::USAGE);
            }
            status = self.run_list(&list, io)?;
            self.status = status;
        }
        Ok(status)
    }
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
    let result = shell.run_text(&src, &shown, io);
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
