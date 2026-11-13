//! The daily check for a new release (`[update] check = true`).
//!
//! Off by default: a check is network traffic, and the normal path makes none (Gate C). When it
//! is on, the shell asks at most once in 24 hours, on a helper thread, a couple of seconds
//! after the prompt is up, and only what the latest release is. If it is newer than this
//! program the person gets a toast. Nothing is downloaded and nothing is installed.
//!
//! A small stamp file in the state folder remembers when the last lookup was and what it found,
//! so a second shell started the same day neither uses the network nor shows the toast again.
//! The stamp is written *before* the lookup, so a server that never answers is asked once a day
//! and not at every start.
//!
//! The check is silent when it fails; `omsh update --check` shows the reason.

use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use omsh_editor::ToastHandle;

use super::version::Version;
use super::{DetachedNetwork, Fetch, UpdateEnv, latest};

/// How long a lookup counts. A clock set more than this far back counts as "due" too.
pub const DAY: u64 = 24 * 60 * 60;
/// Wait after start before asking, so the first prompt and the first commands come first.
const START_DELAY: Duration = Duration::from_secs(2);
/// The stamp file in the state folder.
const FILE: &str = "update-check";
/// The most the stamp may be.
const MAX_STAMP: u64 = 4096;

/// What the last lookup left behind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stamp {
    /// When the lookup was, in seconds since 1970.
    pub checked: u64,
    /// The latest release it found, if it found one.
    pub latest: Option<Version>,
}

/// Seconds since 1970.
#[must_use]
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// Reads the stamp in `dir`. `None` when there is none or it is not readable or not understood.
#[must_use]
pub fn read(dir: &Path) -> Option<Stamp> {
    let file = fs::File::open(dir.join(FILE)).ok()?;
    let mut text = String::new();
    file.take(MAX_STAMP).read_to_string(&mut text).ok()?;
    let mut checked = None;
    let mut latest = None;
    for line in text.lines() {
        match line.split_once('=') {
            Some(("checked", value)) => checked = value.trim().parse().ok(),
            Some(("latest", value)) => latest = Version::parse(value),
            _ => {}
        }
    }
    Some(Stamp {
        checked: checked?,
        latest,
    })
}

/// Writes the stamp in `dir`, to a temporary name and then renamed, in a folder only the user
/// can enter.
///
/// # Errors
///
/// The I/O error.
pub fn write(dir: &Path, stamp: &Stamp) -> io::Result<()> {
    DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    let tmp = dir.join(format!("{FILE}.tmp-{}", std::process::id()));
    let text = format!(
        "checked={}\nlatest={}\n",
        stamp.checked,
        stamp
            .latest
            .as_ref()
            .map_or_else(String::new, ToString::to_string)
    );
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, dir.join(FILE))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Whether a lookup is due at `now`: no stamp, a day passed, or the stamp is from the future
/// (the clock was set back).
#[must_use]
pub fn is_due(stamp: Option<&Stamp>, now: u64) -> bool {
    match stamp {
        None => true,
        Some(stamp) => now.saturating_sub(stamp.checked) >= DAY || stamp.checked > now + DAY,
    }
}

/// One round of the check: when it is due, writes the stamp, asks for the latest release,
/// writes what it found, and returns the version if it is newer than `current`.
pub fn check_once(
    env: &UpdateEnv,
    now: u64,
    fetch: &dyn Fetch,
    current: &Version,
) -> Option<Version> {
    let dir = env.state_dir.as_deref()?;
    let origin = env.origin.as_ref().ok()?;
    let before = read(dir);
    if !is_due(before.as_ref(), now) {
        return None;
    }
    // First, so that a lookup that fails or hangs is not tried again at every start.
    let _ = write(
        dir,
        &Stamp {
            checked: now,
            latest: before.and_then(|stamp| stamp.latest),
        },
    );
    let (_tag, newest) = latest(origin, fetch, &|| false).ok()?;
    let _ = write(
        dir,
        &Stamp {
            checked: now,
            latest: Some(newest.clone()),
        },
    );
    (newest > *current).then_some(newest)
}

/// The words of the toast.
#[must_use]
pub fn message(newest: &Version, current: &Version) -> String {
    format!("Oh My Shell {newest} is out (you have {current}): run omsh update")
}

/// The running daily check. Dropping it does nothing: the helper thread is not waited for and
/// ends with the process.
pub struct Daily {
    /// The message for a shell with no toast, waiting for the next prompt.
    stash: Arc<Mutex<Option<String>>>,
}

impl Daily {
    /// The message waiting for a shell that has no toast, once.
    #[must_use]
    pub fn take_message(&self) -> Option<String> {
        self.stash
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }
}

/// Starts the check if it is due. `toast` is where the news is shown; without one it waits in
/// [`Daily::take_message`]. Returns `None` (and starts nothing) when no lookup is due or
/// nothing can be written.
#[must_use]
pub fn start(env: &UpdateEnv, toast: Option<ToastHandle>) -> Option<Daily> {
    start_with(env, toast, START_DELAY, Arc::new(DetachedNetwork))
}

/// [`start`] with its delay and its network given, for the tests.
#[must_use]
pub fn start_with(
    env: &UpdateEnv,
    toast: Option<ToastHandle>,
    delay: Duration,
    fetch: Arc<dyn Fetch>,
) -> Option<Daily> {
    let dir = env.state_dir.as_deref()?;
    env.origin.as_ref().ok()?;
    if !is_due(read(dir).as_ref(), now()) {
        return None;
    }
    let stash = Arc::new(Mutex::new(None));
    let env = env.clone();
    let shared = Arc::clone(&stash);
    let spawned = thread::Builder::new()
        .name("update-check".to_owned())
        .spawn(move || {
            thread::sleep(delay);
            let current = Version::current();
            let Some(newest) = check_once(&env, now(), fetch.as_ref(), &current) else {
                return;
            };
            let text = message(&newest, &current);
            match &toast {
                Some(toast) => toast.post(&text),
                None => {
                    *shared.lock().unwrap_or_else(PoisonError::into_inner) = Some(text);
                }
            }
        });
    spawned.ok().map(|_| Daily { stash })
}
