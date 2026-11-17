use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use super::*;

/// A state folder of its own, removed afterwards.
struct State(PathBuf);

impl State {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("omsh-notice-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        Self(path)
    }

    fn stamp(&self, latest: &str, noticed: Option<u64>) {
        daily::write(
            &self.0,
            &Stamp {
                checked: 1_000,
                latest: Version::parse(latest),
                noticed,
            },
        )
        .unwrap();
    }
}

impl Drop for State {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700));
        let _ = fs::remove_dir_all(&self.0);
    }
}

const NOW: u64 = 100_000_000;

fn inputs<'a>(
    state: &'a State,
    current: &'a Version,
    check_on: bool,
    build_epoch: Option<u64>,
) -> Inputs<'a> {
    Inputs {
        state_dir: &state.0,
        now: NOW,
        current,
        check_on,
        build_epoch,
    }
}

fn v(text: &str) -> Version {
    Version::parse(text).unwrap()
}

// -- news -------------------------------------------------------------------------------------------

#[test]
fn a_newer_release_is_announced_once_in_a_day() {
    let state = State::new();
    state.stamp("1.1.0", None);
    let current = v("1.0.0");
    assert_eq!(
        take(&inputs(&state, &current, true, None)),
        Some(Notice::Newer(v("1.1.0")))
    );
    // Counted as shown before it was returned: the next start in the same day is quiet.
    assert_eq!(take(&inputs(&state, &current, true, None)), None);
    // A day later it is said again, if the person is still on the old version.
    let later = Inputs {
        now: NOW + DAY,
        ..inputs(&state, &current, true, None)
    };
    assert_eq!(take(&later), Some(Notice::Newer(v("1.1.0"))));
}

#[test]
fn the_same_or_an_older_release_says_nothing() {
    let state = State::new();
    for (latest, current) in [("1.0.0", "1.0.0"), ("0.9.0", "1.0.0")] {
        state.stamp(latest, None);
        assert_eq!(take(&inputs(&state, &v(current), true, None)), None);
    }
}

#[test]
fn no_stamp_means_no_news_and_nothing_is_written() {
    let state = State::new();
    assert_eq!(take(&inputs(&state, &v("1.0.0"), true, None)), None);
    assert!(!state.0.exists());
}

#[test]
fn a_stamp_from_the_future_does_not_hide_the_news_for_ever() {
    let state = State::new();
    state.stamp("1.1.0", Some(NOW + 10 * DAY));
    assert_eq!(
        take(&inputs(&state, &v("1.0.0"), true, None)),
        Some(Notice::Newer(v("1.1.0")))
    );
}

#[test]
fn news_that_cannot_be_counted_is_not_shown() {
    let state = State::new();
    state.stamp("1.1.0", None);
    // A folder that cannot be written: a read-only state folder must not turn once into always.
    fs::set_permissions(&state.0, fs::Permissions::from_mode(0o500)).unwrap();
    let result = take(&inputs(&state, &v("1.0.0"), true, None));
    // (Root can write anyway; then the news is shown, which is also right.)
    if !omsh_exec::is_root() {
        assert_eq!(result, None);
    }
}

// -- the offline nudge ------------------------------------------------------------------------------

#[test]
fn an_old_build_with_the_check_off_is_nudged_once_per_build() {
    let state = State::new();
    let current = v("1.0.0");
    let old = NOW - 61 * DAY;
    assert_eq!(
        take(&inputs(&state, &current, false, Some(old))),
        Some(Notice::OldBuild { days: 61 })
    );
    assert_eq!(take(&inputs(&state, &current, false, Some(old))), None);
    // A different build that is also old is nudged for its own sake.
    let other = NOW - 90 * DAY;
    assert_eq!(
        take(&inputs(&state, &current, false, Some(other))),
        Some(Notice::OldBuild { days: 90 })
    );
}

#[test]
fn sixty_days_is_not_old_and_sixty_one_is() {
    let state = State::new();
    let current = v("1.0.0");
    assert_eq!(
        take(&inputs(&state, &current, false, Some(NOW - 60 * DAY))),
        None
    );
    assert!(take(&inputs(&state, &current, false, Some(NOW - 61 * DAY))).is_some());
}

#[test]
fn a_build_with_no_date_is_never_nudged() {
    let state = State::new();
    assert_eq!(take(&inputs(&state, &v("1.0.0"), false, None)), None);
    assert!(!state.0.exists());
}

#[test]
fn a_build_from_the_future_is_not_old() {
    let state = State::new();
    assert_eq!(
        take(&inputs(&state, &v("1.0.0"), false, Some(NOW + 400 * DAY))),
        None
    );
}

#[test]
fn with_the_check_on_there_is_no_nudge_even_for_an_old_build() {
    let state = State::new();
    assert_eq!(
        take(&inputs(&state, &v("1.0.0"), true, Some(NOW - 400 * DAY))),
        None
    );
}

#[test]
fn the_age_is_whole_days() {
    assert_eq!(age_days(0, DAY - 1), 0);
    assert_eq!(age_days(0, DAY), 1);
    assert_eq!(age_days(100, 50), 0);
}

#[test]
fn the_files_are_private() {
    let state = State::new();
    assert!(take(&inputs(&state, &v("1.0.0"), false, Some(NOW - 100 * DAY))).is_some());
    let mode = |path: PathBuf| fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(state.0.clone()), 0o700);
    assert_eq!(mode(state.0.join("nudge")), 0o600);
}
