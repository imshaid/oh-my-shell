//! Waiting for files that the shell creates on a thread of its own.
//!
//! **The rule for test authors.** The shell does not wait for its history writer: the writer
//! thread makes the data directory and the database file *after* the first prompt is drawn
//! (`Store::open` returns at once). A test that opens, reads or counts such a file right after
//! `ctx.start()` races that thread. It passes on a fast machine and fails on a loaded runner with
//! `unable to open database file` (CI run 43245864003, `a_line_that_replaces_the_shell_is_stored`).
//!
//! So a test that touches a file the shell creates in the background must first wait for it,
//! with a bound, using the helpers here:
//!
//! - [`wait_for_file`]: the file exists. For any file made on a background thread.
//! - [`wait_for_wal_database`]: the file is the history database *and* the writer has switched it
//!   to WAL mode, which is the first thing the writer does to it. Use this (through
//!   [`crate::Ctx::wait_for_history_file`]) before opening the history database yourself.
//! - [`crate::Session::wait_for_history`]: the shell's own view. Use it before a test relies on
//!   the history being readable at once, for example before `Ctrl+R` or before `history`.
//!
//! Reading a file after the shell has exited (`quit`) needs no wait: the shell waits for its
//! writer when it ends. Files that the shell makes on its main thread before the first prompt
//! (the first-run `config.toml`, `undo.db` and the trash, which are made while a command runs)
//! need no wait either.

use std::fs::{self, File};
use std::io::Read;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

/// How often the file is looked at.
const POLL: Duration = Duration::from_millis(5);

/// Waits until `path` is a regular file, for at most `limit`.
///
/// # Panics
///
/// When the file is still missing after `limit`. The message names the file and lists what the
/// folder holds, so that a wrong path is seen at once.
pub fn wait_for_file(path: &Path, limit: Duration) {
    let end = Instant::now() + limit;
    loop {
        if fs::metadata(path).is_ok_and(|kind| kind.is_file()) {
            return;
        }
        if Instant::now() >= end {
            panic!(
                "{} did not appear as a regular file within {limit:?}; {}",
                path.display(),
                describe(path)
            );
        }
        thread::sleep(POLL);
    }
}

/// Waits until `path` is an SQLite database in WAL mode, for at most `limit`: the writer has made
/// the file and run `PRAGMA journal_mode = WAL`, so it is safe to open the file from the test.
///
/// # Panics
///
/// When that has not happened after `limit`. The message says what the file looks like.
pub fn wait_for_wal_database(path: &Path, limit: Duration) {
    let end = Instant::now() + limit;
    loop {
        if is_wal_database(path) {
            return;
        }
        if Instant::now() >= end {
            panic!(
                "{} was not an SQLite database in WAL mode within {limit:?}; {}",
                path.display(),
                describe(path)
            );
        }
        thread::sleep(POLL);
    }
}

/// True when `path` is an SQLite database in WAL mode. The header of an SQLite file is 16 bytes
/// of text, and at offsets 18 and 19 the file format versions for writing and reading, which are
/// 2 for a WAL database and 1 otherwise.
#[must_use]
pub fn is_wal_database(path: &Path) -> bool {
    let mut header = [0u8; 20];
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    file.read_exact(&mut header).is_ok()
        && header.starts_with(b"SQLite format 3\0")
        && header[18] == 2
        && header[19] == 2
}

/// What is at `path` and around it, for a failure message.
fn describe(path: &Path) -> String {
    let own = match fs::metadata(path) {
        Ok(kind) if kind.is_file() => format!("it is a file of {} bytes", kind.len()),
        Ok(_) => "it exists but is not a regular file".to_owned(),
        Err(error) => format!("it cannot be looked at ({error})"),
    };
    let around = match path.parent().map(fs::read_dir) {
        Some(Ok(entries)) => {
            let mut names: Vec<String> = entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            format!("the folder holds {names:?}")
        }
        Some(Err(error)) => format!("the folder cannot be read ({error})"),
        None => "it has no folder".to_owned(),
    };
    format!("{own}; {around}")
}

#[cfg(test)]
mod tests {
    use std::panic::catch_unwind;
    use std::path::PathBuf;

    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("omsh-files-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const PATIENT: Duration = Duration::from_secs(10);

    /// The panic message of `work`, which must panic.
    fn panic_message(work: impl FnOnce() + std::panic::UnwindSafe) -> String {
        let payload = catch_unwind(work).expect_err("this was meant to panic");
        payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| {
                payload
                    .downcast_ref::<&str>()
                    .map(|text| (*text).to_owned())
            })
            .unwrap_or_default()
    }

    #[test]
    fn a_file_that_exists_is_returned_at_once() {
        let dir = Scratch::new("exists");
        fs::write(dir.path("here"), "x").unwrap();
        let start = Instant::now();
        wait_for_file(&dir.path("here"), PATIENT);
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn a_file_made_later_on_another_thread_is_waited_for() {
        let dir = Scratch::new("later");
        // The folder does not exist either, as with the shell's data directory.
        let file = dir.path("data/history.db");
        let maker = {
            let file = file.clone();
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(250));
                fs::create_dir_all(file.parent().unwrap()).unwrap();
                fs::write(&file, "x").unwrap();
            })
        };
        let start = Instant::now();
        wait_for_file(&file, PATIENT);
        assert!(
            start.elapsed() >= Duration::from_millis(200),
            "returned before the file existed: {:?}",
            start.elapsed()
        );
        assert!(file.is_file());
        maker.join().unwrap();
    }

    #[test]
    fn a_file_that_never_comes_fails_after_the_limit_and_names_the_folder() {
        let dir = Scratch::new("never");
        fs::write(dir.path("neighbour.txt"), "x").unwrap();
        let wanted = dir.path("history.db");
        let start = Instant::now();
        let message = {
            let wanted = wanted.clone();
            panic_message(move || wait_for_file(&wanted, Duration::from_millis(300)))
        };
        assert!(
            start.elapsed() >= Duration::from_millis(300),
            "gave up early"
        );
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "waited far too long"
        );
        assert!(message.contains("history.db"), "{message}");
        assert!(message.contains("neighbour.txt"), "{message}");
    }

    #[test]
    fn a_folder_is_not_the_file() {
        let dir = Scratch::new("folder");
        fs::create_dir_all(dir.path("history.db")).unwrap();
        let wanted = dir.path("history.db");
        let message = panic_message(move || wait_for_file(&wanted, Duration::from_millis(100)));
        assert!(message.contains("not a regular file"), "{message}");
    }

    #[test]
    fn a_database_is_ready_only_once_it_is_in_wal_mode() {
        let dir = Scratch::new("wal");
        let file = dir.path("history.db");
        // Not there.
        assert!(!is_wal_database(&file));
        // Made but empty: the writer has opened the file and not done anything to it yet.
        fs::write(&file, b"").unwrap();
        assert!(!is_wal_database(&file));
        // A database in the default mode.
        let connection = rusqlite::Connection::open(&file).unwrap();
        connection.execute_batch("CREATE TABLE t (x)").unwrap();
        assert!(!is_wal_database(&file));
        // And in WAL mode.
        let mode: String = connection
            .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        assert!(is_wal_database(&file));
        // Something that is not a database.
        fs::write(dir.path("other"), vec![2u8; 100]).unwrap();
        assert!(!is_wal_database(&dir.path("other")));
    }

    #[test]
    fn the_wal_wait_follows_a_writer_that_is_slow_to_switch_the_mode() {
        let dir = Scratch::new("slow-wal");
        let file = dir.path("history.db");
        let connection = rusqlite::Connection::open(&file).unwrap();
        connection.execute_batch("CREATE TABLE t (x)").unwrap();
        let writer = thread::spawn(move || {
            thread::sleep(Duration::from_millis(250));
            let _: String = connection
                .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
                .unwrap();
            // Keep the connection open a moment: closing the last one may fold the log away.
            thread::sleep(Duration::from_millis(200));
        });
        let start = Instant::now();
        wait_for_wal_database(&file, PATIENT);
        assert!(start.elapsed() >= Duration::from_millis(200));
        writer.join().unwrap();
    }

    #[test]
    fn the_wal_wait_gives_up_with_a_description_of_the_file() {
        let dir = Scratch::new("no-wal");
        let file = dir.path("history.db");
        fs::write(&file, b"not a database").unwrap();
        let message =
            panic_message(move || wait_for_wal_database(&file, Duration::from_millis(100)));
        assert!(message.contains("WAL mode"), "{message}");
        assert!(message.contains("14 bytes"), "{message}");
    }
}
