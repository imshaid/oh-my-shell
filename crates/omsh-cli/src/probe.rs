//! The real file system for the safety layer: `std` for the facts, and the shell's own
//! pathname expansion for patterns, so a count of what `rm *.log` matches is what the shell
//! will really delete.

use std::ffi::OsStr;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use omsh_safety::blast::{Fs, Meta, Names, StdFs};
use omsh_safety::{FsProbe, Probe};

fn expand(pattern: &[u8], active: &[bool], base: &Path) -> Vec<PathBuf> {
    omsh_exec::glob(pattern, active, base)
        .into_iter()
        .map(|name| PathBuf::from(OsStr::from_bytes(&name)))
        .collect()
}

/// What the analyzer asks while it judges a line.
#[derive(Clone, Copy, Debug, Default)]
pub struct ShellProbe;

impl Probe for ShellProbe {
    fn is_nonempty_file(&self, path: &Path) -> bool {
        FsProbe.is_nonempty_file(path)
    }

    fn glob(&self, pattern: &[u8], active: &[bool], base: &Path) -> Option<Vec<PathBuf>> {
        Some(expand(pattern, active, base))
    }
}

/// What the blast-radius walk reads.
#[derive(Clone, Copy, Debug, Default)]
pub struct ShellFs;

impl Fs for ShellFs {
    fn lstat(&self, path: &Path) -> io::Result<Meta> {
        StdFs.lstat(path)
    }

    fn stat(&self, path: &Path) -> io::Result<Meta> {
        StdFs.stat(path)
    }

    fn read_dir(&self, path: &Path) -> io::Result<Names> {
        StdFs.read_dir(path)
    }

    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        StdFs.canonicalize(path)
    }

    fn glob(&self, pattern: &[u8], active: &[bool], base: &Path) -> Option<Vec<PathBuf>> {
        Some(expand(pattern, active, base))
    }
}
