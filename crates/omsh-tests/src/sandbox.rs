//! The only way a test runs `omsh update`, `omsh uninstall` or the daily check (S29b).
//!
//! Those commands change or delete files: the program itself, settings, history, keys. A test
//! that ran one with the real `HOME` could delete the owner's real data. So the commands are run
//! only through a [`Sandbox`], which
//!
//! - gives the program a **fresh folder** for `HOME` and for every `XDG_*` variable and
//!   `CARGO_HOME` (all inside the test's own temporary home), and nothing else of the
//!   environment;
//! - **checks first** that none of those paths is the real home directory of the user or inside
//!   it (read from the account database, not from `$HOME`, which a test may have changed; and
//!   `$HOME` of the test process too), after resolving symbolic links, and **panics** if one is,
//!   before anything is created or started;
//! - runs a **copy** (a hard link) of the shell under the name `oh-my-shell` in the sandbox, never
//!   the file in `target/`: `omsh update` replaces the file it runs from and `omsh uninstall`
//!   deletes it.
//!
//! If `TMPDIR` is inside the real home the check fails, and says so. Set `TMPDIR` to a folder
//! outside it.

use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use nix::unistd::{Uid, User};

use crate::session::Launch;
use crate::watchdog::Ctx;

/// The variables a sandbox sets, with their place under the sandbox home (empty: the home).
pub const FOLDERS: [(&str, &str); 6] = [
    ("HOME", ""),
    ("XDG_CONFIG_HOME", ".config"),
    ("XDG_DATA_HOME", ".local/share"),
    ("XDG_STATE_HOME", ".local/state"),
    ("XDG_CACHE_HOME", ".cache"),
    ("CARGO_HOME", ".cargo"),
];

/// The real home directories that no test may touch: the account database's entry for this
/// user and the `HOME` of the test process, resolved.
#[must_use]
pub fn protected_homes() -> Vec<PathBuf> {
    let mut homes = Vec::new();
    if let Ok(Some(user)) = User::from_uid(Uid::current()) {
        homes.push(user.dir);
    }
    if let Some(home) = std::env::var_os("HOME") {
        homes.push(PathBuf::from(home));
    }
    let mut resolved: Vec<PathBuf> = homes
        .into_iter()
        .filter(|home| home.is_absolute())
        .map(|home| resolve(&home))
        .collect();
    resolved.sort();
    resolved.dedup();
    resolved
}

/// `path` with symbolic links resolved as far as the path exists, and the rest (which does not
/// exist yet) appended without its `..` parts. So a link in a fresh folder that points into the
/// real home cannot hide a path.
#[must_use]
pub fn resolve(path: &Path) -> PathBuf {
    let mut existing = path.to_path_buf();
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if let Ok(real) = fs::canonicalize(&existing) {
            let mut out = real;
            for part in rest.iter().rev() {
                out.push(part);
            }
            return out;
        }
        match (
            existing.file_name().map(ToOwned::to_owned),
            existing.parent(),
        ) {
            (Some(name), Some(parent)) => {
                rest.push(name);
                existing = parent.to_path_buf();
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// What is wrong with `folders` (name and path), given the homes that are protected. Empty when
/// all is well.
#[must_use]
pub fn problems(folders: &[(String, PathBuf)], protected: &[PathBuf]) -> Vec<String> {
    let mut found = Vec::new();
    if protected.is_empty() {
        found.push("the real home directory could not be found, so nothing can be checked".into());
    }
    for (name, path) in folders {
        if !path.is_absolute() || path.components().any(|c| c == Component::ParentDir) {
            found.push(format!(
                "{name}={} is not an absolute path without `..`",
                path.display()
            ));
            continue;
        }
        let real = resolve(path);
        for home in protected {
            if real.starts_with(home) {
                found.push(format!(
                    "{name}={} is the real home directory {} or inside it",
                    path.display(),
                    home.display()
                ));
            }
        }
    }
    found
}

/// Panics, naming every folder that is the real home or inside it.
///
/// # Panics
///
/// When [`problems`] finds any.
pub fn assert_isolated(folders: &[(String, PathBuf)], protected: &[PathBuf]) {
    let found = problems(folders, protected);
    assert!(
        found.is_empty(),
        "refusing to run a test that could touch the real home directory:\n  {}\n\
         (a sandbox keeps HOME and every XDG_* folder in the test's own temporary folder; \
         if TMPDIR is inside your home, set it to a folder outside)",
        found.join("\n  ")
    );
}

/// The master copy of the program, shared by the sandboxes of one test process: it is copied
/// once and each sandbox gets a hard link to it (a 250 MB debug build is not copied for every
/// test). Nothing writes into the file in place: `omsh update` writes a new file and renames
/// it, `omsh uninstall` removes the name. The master is deleted when the last link is gone.
static MASTER: Mutex<Option<PathBuf>> = Mutex::new(None);

fn link_program(source: &Path, into: &Path) -> io::Result<()> {
    let mut master = MASTER.lock().unwrap_or_else(PoisonError::into_inner);
    let path = match master.as_ref() {
        Some(path) if path.exists() => path.clone(),
        _ => {
            let dir =
                std::env::temp_dir().join(format!("omsh-test-program-{}", std::process::id()));
            fs::create_dir_all(&dir)?;
            let path = dir.join("oh-my-shell");
            fs::copy(source, &path)?;
            *master = Some(path.clone());
            path
        }
    };
    fs::hard_link(path, into)
}

fn release_program() {
    let mut master = MASTER.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(path) = master.as_ref()
        && fs::metadata(path).map_or(true, |meta| meta.nlink() <= 1)
    {
        let _ = fs::remove_file(path);
        if let Some(dir) = path.parent() {
            let _ = fs::remove_dir(dir);
        }
        *master = None;
    }
}

/// A folder set for one test where the update and uninstall commands may run.
pub struct Sandbox<'a> {
    ctx: &'a Ctx,
    home: PathBuf,
    program: PathBuf,
    /// The homes that must not be touched, checked again at every launch.
    protected: Vec<PathBuf>,
}

impl<'a> Sandbox<'a> {
    /// A sandbox in the test's own home, with a copy of `shell` installed in `bin/` under the
    /// name `oh-my-shell`.
    ///
    /// # Panics
    ///
    /// When the folders are not safe (see [`assert_isolated`]).
    ///
    /// # Errors
    ///
    /// When the copy cannot be made.
    pub fn new(ctx: &'a Ctx, shell: &str) -> io::Result<Self> {
        Self::in_home(ctx, shell, ctx.home(), &protected_homes())
    }

    /// [`new`](Self::new) with another home and another list of protected homes. The check runs
    /// before anything is created, so a home that is not safe fails here and writes nothing.
    /// (Tests of the check itself pass a stand-in for the real home, so that they never need
    /// to point a sandbox at the real one.)
    ///
    /// # Panics
    ///
    /// When the folders are not safe.
    ///
    /// # Errors
    ///
    /// When the copy cannot be made.
    pub fn in_home(
        ctx: &'a Ctx,
        shell: &str,
        home: &Path,
        protected: &[PathBuf],
    ) -> io::Result<Self> {
        let sandbox = Self {
            ctx,
            home: home.to_path_buf(),
            program: home.join("bin/oh-my-shell"),
            protected: protected.to_vec(),
        };
        assert_isolated(&sandbox.folders(), protected);
        fs::create_dir_all(home.join("bin"))?;
        link_program(Path::new(shell), &sandbox.program)?;
        Ok(sandbox)
    }

    /// The folder variables and where they point.
    #[must_use]
    pub fn folders(&self) -> Vec<(String, PathBuf)> {
        FOLDERS
            .iter()
            .map(|(name, under)| ((*name).to_owned(), self.home.join(under)))
            .collect()
    }

    /// The home of the sandbox.
    #[must_use]
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// The installed copy of the shell.
    #[must_use]
    pub fn program(&self) -> &Path {
        &self.program
    }

    /// A launch of the installed copy as `oh-my-shell`, with the sandbox's folders and no keyring
    /// bus. The folders are checked again, so a launch cannot outlive a change to them.
    ///
    /// # Panics
    ///
    /// When the folders are not safe.
    #[must_use]
    pub fn launch(&self) -> Launch {
        self.launch_at(&self.program)
    }

    /// [`launch`](Self::launch) for another copy of the program that a test put in the
    /// sandbox (for example under `.cargo/bin`).
    ///
    /// # Panics
    ///
    /// When the folders are not safe, or `program` is not inside the sandbox home.
    #[must_use]
    pub fn launch_at(&self, program: &Path) -> Launch {
        assert_isolated(&self.folders(), &self.protected);
        assert!(
            program.starts_with(&self.home),
            "{} is not inside the sandbox home {}",
            program.display(),
            self.home.display()
        );
        let mut launch = self.ctx.launch_program(program).env(
            "DBUS_SESSION_BUS_ADDRESS",
            "unix:path=/nonexistent/omsh-test-bus",
        );
        for (name, path) in self.folders() {
            launch = launch.env(&name, &path.to_string_lossy());
        }
        launch.cwd(&self.home)
    }

    /// The installed copy as `omsh` with no terminal: a plain process whose standard input is
    /// empty and whose standard output and error are pipes, in the sandbox's environment only.
    /// For commands that must refuse when there is no person to answer.
    ///
    /// # Errors
    ///
    /// When the link cannot be made.
    ///
    /// # Panics
    ///
    /// When the folders are not safe.
    pub fn command(&self) -> io::Result<std::process::Command> {
        use std::process::{Command, Stdio};
        assert_isolated(&self.folders(), &self.protected);
        let link = self.home.join("bin/omsh");
        let _ = fs::remove_file(&link);
        std::os::unix::fs::symlink(&self.program, &link)?;
        let mut command = Command::new(&link);
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "C")
            .env(
                "DBUS_SESSION_BUS_ADDRESS",
                "unix:path=/nonexistent/omsh-test-bus",
            )
            .current_dir(&self.home)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (name, path) in self.folders() {
            command.env(name, path);
        }
        Ok(command)
    }

    /// A launch of the installed copy as `omsh` (a link next to it), as the installer makes it.
    ///
    /// # Errors
    ///
    /// When the link cannot be made.
    ///
    /// # Panics
    ///
    /// When the folders are not safe.
    pub fn omsh(&self) -> io::Result<Launch> {
        self.launch().named(&self.home.join("bin"), "omsh")
    }
}

impl Drop for Sandbox<'_> {
    fn drop(&mut self) {
        // Our link first, so that the master copy can go when the last sandbox ends.
        let _ = fs::remove_file(&self.program);
        release_program();
    }
}
