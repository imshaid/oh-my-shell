//! What the line where the hint bar goes may say about releases (UI upgrade, phase 5):
//!
//! - **news**, for a person who turned the daily check on: the stamp the check left says a newer
//!   release exists, and the line has not been shown in the last day;
//! - **the offline nudge**, for a person who left it off: this build is more than 60 days old,
//!   and the line has not been shown for this build.
//!
//! Both are read from small files in the state folder. **Nothing here uses the network.** A line
//! is counted as shown *before* it is returned, and is not returned when the count cannot be
//! written, so a read-only state folder never turns "once" into "every time".
//!
//! The age of the build comes from `OMSH_BUILD_EPOCH`, set when the release is built (the time of
//! the commit, so the same source gives the same number). A build with no such number (a
//! `cargo install`, a development build) says nothing: the age would be a guess.

use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;

use super::daily::{self, DAY, Stamp};
use super::version::Version;

/// The build's commit time in seconds since 1970, as text, if this build was given one.
pub const BUILD_EPOCH: Option<&str> = option_env!("OMSH_BUILD_EPOCH");

/// A build is old after this many days.
pub const OLD_AFTER_DAYS: u64 = 60;

/// The file that remembers which build the nudge was shown for.
const NUDGE_FILE: &str = "nudge";
/// The most that file may be.
const LIMIT: u64 = 64;

/// What the line says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    /// A newer release exists.
    Newer(Version),
    /// This build is `days` days old, and the daily check is off.
    OldBuild { days: u64 },
}

/// What the choice needs to know.
pub struct Inputs<'a> {
    pub state_dir: &'a Path,
    /// Seconds since 1970.
    pub now: u64,
    pub current: &'a Version,
    /// `[update] check` (or the answer given at the first start).
    pub check_on: bool,
    /// The build's age source: see [`BUILD_EPOCH`].
    pub build_epoch: Option<u64>,
}

/// The build epoch of this program, if it has one.
#[must_use]
pub fn build_epoch() -> Option<u64> {
    BUILD_EPOCH.and_then(|text| text.trim().parse().ok())
}

/// Whole days between `epoch` and `now`; 0 for a build from the future (a clock set back).
#[must_use]
pub fn age_days(epoch: u64, now: u64) -> u64 {
    now.saturating_sub(epoch) / DAY
}

/// The line to show at this start, if any. Counts it as shown.
#[must_use]
pub fn take(inputs: &Inputs<'_>) -> Option<Notice> {
    if inputs.check_on {
        news(inputs)
    } else {
        nudge(inputs)
    }
}

fn news(inputs: &Inputs<'_>) -> Option<Notice> {
    let stamp = daily::read(inputs.state_dir)?;
    let newest = stamp
        .latest
        .clone()
        .filter(|latest| latest > inputs.current)?;
    let shown_recently = stamp
        .noticed
        .is_some_and(|at| inputs.now.saturating_sub(at) < DAY && at <= inputs.now + DAY);
    if shown_recently {
        return None;
    }
    daily::write(
        inputs.state_dir,
        &Stamp {
            noticed: Some(inputs.now),
            ..stamp
        },
    )
    .ok()?;
    Some(Notice::Newer(newest))
}

fn nudge(inputs: &Inputs<'_>) -> Option<Notice> {
    let epoch = inputs.build_epoch?;
    let days = age_days(epoch, inputs.now);
    if days <= OLD_AFTER_DAYS || read_nudge(inputs.state_dir) == Some(epoch) {
        return None;
    }
    write_nudge(inputs.state_dir, epoch).ok()?;
    Some(Notice::OldBuild { days })
}

/// The build the nudge was last shown for.
fn read_nudge(dir: &Path) -> Option<u64> {
    let file = fs::File::open(dir.join(NUDGE_FILE)).ok()?;
    let mut text = String::new();
    file.take(LIMIT).read_to_string(&mut text).ok()?;
    text.lines()
        .find_map(|line| line.strip_prefix("build="))
        .and_then(|value| value.trim().parse().ok())
}

fn write_nudge(dir: &Path, epoch: u64) -> io::Result<()> {
    DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    let tmp = dir.join(format!("{NUDGE_FILE}.tmp-{}", std::process::id()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(format!("build={epoch}\n").as_bytes())?;
        drop(file);
        fs::rename(&tmp, dir.join(NUDGE_FILE))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests;
