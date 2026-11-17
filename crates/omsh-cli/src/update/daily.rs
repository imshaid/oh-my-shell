//! The daily check for a new release (`[update] check = true`).
//!
//! Off by default: a check is network traffic, and the normal path makes none (Gate C). When it
//! is on, the shell asks at most once in 24 hours, on a helper thread, a couple of seconds
//! after the prompt is up, and only what the latest release is. If it is newer than this
//! program, the next session says so in one line where the hint bar is (`notice.rs`). Nothing is
//! downloaded and nothing is installed.
//!
//! A small stamp file in the state folder remembers when the last lookup was, what it found, and
//! when the news was last shown, so a second shell started the same day neither uses the network
//! nor shows the line again.
//! The stamp is written *before* the lookup, so a server that never answers is asked once a day
//! and not at every start.
//!
//! The check is silent when it fails; `omsh update --check` shows the reason.

use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
    /// When the notice about `latest` was last shown, in seconds since 1970.
    pub noticed: Option<u64>,
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
    let mut noticed = None;
    for line in text.lines() {
        match line.split_once('=') {
            Some(("checked", value)) => checked = value.trim().parse().ok(),
            Some(("latest", value)) => latest = Version::parse(value),
            Some(("noticed", value)) => noticed = value.trim().parse().ok(),
            _ => {}
        }
    }
    Some(Stamp {
        checked: checked?,
        latest,
        noticed,
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
        "checked={}\nlatest={}\nnoticed={}\n",
        stamp.checked,
        stamp
            .latest
            .as_ref()
            .map_or_else(String::new, ToString::to_string),
        stamp.noticed.map_or_else(String::new, |at| at.to_string()),
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
    let before_latest = before.as_ref().and_then(|stamp| stamp.latest.clone());
    let noticed = before.as_ref().and_then(|stamp| stamp.noticed);
    // First, so that a lookup that fails or hangs is not tried again at every start.
    let _ = write(
        dir,
        &Stamp {
            checked: now,
            latest: before_latest.clone(),
            noticed,
        },
    );
    let (_tag, newest) = latest(origin, fetch, &|| false).ok()?;
    let _ = write(
        dir,
        &Stamp {
            checked: now,
            latest: Some(newest.clone()),
            // The same release that was already announced is not announced again; a newer one is.
            noticed: noticed.filter(|_| before_latest.as_ref() == Some(&newest)),
        },
    );
    (newest > *current).then_some(newest)
}

/// Why the daily check does not run although `[update] check` is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Skip {
    /// `OMSH_NO_UPDATE_CHECK` is set to something other than empty or `0`.
    Variable,
    /// A continuous-integration run.
    Ci,
    /// Standard error is not a terminal: nobody would see the news.
    NotTerminal,
}

impl Skip {
    /// The reason in words, for `omsh status`.
    #[must_use]
    pub fn words(self) -> &'static str {
        match self {
            Skip::Variable => "OMSH_NO_UPDATE_CHECK is set",
            Skip::Ci => "this is a CI run",
            Skip::NotTerminal => "standard error is not a terminal",
        }
    }
}

/// The name of the variable that turns the check (and its notice) off.
pub const OPT_OUT: &str = "OMSH_NO_UPDATE_CHECK";

/// Whether the check is skipped for this start, and why. `variable` is the value of
/// [`OPT_OUT`].
#[must_use]
pub fn skip_reason(variable: Option<&[u8]>, ci: bool, terminal: bool) -> Option<Skip> {
    if variable.is_some_and(|value| !value.is_empty() && value != b"0") {
        Some(Skip::Variable)
    } else if ci {
        Some(Skip::Ci)
    } else if !terminal {
        Some(Skip::NotTerminal)
    } else {
        None
    }
}

/// Starts the check if it is due. Returns false (and starts nothing) when no lookup is due or
/// nothing can be written. The news is not shown here: the next session reads it from the stamp.
#[must_use]
pub fn start(env: &UpdateEnv) -> bool {
    start_with(env, START_DELAY, Arc::new(DetachedNetwork))
}

/// [`start`] with its delay and its network given, for the tests.
#[must_use]
pub fn start_with(env: &UpdateEnv, delay: Duration, fetch: Arc<dyn Fetch>) -> bool {
    let Some(dir) = env.state_dir.as_deref() else {
        return false;
    };
    if env.origin.is_err() || !is_due(read(dir).as_ref(), now()) {
        return false;
    }
    let env = env.clone();
    thread::Builder::new()
        .name("update-check".to_owned())
        .spawn(move || {
            thread::sleep(delay);
            let _ = check_once(&env, now(), fetch.as_ref(), &Version::current());
        })
        .is_ok()
}
