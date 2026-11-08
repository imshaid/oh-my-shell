//! The file descriptors a command sees, as the shell tracks them.
//!
//! A redirection such as `2>&1` or `3>file` changes which file a command's descriptor 2 or 3
//! refers to. The shell cannot change its own descriptors for that, because other commands
//! (running on other threads in a pipeline) need the originals. So it keeps a table, [`Io`], from
//! a descriptor number to an open file, and gives each command its own copy of the table.
//!
//! - A builtin writes to the files in its table directly.
//! - A program started with `fork` and `execve` gets the table applied by the child, with
//!   `dup2`, just before `execve`. The child learns what to do from an [`FdPlan`] that was
//!   built before the `fork` (§4.3 forbids allocating in the child).
//!
//! Every file in a table is marked close-on-exec, so a program receives only what its table
//! says, and nothing else.

use std::collections::BTreeMap;
use std::io::{stderr, stdin, stdout};
use std::os::fd::{AsFd, AsRawFd, OwnedFd, RawFd};
use std::sync::Arc;

use nix::errno::Errno;
use nix::fcntl::{OFlag, open};
use nix::sys::stat::Mode;

/// An open file in a table. Several tables can hold the same file. It closes when the last
/// one goes away. That is how the end of a pipe is closed: once no stage holds the write end,
/// the reader sees the end of its input.
pub type Slot = Arc<OwnedFd>;

/// The table of open files for one command.
#[derive(Clone, Debug, Default)]
pub struct Io {
    slots: BTreeMap<u32, Slot>,
}

impl Io {
    /// The shell's own standard input, output and error.
    ///
    /// These are copies (`dup`) of descriptors 0, 1 and 2, so that the table owns everything
    /// in it. A copy shares the open file with the original, including its read position.
    #[must_use]
    pub fn process() -> Self {
        let mut io = Self::default();
        let copies = [
            stdin().as_fd().try_clone_to_owned(),
            stdout().as_fd().try_clone_to_owned(),
            stderr().as_fd().try_clone_to_owned(),
        ];
        for (fd, copy) in (0_u32..).zip(copies) {
            // A closed descriptor stays closed.
            if let Ok(copy) = copy {
                io.set(fd, Arc::new(copy));
            }
        }
        io
    }

    /// The file behind `fd`, if it is open.
    #[must_use]
    pub fn get(&self, fd: u32) -> Option<&Slot> {
        self.slots.get(&fd)
    }

    /// Makes `fd` refer to `slot`. The file it referred to before is closed if nothing else
    /// holds it.
    pub fn set(&mut self, fd: u32, slot: Slot) {
        self.slots.insert(fd, slot);
    }

    /// Makes descriptor 0 read from `/dev/null`, if that file can be opened.
    pub fn null_stdin(&mut self) {
        if let Ok(null) = open(
            "/dev/null",
            OFlag::O_RDONLY | OFlag::O_CLOEXEC,
            Mode::empty(),
        ) {
            self.set(0, Arc::new(null));
        }
    }

    /// Closes `fd` for this command.
    pub fn close(&mut self, fd: u32) {
        self.slots.remove(&fd);
    }

    /// What `self` has changed compared with `base`: each descriptor that now refers to another
    /// file (or to none, `None`). `exec 3>file` with no command is recorded this way, so the
    /// change can be made on the table of the code that follows.
    #[must_use]
    pub fn diff(&self, base: &Self) -> Vec<(u32, Option<Slot>)> {
        let mut out = Vec::new();
        for (fd, slot) in &self.slots {
            if base.slots.get(fd).is_none_or(|old| !Arc::ptr_eq(old, slot)) {
                out.push((*fd, Some(Arc::clone(slot))));
            }
        }
        for fd in base.slots.keys() {
            if !self.slots.contains_key(fd) {
                out.push((*fd, None));
            }
        }
        out
    }

    /// Makes the changes that [`diff`](Self::diff) found.
    pub fn apply(&mut self, changes: &[(u32, Option<Slot>)]) {
        for (fd, slot) in changes {
            match slot {
                Some(slot) => self.set(*fd, Arc::clone(slot)),
                None => self.close(*fd),
            }
        }
    }

    /// Writes all of `bytes` to `fd`.
    ///
    /// # Errors
    ///
    /// Returns the error from `write`: `EBADF` if `fd` is closed or not open for writing,
    /// `EPIPE` if the reader of a pipe has gone.
    pub fn write_all(&self, fd: u32, mut bytes: &[u8]) -> Result<(), Errno> {
        let slot = self.slots.get(&fd).ok_or(Errno::EBADF)?;
        while !bytes.is_empty() {
            match nix::unistd::write(slot.as_fd(), bytes) {
                Ok(n) => bytes = &bytes[n..],
                Err(Errno::EINTR) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    /// Writes one line to descriptor 2. A failure is ignored: there is nowhere left to say so.
    pub fn say(&self, message: impl AsRef<[u8]>) {
        let mut line = message.as_ref().to_vec();
        line.push(b'\n');
        let _ = self.write_all(2, &line);
    }

    /// Builds what a child process needs to put this table in place. See [`FdPlan`].
    #[must_use]
    pub fn plan(&self) -> FdPlan {
        let mut pairs = Vec::new();
        let mut floor: RawFd = 3;
        for (target, slot) in &self.slots {
            let Ok(target) = RawFd::try_from(*target) else {
                continue;
            };
            let source = slot.as_raw_fd();
            pairs.push((target, source));
            floor = floor
                .max(target.saturating_add(1))
                .max(source.saturating_add(1));
        }
        let closes = (0..3_u32)
            .filter(|fd| !self.slots.contains_key(fd))
            .filter_map(|fd| RawFd::try_from(fd).ok())
            .collect();
        let scratch = vec![-1; pairs.len()];
        FdPlan {
            pairs,
            closes,
            floor,
            scratch,
        }
    }
}

/// The work a child does with file descriptors before `execve`.
///
/// Everything is prepared before `fork`, so the child allocates nothing. The child works in
/// two passes. First it copies every source to a number above all targets and all sources.
/// Then it moves each copy to its target. Without the first pass, `3>&1 1>&2 2>&3` (swap 1
/// and 2) would overwrite a source before it is read. `spawn` runs the plan.
#[derive(Debug)]
pub struct FdPlan {
    /// `(target, source)`: after the plan, descriptor `target` refers to what `source` did.
    pub(crate) pairs: Vec<(RawFd, RawFd)>,
    /// Descriptors to close.
    pub(crate) closes: Vec<RawFd>,
    /// The copies are made at this number or above.
    pub(crate) floor: RawFd,
    /// Room for the copies of the first pass.
    pub(crate) scratch: Vec<RawFd>,
}

impl FdPlan {
    /// A plan that changes nothing.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            pairs: Vec::new(),
            closes: Vec::new(),
            floor: 3,
            scratch: Vec::new(),
        }
    }
}

/// Test helpers: a table whose output goes to memory.
#[cfg(test)]
pub(crate) mod capture {
    use std::os::fd::{AsFd, OwnedFd};
    use std::sync::Arc;

    use nix::fcntl::{OFlag, open};
    use nix::sys::memfd::{MFdFlags, memfd_create};
    use nix::sys::stat::Mode;
    use nix::unistd::{Whence, lseek, read};

    use super::Io;

    /// A table with standard input from `/dev/null` and standard output and error in memory.
    pub(crate) struct Captured {
        pub(crate) io: Io,
        out: OwnedFd,
        err: OwnedFd,
    }

    impl Captured {
        pub(crate) fn new() -> Self {
            let mut io = Io::default();
            let null = open(
                "/dev/null",
                OFlag::O_RDONLY | OFlag::O_CLOEXEC,
                Mode::empty(),
            );
            io.set(0, Arc::new(null.unwrap()));
            let out = memfd_create(c"omsh-test-out", MFdFlags::MFD_CLOEXEC).unwrap();
            let err = memfd_create(c"omsh-test-err", MFdFlags::MFD_CLOEXEC).unwrap();
            io.set(1, Arc::new(out.try_clone().unwrap()));
            io.set(2, Arc::new(err.try_clone().unwrap()));
            Self { io, out, err }
        }

        pub(crate) fn stdout(&self) -> String {
            text(&self.out)
        }

        pub(crate) fn stderr(&self) -> String {
            text(&self.err)
        }
    }

    fn text(fd: &OwnedFd) -> String {
        // The position is shared with every writer: read from the start, then go back to the
        // end so that later writes append.
        lseek(fd.as_fd(), 0, Whence::SeekSet).unwrap();
        let mut bytes = Vec::new();
        let mut chunk = [0_u8; 4096];
        loop {
            let n = read(fd.as_fd(), &mut chunk).unwrap();
            if n == 0 {
                break;
            }
            bytes.extend_from_slice(&chunk[..n]);
        }
        lseek(fd.as_fd(), 0, Whence::SeekEnd).unwrap();
        String::from_utf8_lossy(&bytes).into_owned()
    }
}
