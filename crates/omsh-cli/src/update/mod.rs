//! `omsh update` and the daily check for a new release (S29b, blueprint §18, D12).
//!
//! | Piece | What it does |
//! |---|---|
//! | [`run`] | `omsh update [--check]`: look up the latest release, download the archive for this CPU, check it against the release's `sha256.sum`, run the new program once, and rename it over the old one |
//! | [`daily`] | `[update] check = true`: at most once a day, on a helper thread, ask only what the latest release is and show a toast |
//! | [`place`] | refuses to touch a program that a package manager, `cargo` or a build folder owns |
//! | [`archive`] | unpacks one file from the `.tar.gz` and computes SHA-256 |
//! | [`version`] | reads and orders version numbers |
//!
//! Updates go through verified release files only, never `git pull` (D12). Nothing here runs
//! unless a person types `omsh update` or turns the check on: the normal path makes no network
//! call (Gate C). The network itself is in `omsh_ai::download`, the one crate allowed to open a
//! socket.

pub mod archive;
pub mod daily;
pub mod place;
pub mod version;

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use omsh_ai::download::{self, ACCEPT_ANY, Origin};
use omsh_ai::{ErrorKind, ProviderError};
use omsh_exec::Terminal;

use self::place::Place;
use self::version::Version;

/// The repository releases come from.
pub const REPO: &str = "imshaid/oh-my-shell";

/// The most a `sha256.sum` may be.
const MAX_SUMS: usize = 64 * 1024;
/// The most a release archive may be (the real one is about 5 MB).
const MAX_ARCHIVE: usize = 100 * 1024 * 1024;
/// How long one lookup may take.
const LOOKUP_TIME: Duration = Duration::from_secs(15);
/// How long the archive may take.
const DOWNLOAD_TIME: Duration = Duration::from_secs(300);
/// How long the new program may take to print its version.
const TRY_TIME: Duration = Duration::from_secs(5);

/// Exit status of a command that was used wrongly.
const USAGE_STATUS: u8 = 2;

/// Everything `omsh update` and the daily check need from outside.
#[derive(Clone, Debug)]
pub struct UpdateEnv {
    /// Where releases come from: GitHub, or the loopback server of the test hook. An error
    /// is the sentence saying the hook's value was refused.
    pub origin: Result<Origin, String>,
    /// The state folder, where the daily stamp is kept. `None` under `--no-config` (nothing
    /// is written then).
    pub state_dir: Option<PathBuf>,
    pub home: Option<PathBuf>,
    pub cargo_home: Option<PathBuf>,
}

/// Where downloads come from. The real one is the network; tests give a table.
pub trait Fetch: Send + Sync {
    /// One GET: the body, or why not.
    ///
    /// # Errors
    ///
    /// [`ProviderError`] on a network failure, a status other than 2xx, a body over
    /// `max_bytes`, or a Ctrl+C.
    fn get(
        &self,
        url: &str,
        accept: &str,
        max_bytes: usize,
        timeout: Duration,
        interrupted: &dyn Fn() -> bool,
    ) -> Result<Vec<u8>, ProviderError>;

    /// One GET that is not followed: the address a redirect points to. This is how the latest
    /// release is found (`releases/latest` redirects to `releases/tag/<tag>`).
    ///
    /// # Errors
    ///
    /// [`ProviderError`] on a network failure, an answer that is not a redirect, or a Ctrl+C.
    fn location(
        &self,
        url: &str,
        timeout: Duration,
        interrupted: &dyn Fn() -> bool,
    ) -> Result<String, ProviderError>;
}

/// The network, on the shared runtime (a command a person waits for).
pub struct Network;

impl Fetch for Network {
    fn get(
        &self,
        url: &str,
        accept: &str,
        max_bytes: usize,
        timeout: Duration,
        interrupted: &dyn Fn() -> bool,
    ) -> Result<Vec<u8>, ProviderError> {
        download::get(url, accept, max_bytes, timeout, interrupted)
    }

    fn location(
        &self,
        url: &str,
        timeout: Duration,
        interrupted: &dyn Fn() -> bool,
    ) -> Result<String, ProviderError> {
        download::location(url, timeout, interrupted)
    }
}

/// The network, on a runtime of its own (the background check; it cannot be interrupted).
pub struct DetachedNetwork;

impl Fetch for DetachedNetwork {
    fn get(
        &self,
        url: &str,
        accept: &str,
        max_bytes: usize,
        timeout: Duration,
        _interrupted: &dyn Fn() -> bool,
    ) -> Result<Vec<u8>, ProviderError> {
        download::get_detached(url, accept, max_bytes, timeout)
    }

    fn location(
        &self,
        url: &str,
        timeout: Duration,
        _interrupted: &dyn Fn() -> bool,
    ) -> Result<String, ProviderError> {
        download::location_detached(url, timeout)
    }
}

/// The words for a failed download, never an address and never a response body.
#[must_use]
pub fn describe(error: &ProviderError) -> String {
    let detail = &error.detail;
    match error.kind {
        ErrorKind::Offline => {
            format!("cannot reach the release server ({detail}); check the network")
        }
        ErrorKind::Timeout => "the release server did not answer in time".to_owned(),
        ErrorKind::NotFound => format!("the release was not found ({detail})"),
        ErrorKind::RateLimited => {
            "GitHub says there are too many requests; try again later".to_owned()
        }
        ErrorKind::Cancelled => "cancelled".to_owned(),
        _ => format!("the download failed ({detail})"),
    }
}

/// The latest release, as a tag and a version.
///
/// # Errors
///
/// A sentence saying why it is not known.
pub fn latest(
    origin: &Origin,
    fetch: &dyn Fetch,
    interrupted: &dyn Fn() -> bool,
) -> Result<(String, Version), String> {
    // Not the GitHub API (60 requests an hour for an address): the web address redirects to
    // the newest release, and the tag is in the address it redirects to.
    let url = format!("{}/{REPO}/releases/latest", origin.web);
    let location = fetch
        .location(&url, LOOKUP_TIME, interrupted)
        .map_err(|error| describe(&error))?;
    let tag = download::tag_from_location(&location)
        .ok_or("the latest release could not be read (is there one yet?)")?;
    let version = Version::parse(&tag).ok_or_else(|| {
        format!("the latest release has a tag that is not a version number ({tag})")
    })?;
    Ok((tag, version))
}

/// The release archive's name for this CPU, or `None` where there is no release.
#[must_use]
pub fn archive_name(arch: &str) -> Option<String> {
    let target = match arch {
        "x86_64" => "x86_64-unknown-linux-gnu",
        "aarch64" => "aarch64-unknown-linux-gnu",
        _ => return None,
    };
    Some(format!("oh-my-shell-{target}.tar.gz"))
}

/// What to do about an old program at `place`, in words.
fn refusal(place: &Place) -> Option<&'static str> {
    match place {
        Place::Own => None,
        Place::Package => Some(
            "this copy was installed by a package (under /usr). Download the new .deb from \
             https://github.com/imshaid/oh-my-shell/releases and install it with: \
             sudo apt install ./oh-my-shell_<version>_<arch>.deb",
        ),
        Place::Cargo => Some(
            "this copy was installed by cargo. Update it with: cargo install --locked --git \
             https://github.com/imshaid/oh-my-shell omsh-cli",
        ),
        Place::Build => Some(
            "this is a build from source (a target/ folder), not an installed copy. Build it \
             again, or install a release with install.sh",
        ),
    }
}

const USAGE: &str = "\
usage: omsh update [--check]
  (no option)  download the latest release, check it, and replace this program
  --check      only say whether a newer release exists
";

/// `omsh update`.
pub fn run(
    args: &[&str],
    terminal: &mut dyn Terminal,
    env: &UpdateEnv,
    exe: Result<PathBuf, String>,
    fetch: &dyn Fetch,
) -> u8 {
    let check_only = match args {
        [] => false,
        ["--check"] => true,
        ["-h" | "--help"] => {
            terminal.out(USAGE);
            return 0;
        }
        _ => {
            terminal.err(&format!("omsh: update: bad arguments\n{USAGE}"));
            return USAGE_STATUS;
        }
    };
    let origin = match &env.origin {
        Ok(origin) => origin,
        Err(why) => {
            terminal.err(&format!("omsh: update: {why}\n"));
            return 1;
        }
    };
    // The location is looked at before anything is downloaded; `--check` only reads.
    let mut target = None;
    if !check_only {
        let path = match exe {
            Ok(path) => path,
            Err(why) => {
                terminal.err(&format!("omsh: update: {why}\n"));
                return 1;
            }
        };
        let place = place::classify(&path, env.home.as_deref(), env.cargo_home.as_deref());
        if let Some(text) = refusal(&place) {
            terminal.err(&format!("omsh: update: {text}\n"));
            return 1;
        }
        target = Some(path);
    }

    terminal.err("Looking for the latest release...\n");
    let (tag, newest) = match latest(origin, fetch, &|| terminal.interrupted()) {
        Ok(found) => found,
        Err(why) => return failed(terminal, &why),
    };
    let current = Version::current();
    note_checked(env, &newest);
    if newest <= current {
        terminal.out(&format!("Oh My Shell {current} is the newest release.\n"));
        return 0;
    }
    if check_only {
        terminal.out(&format!(
            "Oh My Shell {newest} is available (you have {current}). Run: omsh update\n"
        ));
        return 0;
    }
    let Some(target) = target else {
        return 1;
    };
    let Some(name) = archive_name(std::env::consts::ARCH) else {
        terminal.err(&format!(
            "omsh: update: there is no release for the CPU type {} (x86_64 and aarch64 only)\n",
            std::env::consts::ARCH
        ));
        return 1;
    };

    let base = format!("{}/{REPO}/releases/download/{tag}", origin.web);
    terminal.err(&format!("Downloading {name} ({newest})...\n"));
    let archive_bytes = match fetch.get(
        &format!("{base}/{name}"),
        ACCEPT_ANY,
        MAX_ARCHIVE,
        DOWNLOAD_TIME,
        &|| terminal.interrupted(),
    ) {
        Ok(bytes) => bytes,
        Err(error) => return failed(terminal, &describe(&error)),
    };
    let sums = match fetch.get(
        &format!("{base}/sha256.sum"),
        ACCEPT_ANY,
        MAX_SUMS,
        LOOKUP_TIME,
        &|| terminal.interrupted(),
    ) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(error) => return failed(terminal, &describe(&error)),
    };
    let Some(wanted) = archive::wanted_sum(&sums, &name) else {
        return failed(
            terminal,
            &format!("sha256.sum does not list {name}; nothing was changed"),
        );
    };
    let got = archive::sha256_hex(&archive_bytes);
    if got != wanted {
        return failed(
            terminal,
            &format!(
                "checksum mismatch for {name} (wanted {wanted}, got {got}); nothing was changed"
            ),
        );
    }
    terminal.err("Checksum ok.\n");

    let inside = format!("{}/oh-my-shell", name.trim_end_matches(".tar.gz"));
    let program = match archive::extract(&archive_bytes, &inside) {
        Ok(bytes) => bytes,
        Err(why) => return failed(terminal, &format!("{why}; nothing was changed")),
    };
    match install(&target, &program, &newest) {
        Ok(()) => {
            terminal.out(&format!(
                "Updated Oh My Shell {current} -> {newest} at {}.\n\
                 Shells that are running keep the old version until they are restarted.\n",
                target.display()
            ));
            0
        }
        Err(why) => failed(terminal, &why),
    }
}

/// Prints `why` as an error of `omsh update` and returns the failure status.
fn failed(terminal: &mut dyn Terminal, why: &str) -> u8 {
    if why == "cancelled" {
        terminal.err("omsh: update: cancelled\n");
        return 130;
    }
    terminal.err(&format!("omsh: update: {why}\n"));
    1
}

/// Records that the latest release is `newest`, so the daily check knows. Silent: the stamp
/// is a convenience.
fn note_checked(env: &UpdateEnv, newest: &Version) {
    if let Some(dir) = &env.state_dir {
        let _ = daily::write(
            dir,
            &daily::Stamp {
                checked: daily::now(),
                latest: Some(newest.clone()),
            },
        );
    }
}

/// Removes a file when dropped, unless it was kept.
struct Remove<'a> {
    path: &'a Path,
    keep: bool,
}

impl Drop for Remove<'_> {
    fn drop(&mut self) {
        if !self.keep {
            let _ = fs::remove_file(self.path);
        }
    }
}

/// Puts `program` in place of the file at `target`: written next to it, tried once with
/// `--version`, then renamed over it. A running copy keeps its old file. The old file is not
/// touched until the new one has been proven to run.
///
/// # Errors
///
/// A sentence; the old file is then as it was and the temporary file is gone.
pub fn install(target: &Path, program: &[u8], expected: &Version) -> Result<(), String> {
    let dir = target
        .parent()
        .ok_or_else(|| format!("{} has no folder", target.display()))?;
    let tmp = dir.join(format!(".oh-my-shell.update-{}", std::process::id()));
    let mode = fs::metadata(target)
        .map(|meta| meta.permissions().mode() & 0o7777)
        .unwrap_or(0o755)
        | 0o100;
    let mut guard = Remove {
        path: &tmp,
        keep: false,
    };
    write_new(&tmp, program, mode).map_err(|error| {
        if error.kind() == io::ErrorKind::PermissionDenied {
            format!(
                "cannot write to {}; run it with the rights to write there, for example: \
                 sudo {} update",
                dir.display(),
                target.display()
            )
        } else {
            format!("cannot write {}: {error}", tmp.display())
        }
    })?;
    let shown = try_version(&tmp)?;
    let want = format!("oh-my-shell {expected}");
    if shown != want {
        return Err(format!(
            "the new program says \"{shown}\", not \"{want}\"; nothing was changed"
        ));
    }
    fs::rename(&tmp, target)
        .map_err(|error| format!("cannot replace {}: {error}", target.display()))?;
    guard.keep = true;
    // The rename is on the disk when the folder is.
    if let Ok(folder) = File::open(dir) {
        let _ = folder.sync_all();
    }
    Ok(())
}

fn write_new(path: &Path, data: &[u8], mode: u32) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o700)
        .open(path)?;
    file.write_all(data)?;
    file.sync_all()?;
    drop(file);
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

/// Runs `program --version` and returns its trimmed output, or says why it did not work.
fn try_version(program: &Path) -> Result<String, String> {
    // A file that was just written may be "busy" for a moment while another thread forks
    // (the writer's descriptor is still open in that child until it execs).
    let mut child = None;
    for _ in 0..200 {
        match Command::new(program)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(started) => {
                child = Some(started);
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::ExecutableFileBusy => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => {
                return Err(format!(
                    "the new program does not run on this system ({error}); nothing was changed"
                ));
            }
        }
    }
    let Some(mut child) = child else {
        return Err(
            "the new program stayed busy and could not be tried; nothing was changed".into(),
        );
    };
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < TRY_TIME => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("the new program did not answer --version; nothing was changed".into());
            }
            Err(error) => return Err(format!("cannot wait for the new program ({error})")),
        }
    };
    let mut out = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        let _ = (&mut stdout).take(4096).read_to_string(&mut out);
    }
    if !status.success() {
        return Err("the new program does not run on this system; nothing was changed".into());
    }
    Ok(out.trim().to_owned())
}

#[cfg(test)]
mod tests;
