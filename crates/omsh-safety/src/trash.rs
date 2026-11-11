//! The trash (§12.4): putting files out of the way, and bringing them back as they were.
//!
//! This module only moves files. It knows nothing about the journal; [`crate::journal`] decides
//! where things go and remembers what was moved. A move is a `rename`. Across file systems
//! (`EXDEV`) it is a copy, a check that the copy is the same, and only then the removal of the
//! original, so a failure on the way never loses the file: the original stays and the half-made
//! copy is removed.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt, lchown};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use nix::unistd::{AccessFlags, access};

/// What kind of thing a path is. Devices, pipes and sockets are not moved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    Link,
}

impl Kind {
    /// The name kept in the journal.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Dir => "dir",
            Self::Link => "link",
        }
    }

    /// The kind written by [`as_str`](Self::as_str).
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "file" => Some(Self::File),
            "dir" => Some(Self::Dir),
            "link" => Some(Self::Link),
            _ => None,
        }
    }
}

/// What is known about a path before it is moved: enough to put it back as it was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Meta {
    pub kind: Kind,
    /// The permission bits, with setuid, setgid and sticky.
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    /// Modification time.
    pub mtime_s: i64,
    pub mtime_ns: i64,
    /// Device and inode: tells a moved file from another one with the same name.
    pub dev: u64,
    pub ino: u64,
}

/// Reads the meta data of `path` without following a link.
///
/// # Errors
///
/// The error of `lstat`, or `Unsupported` for a device, a pipe or a socket.
pub fn meta(path: &Path) -> io::Result<Meta> {
    let data = fs::symlink_metadata(path)?;
    let kind = if data.file_type().is_symlink() {
        Kind::Link
    } else if data.is_dir() {
        Kind::Dir
    } else if data.is_file() {
        Kind::File
    } else {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "it is not a file, a folder or a link",
        ));
    };
    Ok(Meta {
        kind,
        mode: data.mode() & 0o7777,
        uid: data.uid(),
        gid: data.gid(),
        mtime_s: data.mtime(),
        mtime_ns: data.mtime_nsec(),
        dev: data.dev(),
        ino: data.ino(),
    })
}

/// True if something is at `path` (a broken link counts).
#[must_use]
pub fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

/// Why a move did not happen.
#[derive(Debug)]
pub enum MoveError {
    /// Nothing changed: the original is where it was, and no copy is left.
    Failed(io::Error),
    /// The copy is complete and checked, but the original could not be removed completely.
    /// The copy is kept.
    Partial(io::Error),
}

impl MoveError {
    /// The error underneath.
    #[must_use]
    pub const fn error(&self) -> &io::Error {
        match self {
            Self::Failed(error) | Self::Partial(error) => error,
        }
    }
}

/// How a move was done.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Moved {
    Renamed,
    Copied,
}

fn interrupted() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "interrupted")
}

/// Moves `src` to `dst` (which must not exist; its folder must). `cancelled` is looked at
/// between the files of a copy.
///
/// # Errors
///
/// See [`MoveError`].
pub fn move_to(src: &Path, dst: &Path, cancelled: &dyn Fn() -> bool) -> Result<Moved, MoveError> {
    match rename(src, dst) {
        Ok(()) => return Ok(Moved::Renamed),
        Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {}
        Err(error) => return Err(MoveError::Failed(error)),
    }
    move_by_copy(src, dst, cancelled)
}

/// The move across file systems: copy, check, remove the original. Public for the tests, which
/// have only one file system to work with.
///
/// # Errors
///
/// See [`MoveError`].
pub fn move_by_copy(
    src: &Path,
    dst: &Path,
    cancelled: &dyn Fn() -> bool,
) -> Result<Moved, MoveError> {
    // Before anything is copied: can the original be removed at all?
    if let Some(parent) = src.parent()
        && access(parent, AccessFlags::W_OK | AccessFlags::X_OK).is_err()
    {
        return Err(MoveError::Failed(io::Error::from(
            io::ErrorKind::PermissionDenied,
        )));
    }
    let copied = copy_tree(src, dst, cancelled).and_then(|()| verify_tree(src, dst));
    if let Err(error) = copied {
        let _ = remove_tree(dst);
        return Err(MoveError::Failed(error));
    }
    match remove_tree(src) {
        Ok(()) => Ok(Moved::Copied),
        Err(error) => Err(MoveError::Partial(error)),
    }
}

/// `rename`, which for a folder going to another parent needs write permission on the folder
/// itself (the kernel rewrites its `..`). A folder without it gets it for the moment.
fn rename(src: &Path, dst: &Path) -> io::Result<()> {
    match fs::rename(src, dst) {
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            let Ok(data) = fs::symlink_metadata(src) else {
                return Err(error);
            };
            if !data.is_dir() {
                return Err(error);
            }
            let mode = data.mode() & 0o7777;
            fs::set_permissions(src, fs::Permissions::from_mode(mode | 0o700))?;
            match fs::rename(src, dst) {
                Ok(()) => {
                    fs::set_permissions(dst, fs::Permissions::from_mode(mode))?;
                    Ok(())
                }
                Err(second) => {
                    let _ = fs::set_permissions(src, fs::Permissions::from_mode(mode));
                    Err(second)
                }
            }
        }
        other => other,
    }
}

fn set_mtime(path: &Path, seconds: i64, nanos: i64) {
    let time = if seconds >= 0 {
        UNIX_EPOCH + Duration::new(seconds.unsigned_abs(), u32::try_from(nanos).unwrap_or(0))
    } else {
        UNIX_EPOCH
    };
    if let Ok(file) = File::open(path) {
        let _ = file.set_modified(time);
    }
}

/// Copies a file, a link or a folder with everything in it, keeping modes and times.
fn copy_tree(src: &Path, dst: &Path, cancelled: &dyn Fn() -> bool) -> io::Result<()> {
    if cancelled() {
        return Err(interrupted());
    }
    let data = fs::symlink_metadata(src)?;
    let kind = data.file_type();
    if kind.is_symlink() {
        std::os::unix::fs::symlink(fs::read_link(src)?, dst)?;
        let _ = lchown(dst, Some(data.uid()), Some(data.gid()));
    } else if kind.is_dir() {
        fs::create_dir(dst)?;
        // Writable while it is filled; the real mode is set at the end.
        fs::set_permissions(dst, fs::Permissions::from_mode(0o700))?;
        let mut names: Vec<_> = fs::read_dir(src)?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<io::Result<_>>()?;
        names.sort();
        for name in names {
            copy_tree(&src.join(&name), &dst.join(&name), cancelled)?;
        }
        set_mtime(dst, data.mtime(), data.mtime_nsec());
        fs::set_permissions(dst, fs::Permissions::from_mode(data.mode() & 0o7777))?;
        let _ = lchown(dst, Some(data.uid()), Some(data.gid()));
    } else if kind.is_file() {
        let mut from = File::open(src)?;
        let mut to = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(dst)?;
        io::copy(&mut from, &mut to)?;
        to.set_modified(data.modified()?)?;
        drop(to);
        fs::set_permissions(dst, fs::Permissions::from_mode(data.mode() & 0o7777))?;
        let _ = lchown(dst, Some(data.uid()), Some(data.gid()));
    } else {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "{} is a {} and cannot be moved to the trash",
                src.display(),
                if kind.is_fifo() {
                    "pipe"
                } else if kind.is_socket() {
                    "socket"
                } else {
                    "device"
                }
            ),
        ));
    }
    Ok(())
}

fn same_contents(a: &Path, b: &Path) -> io::Result<bool> {
    let (mut a, mut b) = (File::open(a)?, File::open(b)?);
    let (mut left, mut right) = ([0_u8; 64 * 1024], [0_u8; 64 * 1024]);
    loop {
        let n = a.read(&mut left)?;
        let mut got = 0;
        while got < n {
            let m = b.read(&mut right[got..n])?;
            if m == 0 {
                return Ok(false);
            }
            got += m;
        }
        if n == 0 {
            return Ok(b.read(&mut right[..1])? == 0);
        }
        if left[..n] != right[..n] {
            return Ok(false);
        }
    }
}

/// Checks that `copy` has the same kinds, names, sizes, bytes and link targets as `original`.
fn verify_tree(original: &Path, copy: &Path) -> io::Result<()> {
    let wrong = |what: &str| {
        io::Error::other(format!(
            "the copy of {} does not match the original ({what})",
            original.display()
        ))
    };
    let (a, b) = (fs::symlink_metadata(original)?, fs::symlink_metadata(copy)?);
    if a.file_type() != b.file_type() {
        return Err(wrong("kind"));
    }
    if a.file_type().is_symlink() {
        if fs::read_link(original)? != fs::read_link(copy)? {
            return Err(wrong("link target"));
        }
    } else if a.is_dir() {
        let names = |path: &Path| -> io::Result<Vec<_>> {
            let mut names: Vec<_> = fs::read_dir(path)?
                .map(|entry| entry.map(|entry| entry.file_name()))
                .collect::<io::Result<_>>()?;
            names.sort();
            Ok(names)
        };
        let (left, right) = (names(original)?, names(copy)?);
        if left != right {
            return Err(wrong("names"));
        }
        for name in left {
            verify_tree(&original.join(&name), &copy.join(&name))?;
        }
    } else if a.len() != b.len() || !same_contents(original, copy)? {
        return Err(wrong("content"));
    }
    Ok(())
}

/// Removes a file, a link or a folder with everything in it. A folder without write permission
/// gets it first, as `rm -rf` would not do and the trash has to.
///
/// # Errors
///
/// The first error met.
pub fn remove_tree(path: &Path) -> io::Result<()> {
    let data = match fs::symlink_metadata(path) {
        Ok(data) => data,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if data.is_dir() {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        for entry in fs::read_dir(path)? {
            remove_tree(&entry?.path())?;
        }
        fs::remove_dir(path)
    } else {
        fs::remove_file(path)
    }
}

/// What could not be restored exactly.
#[derive(Debug, PartialEq, Eq)]
pub enum RestoreNote {
    /// Missing folders above the path were made.
    MadeFolders,
    /// The owner could not be set back (only root can give files away).
    OwnerNotRestored,
}

/// Why a restore did not happen.
#[derive(Debug)]
pub enum RestoreError {
    /// Something is at the original path now.
    Exists,
    Move(MoveError),
    Io(io::Error),
}

/// Moves `from` (in the trash) back to `to`, then sets mode, owner and time to what `meta`
/// says. Missing folders above `to` are made.
///
/// # Errors
///
/// See [`RestoreError`].
pub fn restore(
    from: &Path,
    to: &Path,
    meta: &Meta,
    cancelled: &dyn Fn() -> bool,
) -> Result<Vec<RestoreNote>, RestoreError> {
    if exists(to) {
        return Err(RestoreError::Exists);
    }
    let mut notes = Vec::new();
    if let Some(parent) = to.parent()
        && !parent.exists()
    {
        fs::create_dir_all(parent).map_err(RestoreError::Io)?;
        notes.push(RestoreNote::MadeFolders);
    }
    move_to(from, to, cancelled).map_err(RestoreError::Move)?;
    if meta.kind != Kind::Link {
        let _ = fs::set_permissions(to, fs::Permissions::from_mode(meta.mode));
        set_mtime(to, meta.mtime_s, meta.mtime_ns);
    }
    if let Ok(now) = fs::symlink_metadata(to)
        && (now.uid() != meta.uid || now.gid() != meta.gid)
        && lchown(to, Some(meta.uid), Some(meta.gid)).is_err()
    {
        notes.push(RestoreNote::OwnerNotRestored);
    }
    Ok(notes)
}

/// The folders of `root` made by actions older than `before` (unix seconds): folders are named
/// `<unix seconds>-<action id>`. Folders being emptied by another shell (`.purging-…`) are
/// returned separately.
#[must_use]
pub fn old_folders(root: &Path, before: i64) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let (mut old, mut leftovers) = (Vec::new(), Vec::new());
    let Ok(entries) = fs::read_dir(root) else {
        return (old, leftovers);
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with(".purging-") {
            leftovers.push(entry.path());
        } else if let Some((secs, _)) = name.split_once('-')
            && let Ok(secs) = secs.parse::<i64>()
            && secs < before
        {
            old.push(entry.path());
        }
    }
    old.sort();
    (old, leftovers)
}

/// Takes a folder for emptying by renaming it to `.purging-<name>`. Only one shell wins; the
/// others get `None`.
#[must_use]
pub fn claim_for_purge(folder: &Path) -> Option<PathBuf> {
    let name = folder.file_name()?.to_str()?;
    let target = folder.with_file_name(format!(".purging-{name}"));
    fs::rename(folder, &target).ok()?;
    Some(target)
}

/// Now, in unix seconds.
#[must_use]
pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests;
