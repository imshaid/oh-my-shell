//! `read [-r] [-p prompt] [name...]`: reads one line from standard input into variables.
//!
//! The line is read one byte at a time from descriptor 0 of the command's table of files, so
//! that nothing after the line is taken from a program that reads the same input later
//! (`while read l; do ...; done < file` and `{ read a; cat; } < file` both depend on this).

use std::io::IsTerminal;
use std::os::fd::AsFd;

use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::unistd::read as read_fd;

use super::is_name;
use crate::exec::{Flow, Unwind};
use crate::io::Io;
use crate::shell::Shell;
use crate::signals;
use crate::status::ExitStatus;

/// One byte of the line, and whether a backslash protected it from field splitting.
type Char = (u8, bool);

/// How long to wait for input before looking at the Ctrl+C flag again. The signal handler
/// restarts system calls (`SA_RESTART`), so a plain blocking `read` would never notice it.
const WAIT_MS: u16 = 100;

/// `read`.
pub(super) fn read(shell: &mut Shell, args: &[Vec<u8>], io: &Io) -> Flow {
    let mut raw = false;
    let mut prompt: Option<&[u8]> = None;
    let mut rest = args;
    while let Some(arg) = rest.first() {
        match arg.as_slice() {
            b"-r" => raw = true,
            b"-p" => {
                let Some(text) = rest.get(1) else {
                    io.say("oh-my-shell: read: -p needs an argument");
                    return Ok(ExitStatus::USAGE);
                };
                prompt = Some(text);
                rest = &rest[1..];
            }
            b"--" => {
                rest = &rest[1..];
                break;
            }
            other if other.len() > 1 && other[0] == b'-' => {
                io.say(format!(
                    "oh-my-shell: read: {}: unknown option",
                    String::from_utf8_lossy(other)
                ));
                return Ok(ExitStatus::USAGE);
            }
            _ => break,
        }
        rest = &rest[1..];
    }
    if let Some(bad) = rest.iter().find(|name| !is_name(name)) {
        io.say(format!(
            "oh-my-shell: read: {}: not a valid name",
            String::from_utf8_lossy(bad)
        ));
        return Ok(ExitStatus::USAGE);
    }
    let Some(input) = io.get(0) else {
        io.say("oh-my-shell: read: 0: bad file descriptor");
        return Ok(ExitStatus(1));
    };
    if let Some(prompt) = prompt
        && input.as_fd().is_terminal()
    {
        let _ = io.write_all(2, prompt);
    }

    let (line, got_newline) = read_line(io, raw)?;
    let ifs = shell.vars.get("IFS").unwrap_or(b" \t\n").to_vec();
    if rest.is_empty() {
        let value: Vec<u8> = line.iter().map(|(b, _)| *b).collect();
        shell.vars.set("REPLY", value);
    } else {
        let names: Vec<String> = rest
            .iter()
            .map(|n| String::from_utf8_lossy(n).into_owned())
            .collect();
        for (name, value) in names.iter().zip(split_fields(&line, &ifs, names.len())) {
            shell.vars.set(name, value);
        }
    }
    // The end of the input before a line ending gives 1, but the variables are still set.
    Ok(if got_newline {
        ExitStatus::SUCCESS
    } else {
        ExitStatus(1)
    })
}

/// Reads one line. Returns its bytes and whether a line ending was found.
///
/// Unless `raw`, a backslash makes the next byte ordinary (it is marked as protected) and a
/// backslash before a line ending joins the next line on.
fn read_line(io: &Io, raw: bool) -> Result<(Vec<Char>, bool), Unwind> {
    let mut line = Vec::new();
    let mut backslash = false;
    loop {
        let Some(byte) = read_byte(io)? else {
            return Ok((line, false));
        };
        // dash ignores NUL bytes.
        if byte == 0 {
            continue;
        }
        if backslash {
            backslash = false;
            if byte != b'\n' {
                line.push((byte, true));
            }
            continue;
        }
        if !raw && byte == b'\\' {
            backslash = true;
            continue;
        }
        if byte == b'\n' {
            return Ok((line, true));
        }
        line.push((byte, false));
    }
}

/// Reads one byte from descriptor 0, waiting in short steps so that Ctrl+C can interrupt.
fn read_byte(io: &Io) -> Result<Option<u8>, Unwind> {
    let Some(slot) = io.get(0) else {
        return Ok(None);
    };
    let mut byte = [0_u8; 1];
    loop {
        if signals::interrupted() {
            return Err(Unwind::Interrupted);
        }
        let mut fds = [PollFd::new(slot.as_fd(), PollFlags::POLLIN)];
        match poll(&mut fds, PollTimeout::from(WAIT_MS)) {
            Ok(0) | Err(Errno::EINTR) => continue,
            // An error here shows up again in `read`, with the details.
            Ok(_) | Err(_) => {}
        }
        match read_fd(slot.as_fd(), &mut byte) {
            Ok(0) => return Ok(None),
            Ok(_) => return Ok(Some(byte[0])),
            Err(Errno::EINTR | Errno::EAGAIN) => {}
            // An unreadable input ends like a closed one.
            Err(_) => return Ok(None),
        }
    }
}

/// Cuts `line` into `count` fields the way `read` does: the last field takes everything that is
/// left, with trailing white space of `$IFS` removed.
fn split_fields(line: &[Char], ifs: &[u8], count: usize) -> Vec<Vec<u8>> {
    let is_delim = |i: usize| !line[i].1 && ifs.contains(&line[i].0);
    let is_white = |i: usize| is_delim(i) && matches!(line[i].0, b' ' | b'\t' | b'\n');
    let is_other = |i: usize| is_delim(i) && !is_white(i);
    let text = |from: usize, to: usize| -> Vec<u8> { line[from..to].iter().map(|c| c.0).collect() };

    let len = line.len();
    let mut fields = Vec::with_capacity(count);
    let mut i = 0;
    while i < len && is_white(i) {
        i += 1;
    }
    while fields.len() < count {
        if i >= len {
            fields.push(Vec::new());
            continue;
        }
        if fields.len() + 1 == count {
            let mut end = len;
            while end > i && is_white(end - 1) {
                end -= 1;
            }
            // A single trailing delimiter that is not white space ends the last field too
            // (`a:b:` gives `b`), but two of them stay (`a:b::` gives `b::`).
            if end > i && is_other(end - 1) && !(i..end - 1).any(is_other) {
                end -= 1;
                while end > i && is_white(end - 1) {
                    end -= 1;
                }
            }
            fields.push(text(i, end));
            continue;
        }
        let start = i;
        while i < len && !is_delim(i) {
            i += 1;
        }
        fields.push(text(start, i));
        while i < len && is_white(i) {
            i += 1;
        }
        if i < len && is_other(i) {
            i += 1;
            while i < len && is_white(i) {
                i += 1;
            }
        }
    }
    fields
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(text: &str) -> Vec<Char> {
        text.bytes().map(|b| (b, false)).collect()
    }

    fn fields(text: &str, ifs: &str, count: usize) -> Vec<String> {
        split_fields(&plain(text), ifs.as_bytes(), count)
            .into_iter()
            .map(|f| String::from_utf8(f).unwrap())
            .collect()
    }

    #[test]
    fn white_space_separates_and_the_last_name_takes_the_rest() {
        assert_eq!(fields("  a  b   c d  ", " \t\n", 2), ["a", "b   c d"]);
        assert_eq!(fields("a", " \t\n", 3), ["a", "", ""]);
        assert_eq!(fields("", " \t\n", 2), ["", ""]);
    }

    #[test]
    fn other_delimiters_each_count() {
        assert_eq!(fields("x:y::", ":", 2), ["x", "y::"]);
        assert_eq!(fields("x:y:", ":", 2), ["x", "y"]);
        assert_eq!(fields(":a", ":", 2), ["", "a"]);
        assert_eq!(fields("a::b", ":", 3), ["a", "", "b"]);
    }

    #[test]
    fn white_space_around_another_delimiter_belongs_to_it() {
        assert_eq!(fields("a : b", " :", 2), ["a", "b"]);
        assert_eq!(fields("a :: b", " :", 3), ["a", "", "b"]);
    }

    #[test]
    fn a_protected_byte_is_never_a_delimiter() {
        let line = vec![(b'a', false), (b' ', true), (b'b', false)];
        assert_eq!(
            split_fields(&line, b" \t\n", 2),
            vec![b"a b".to_vec(), Vec::new()]
        );
    }
}
