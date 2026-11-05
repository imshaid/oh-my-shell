//! Redirections: `> file`, `2>&1`, `<< EOF` and the rest.
//!
//! They are applied from left to right to a copy of the command's table of files ([`Io`]).
//! That is why `cmd >f 2>&1` sends both to `f`, but `cmd 2>&1 >f` sends the errors to where
//! the output went before: `2>&1` copies what descriptor 1 is at that moment.

use std::io::Write as _;
use std::os::fd::OwnedFd;
use std::sync::Arc;

use nix::errno::Errno;
use nix::fcntl::{OFlag, open};
use nix::sys::memfd::{MFdFlags, memfd_create};
use nix::sys::stat::Mode;
use nix::unistd::{Whence, lseek};
use omsh_syntax::ast::{Redirect, RedirectKind, RedirectOp};

use crate::expand::{ExpandError, Expander};
use crate::io::Io;
use crate::shell::Shell;

/// Why a redirection could not be made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RedirectError {
    /// A file that cannot be opened, a bad descriptor number, and the like. The text is the
    /// message for the user.
    Failed(String),
    /// The word after the operator could not be expanded.
    Expand(ExpandError),
}

impl Shell {
    /// Applies `redirects` to a copy of `io`. A target is expanded in the way of an
    /// assignment value: no field splitting and no pathname expansion.
    pub(crate) fn apply_redirects(
        &mut self,
        redirects: &[Redirect],
        io: &Io,
    ) -> Result<Io, RedirectError> {
        let mut table = io.clone();
        for redirect in redirects {
            self.apply_one(redirect, io, &mut table)?;
        }
        Ok(table)
    }

    fn apply_one(
        &mut self,
        redirect: &Redirect,
        io: &Io,
        table: &mut Io,
    ) -> Result<(), RedirectError> {
        match &redirect.kind {
            RedirectKind::HereDoc(doc) => {
                let body = Expander { shell: self, io }
                    .heredoc_body(&doc.body)
                    .map_err(RedirectError::Expand)?;
                let file = memory_file(&body)?;
                table.set(redirect.fd.unwrap_or(0), Arc::new(file));
            }
            RedirectKind::Target { op, word } => {
                let target = Expander { shell: self, io }
                    .target(word)
                    .map_err(RedirectError::Expand)?;
                self.apply_target(*op, redirect.fd, &target, table)?;
            }
        }
        Ok(())
    }

    fn apply_target(
        &self,
        op: RedirectOp,
        fd: Option<u32>,
        target: &[u8],
        table: &mut Io,
    ) -> Result<(), RedirectError> {
        let output = OFlag::O_WRONLY | OFlag::O_CREAT;
        match op {
            RedirectOp::Input => {
                let file = self.open_file(target, OFlag::O_RDONLY)?;
                table.set(fd.unwrap_or(0), Arc::new(file));
            }
            RedirectOp::Output | RedirectOp::Clobber => {
                let file = self.open_file(target, output | OFlag::O_TRUNC)?;
                table.set(fd.unwrap_or(1), Arc::new(file));
            }
            RedirectOp::Append => {
                let file = self.open_file(target, output | OFlag::O_APPEND)?;
                table.set(fd.unwrap_or(1), Arc::new(file));
            }
            RedirectOp::ReadWrite => {
                let file = self.open_file(target, OFlag::O_RDWR | OFlag::O_CREAT)?;
                table.set(fd.unwrap_or(0), Arc::new(file));
            }
            RedirectOp::OutputAndError | RedirectOp::AppendAndError => {
                let how = if op == RedirectOp::OutputAndError {
                    OFlag::O_TRUNC
                } else {
                    OFlag::O_APPEND
                };
                let file = Arc::new(self.open_file(target, output | how)?);
                table.set(1, Arc::clone(&file));
                table.set(2, file);
            }
            RedirectOp::DupInput | RedirectOp::DupOutput => {
                let fd = fd.unwrap_or(u32::from(op == RedirectOp::DupOutput));
                if target == b"-" {
                    table.close(fd);
                } else {
                    let source = parse_fd(target).ok_or_else(|| {
                        RedirectError::Failed(format!(
                            "{}: bad fd number",
                            String::from_utf8_lossy(target)
                        ))
                    })?;
                    let slot = table.get(source).cloned().ok_or_else(|| {
                        RedirectError::Failed(format!("{source}: bad file descriptor"))
                    })?;
                    table.set(fd, slot);
                }
            }
            RedirectOp::HereString => {
                let mut text = target.to_vec();
                text.push(b'\n');
                table.set(fd.unwrap_or(0), Arc::new(memory_file(&text)?));
            }
        }
        Ok(())
    }

    fn open_file(&self, name: &[u8], flags: OFlag) -> Result<OwnedFd, RedirectError> {
        open(
            &self.resolve(name),
            flags | OFlag::O_CLOEXEC,
            Mode::from_bits_truncate(0o666),
        )
        .map_err(|errno| {
            RedirectError::Failed(format!(
                "{}: {}",
                String::from_utf8_lossy(name),
                errno.desc()
            ))
        })
    }
}

/// A descriptor number written as digits only.
fn parse_fd(text: &[u8]) -> Option<u32> {
    if text.is_empty() || !text.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(text).ok()?.parse().ok()
}

/// A file that holds `bytes`, positioned at the start, for a command to read. It lives in
/// memory (`memfd_create`), so it has no size limit like a pipe and needs no helper process.
fn memory_file(bytes: &[u8]) -> Result<OwnedFd, RedirectError> {
    let failed = |errno: Errno| {
        RedirectError::Failed(format!("cannot make a here-document: {}", errno.desc()))
    };
    let fd = memfd_create(c"omsh-heredoc", MFdFlags::MFD_CLOEXEC).map_err(failed)?;
    let mut file = std::fs::File::from(fd);
    file.write_all(bytes)
        .map_err(|e| failed(Errno::from_raw(e.raw_os_error().unwrap_or(0))))?;
    let fd = OwnedFd::from(file);
    lseek(&fd, 0, Whence::SeekSet).map_err(failed)?;
    Ok(fd)
}
