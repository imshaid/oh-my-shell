//! What the analyzer may know about the machine, and nothing else.

use std::path::{Path, PathBuf};

/// Questions about the file system that a rule may ask.
pub trait Probe {
    /// True if `path` is a regular file with something in it.
    fn is_nonempty_file(&self, path: &Path) -> bool;

    /// The names the pattern matches, looked for from `base` (`active[i]` says whether byte
    /// `i` of `pattern` was written unquoted). `None` if this probe cannot expand patterns:
    /// the rules then judge the pattern without a count.
    fn glob(&self, _pattern: &[u8], _active: &[bool], _base: &Path) -> Option<Vec<PathBuf>> {
        None
    }
}

/// The real file system. Only `stat`: nothing is opened, so a named pipe cannot block it.
#[derive(Clone, Copy, Debug, Default)]
pub struct FsProbe;

impl Probe for FsProbe {
    fn is_nonempty_file(&self, path: &Path) -> bool {
        std::fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.len() > 0)
    }
}

/// The facts about the shell that the analysis starts from.
#[derive(Clone, Copy)]
pub struct Context<'p> {
    /// The working directory. Without it relative paths are not judged.
    pub cwd: Option<&'p Path>,
    /// What `~` and `$HOME` mean.
    pub home: Option<&'p Path>,
    /// Answers questions about files.
    pub probe: &'p dyn Probe,
}

impl Context<'_> {
    pub(crate) fn cwd_text(&self) -> Option<String> {
        self.cwd.and_then(Path::to_str).map(str::to_owned)
    }

    pub(crate) fn home_text(&self) -> Option<String> {
        self.home.and_then(Path::to_str).map(str::to_owned)
    }
}

impl std::fmt::Debug for Context<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Context")
            .field("cwd", &self.cwd)
            .field("home", &self.home)
            .finish_non_exhaustive()
    }
}

/// The paths a [`Context`] holds, owned, so a caller can build one without keeping the
/// pieces alive by hand.
#[derive(Clone, Debug, Default)]
pub struct Paths {
    /// See [`Context::cwd`].
    pub cwd: Option<PathBuf>,
    /// See [`Context::home`].
    pub home: Option<PathBuf>,
}

impl Paths {
    /// A context over the real file system.
    #[must_use]
    pub fn context<'p>(&'p self, probe: &'p dyn Probe) -> Context<'p> {
        Context {
            cwd: self.cwd.as_deref(),
            home: self.home.as_deref(),
            probe,
        }
    }
}
