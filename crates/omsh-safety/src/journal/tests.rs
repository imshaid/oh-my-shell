use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use super::{ItemKind, Journal, Origin, UndoError, is_same_file};
use crate::trash::{self, meta};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const NEVER: &dyn Fn() -> bool = &|| false;

struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new() -> Self {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("omsh-journal-{}-{n}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self, name: &str) -> std::path::PathBuf {
        self.0.join(name)
    }

    fn journal(&self) -> Journal {
        Journal::open(&self.path("data")).unwrap()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = trash::remove_tree(&self.0);
    }
}

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777
}

fn write_with_mode(path: &Path, text: &str, mode: u32) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

#[test]
fn the_database_and_the_trash_are_private() {
    let dir = Scratch::new();
    let _journal = dir.journal();
    assert_eq!(mode(&dir.path("data")), 0o700);
    assert_eq!(mode(&dir.path("data/trash")), 0o700);
    assert_eq!(mode(&dir.path("data/undo.db")), 0o600);
}

#[test]
fn a_newer_database_is_refused() {
    let dir = Scratch::new();
    drop(dir.journal());
    let connection = rusqlite::Connection::open(dir.path("data/undo.db")).unwrap();
    connection.pragma_update(None, "user_version", 99).unwrap();
    drop(connection);
    let error = Journal::open(&dir.path("data")).err().unwrap();
    assert!(error.contains("newer version"), "{error}");
}

#[test]
fn undo_restores_trashed_files_with_their_permissions() {
    let dir = Scratch::new();
    let journal = dir.journal();
    let work = dir.path("work");
    write_with_mode(&work.join("secret.txt"), "s", 0o600);
    write_with_mode(&work.join("shared.txt"), "x", 0o640);
    write_with_mode(&work.join("tree/inner/f"), "deep", 0o644);
    fs::set_permissions(work.join("tree"), fs::Permissions::from_mode(0o750)).unwrap();
    fs::set_permissions(work.join("tree/inner"), fs::Permissions::from_mode(0o500)).unwrap();
    let action = journal.begin(&work, "rm -rf *", Origin::Typed).unwrap();
    for name in ["secret.txt", "shared.txt", "tree"] {
        assert_eq!(
            journal.trash(&action, &work.join(name), NEVER).unwrap(),
            None
        );
    }
    assert_eq!(journal.finish(&action), 3);
    assert!(!work.join("tree").exists() && !work.join("secret.txt").exists());

    let latest = journal.latest_undoable().unwrap();
    assert_eq!(latest.id, action.id);
    assert_eq!(latest.items, 3);
    assert_eq!(latest.command, "rm -rf *");
    let report = journal.undo(action.id, NEVER).unwrap();
    assert_eq!((report.undone, report.failed), (3, 0), "{:?}", report.lines);
    assert_eq!(mode(&work.join("secret.txt")), 0o600);
    assert_eq!(mode(&work.join("shared.txt")), 0o640);
    assert_eq!(mode(&work.join("tree")), 0o750);
    assert_eq!(mode(&work.join("tree/inner")), 0o500);
    assert_eq!(
        fs::read_to_string(work.join("tree/inner/f")).unwrap(),
        "deep"
    );
    // Done once: the second try says so and nothing changes.
    assert_eq!(
        journal.undo(action.id, NEVER).unwrap_err(),
        UndoError::NotUndoable(format!("#{} has been undone already", action.id))
    );
    assert!(journal.latest_undoable().is_none());
}

#[test]
fn undo_refuses_to_overwrite_and_leaves_the_file_in_the_trash() {
    let dir = Scratch::new();
    let journal = dir.journal();
    let file = dir.path("work/a");
    write_with_mode(&file, "old", 0o644);
    let action = journal
        .begin(dir.path("work").as_path(), "rm a", Origin::Typed)
        .unwrap();
    journal.trash(&action, &file, NEVER).unwrap();
    journal.finish(&action);
    write_with_mode(&file, "new", 0o644);
    let report = journal.undo(action.id, NEVER).unwrap();
    assert_eq!((report.undone, report.failed), (0, 1));
    assert!(report.lines[0].contains("exists now"), "{:?}", report.lines);
    assert_eq!(fs::read_to_string(&file).unwrap(), "new");
    // It can be tried again after the new file is gone: the action is `partial`.
    fs::remove_file(&file).unwrap();
    let report = journal.undo(action.id, NEVER).unwrap();
    assert_eq!((report.undone, report.failed), (1, 0), "{:?}", report.lines);
    assert_eq!(fs::read_to_string(&file).unwrap(), "old");
}

#[test]
fn an_action_that_moved_nothing_leaves_no_trace() {
    let dir = Scratch::new();
    let journal = dir.journal();
    let action = journal
        .begin(dir.path("work").as_path(), "rm x", Origin::Typed)
        .unwrap();
    assert!(journal.trash(&action, &dir.path("missing"), NEVER).is_err());
    assert_eq!(journal.finish(&action), 0);
    assert!(journal.list(10).is_empty());
    assert!(!action.dir.exists());
}

#[test]
fn a_crash_between_the_row_and_the_move_is_settled_by_looking_at_the_disk() {
    let dir = Scratch::new();
    let journal = dir.journal();
    let work = dir.path("work");
    write_with_mode(&work.join("moved"), "m", 0o644);
    write_with_mode(&work.join("kept"), "k", 0o644);
    let action = journal
        .begin(&work, "rm moved kept", Origin::Typed)
        .unwrap();
    journal.trash(&action, &work.join("moved"), NEVER).unwrap();
    journal.trash(&action, &work.join("kept"), NEVER).unwrap();
    // Pretend the second move never happened and the shell died: row `moving`, file in place.
    let connection = rusqlite::Connection::open(dir.path("data/undo.db")).unwrap();
    connection
        .execute("UPDATE item SET state = 'moving' WHERE seq = 2", [])
        .unwrap();
    connection
        .execute(
            "UPDATE action SET state = 'open', at_ms = at_ms - 120000 WHERE id = ?1",
            [action.id],
        )
        .unwrap();
    drop(connection);
    let trashed = journal.items(action.id)[1].other.clone().unwrap();
    fs::rename(&trashed, work.join("kept")).unwrap();
    let report = journal.undo(action.id, NEVER).unwrap();
    assert_eq!(report.failed, 0, "{:?}", report.lines);
    assert!(work.join("moved").exists() && work.join("kept").exists());
    assert!(
        report.lines.iter().any(|l| l.contains("never moved")),
        "{:?}",
        report.lines
    );
}

#[test]
fn a_recent_open_action_belongs_to_another_shell() {
    let dir = Scratch::new();
    let journal = dir.journal();
    let action = journal
        .begin(dir.path("w").as_path(), "x", Origin::Plan)
        .unwrap();
    assert!(matches!(
        journal.undo(action.id, NEVER),
        Err(UndoError::NotUndoable(why)) if why.contains("still being recorded")
    ));
}

#[test]
fn two_claims_of_the_same_action_give_one_winner() {
    let dir = Scratch::new();
    let journal = dir.journal();
    let work = dir.path("work");
    write_with_mode(&work.join("f"), "x", 0o644);
    let action = journal.begin(&work, "rm f", Origin::Typed).unwrap();
    journal.trash(&action, &work.join("f"), NEVER).unwrap();
    journal.finish(&action);
    let other = Journal::open(&dir.path("data")).unwrap();
    let id = action.id;
    let results: Vec<_> = std::thread::scope(|scope| {
        let a = scope.spawn(|| journal.undo(id, NEVER).is_ok());
        let b = scope.spawn(|| other.undo(id, NEVER).is_ok());
        vec![a.join().unwrap(), b.join().unwrap()]
    });
    assert_eq!(results.iter().filter(|ok| **ok).count(), 1, "{results:?}");
    assert_eq!(fs::read_to_string(work.join("f")).unwrap(), "x");
}

#[test]
fn undo_reverses_mkdir_rename_and_write_in_reverse_order() {
    let dir = Scratch::new();
    let journal = dir.journal();
    let work = dir.path("work");
    write_with_mode(&work.join("config"), "before", 0o640);
    write_with_mode(&work.join("a.txt"), "a", 0o644);
    let action = journal.begin(&work, "plan", Origin::Plan).unwrap();
    // mkdir out; mv a.txt out/a.txt; echo after > config
    fs::create_dir(work.join("out")).unwrap();
    journal.note_mkdir(&action, &work.join("out")).unwrap();
    fs::rename(work.join("a.txt"), work.join("out/a.txt")).unwrap();
    journal
        .note_rename(&action, &work.join("a.txt"), &work.join("out/a.txt"))
        .unwrap();
    let backup_dir = action.backup_folder().unwrap();
    let backup = backup_dir.join("config");
    let before = meta(&work.join("config")).unwrap();
    fs::copy(work.join("config"), &backup).unwrap();
    fs::write(work.join("config"), "after").unwrap();
    journal
        .note_write(&action, &work.join("config"), Some((&backup, &before)))
        .unwrap();
    fs::write(work.join("new.txt"), "n").unwrap();
    journal
        .note_write(&action, &work.join("new.txt"), None)
        .unwrap();
    assert_eq!(journal.finish(&action), 4);
    let kinds: Vec<_> = journal.items(action.id).iter().map(|i| i.kind).collect();
    assert_eq!(
        kinds,
        [
            ItemKind::Mkdir,
            ItemKind::Rename,
            ItemKind::Write,
            ItemKind::Created
        ]
    );

    let report = journal.undo(action.id, NEVER).unwrap();
    assert_eq!((report.undone, report.failed), (4, 0), "{:?}", report.lines);
    // The files the undo replaced are in the trash under an action of its own, and a plain
    // `undo` does not pick that one: it would undo the undo.
    assert!(journal.latest_undoable().is_none());
    assert_eq!(fs::read_to_string(work.join("config")).unwrap(), "before");
    assert_eq!(mode(&work.join("config")), 0o640);
    assert!(work.join("a.txt").exists());
    assert!(!work.join("out").exists());
    assert!(!work.join("new.txt").exists());
    // What the undo took away is in the trash, under an action of its own.
    let undo_action = journal
        .list(10)
        .into_iter()
        .find(|a| a.origin == Origin::Undo)
        .unwrap();
    assert_eq!(undo_action.items, 2);
    // Undoing the undo brings the new file back; `config` has its old content again, so that
    // one is refused rather than overwritten.
    let replaced = journal.undo(undo_action.id, NEVER).unwrap();
    assert_eq!(
        (replaced.undone, replaced.failed),
        (1, 1),
        "{:?}",
        replaced.lines
    );
    assert_eq!(fs::read_to_string(work.join("new.txt")).unwrap(), "n");
    assert_eq!(fs::read_to_string(work.join("config")).unwrap(), "before");
}

#[test]
fn mkdir_undo_keeps_a_folder_that_has_files_now() {
    let dir = Scratch::new();
    let journal = dir.journal();
    let work = dir.path("work");
    fs::create_dir_all(work.join("out")).unwrap();
    let action = journal.begin(&work, "mkdir out", Origin::Plan).unwrap();
    journal.note_mkdir(&action, &work.join("out")).unwrap();
    journal.finish(&action);
    fs::write(work.join("out/precious"), "p").unwrap();
    let report = journal.undo(action.id, NEVER).unwrap();
    assert_eq!(report.failed, 1);
    assert!(report.lines[0].contains("not empty"), "{:?}", report.lines);
    assert!(work.join("out/precious").exists());
}

fn some_meta(ino: u64, mtime_s: i64) -> trash::Meta {
    trash::Meta {
        kind: trash::Kind::File,
        mode: 0o644,
        uid: 1000,
        gid: 1000,
        mtime_s,
        mtime_ns: 5,
        dev: 7,
        ino,
    }
}

#[test]
fn the_birth_time_tells_a_replacement_with_the_same_inode_from_the_moved_file() {
    let at_ms = 1_700_000_000_000_i64;
    let at = UNIX_EPOCH + Duration::from_millis(at_ms as u64);
    let then = some_meta(42, 100);
    let ok = |born| is_same_file(&then, &then, &born, at_ms);
    // Born long before the action, at the same moment (cut to a millisecond), 1 ms after.
    assert!(ok(Ok(at - Duration::from_secs(3600))));
    assert!(ok(Ok(at)));
    assert!(ok(Ok(at + Duration::from_millis(1))));
    // A file born after the action is a new file that got the old number.
    assert!(!ok(Ok(at + Duration::from_millis(2))));
    assert!(!ok(Ok(at + Duration::from_secs(60))));
    // The inode and the device still have to match.
    let other = some_meta(43, 100);
    assert!(!is_same_file(&other, &then, &Ok(at), at_ms));
    let mut other_device = then;
    other_device.dev = 8;
    assert!(!is_same_file(&other_device, &then, &Ok(at), at_ms));
}

#[test]
fn a_file_system_without_birth_times_falls_back_to_the_modification_time() {
    let gone = || Err(std::io::Error::from(std::io::ErrorKind::Unsupported));
    let then = some_meta(42, 100);
    assert!(is_same_file(&then, &then, &gone(), 1));
    // Edited after the move: refused here, because without a birth time it cannot be told
    // from a replacement.
    assert!(!is_same_file(&some_meta(42, 101), &then, &gone(), 1));
    let mut nanos = then;
    nanos.mtime_ns += 1;
    assert!(!is_same_file(&nanos, &then, &gone(), 1));
}

#[test]
fn rename_undo_refuses_a_file_born_after_the_action() {
    let dir = Scratch::new();
    let journal = dir.journal();
    let work = dir.path("work");
    write_with_mode(&work.join("a"), "a", 0o644);
    let action = journal.begin(&work, "mv a b", Origin::Plan).unwrap();
    fs::rename(work.join("a"), work.join("b")).unwrap();
    journal
        .note_rename(&action, &work.join("a"), &work.join("b"))
        .unwrap();
    journal.finish(&action);
    // Edited after the move: the same file, so it goes back.
    fs::write(work.join("b"), "edited later").unwrap();
    // Now the journal says the action was recorded an hour before this file was born, which is
    // what a replacement that took over the inode number looks like.
    let hour_ago = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
        - 3_600_000;
    journal
        .lock()
        .execute(
            "UPDATE action SET at_ms = ?1 WHERE id = ?2",
            rusqlite::params![hour_ago, action.id],
        )
        .unwrap();
    let report = journal.undo(action.id, NEVER).unwrap();
    assert_eq!(report.failed, 1, "{:?}", report.lines);
    assert!(
        report.lines[0].contains("not the file that was moved"),
        "{:?}",
        report.lines
    );
    assert!(work.join("b").exists());
    assert!(!work.join("a").exists());
}

#[test]
fn rename_undo_takes_back_a_file_edited_after_the_move() {
    let dir = Scratch::new();
    let journal = dir.journal();
    let work = dir.path("work");
    write_with_mode(&work.join("a"), "a", 0o644);
    let action = journal.begin(&work, "mv a b", Origin::Plan).unwrap();
    fs::rename(work.join("a"), work.join("b")).unwrap();
    journal
        .note_rename(&action, &work.join("a"), &work.join("b"))
        .unwrap();
    journal.finish(&action);
    std::thread::sleep(Duration::from_millis(5));
    fs::write(work.join("b"), "edited later").unwrap();
    let report = journal.undo(action.id, NEVER).unwrap();
    assert_eq!((report.undone, report.failed), (1, 0), "{:?}", report.lines);
    assert_eq!(fs::read_to_string(work.join("a")).unwrap(), "edited later");
}

#[test]
fn rename_undo_checks_that_it_is_the_same_file() {
    let dir = Scratch::new();
    let journal = dir.journal();
    let work = dir.path("work");
    write_with_mode(&work.join("a"), "a", 0o644);
    let action = journal.begin(&work, "mv a b", Origin::Plan).unwrap();
    fs::rename(work.join("a"), work.join("b")).unwrap();
    journal
        .note_rename(&action, &work.join("a"), &work.join("b"))
        .unwrap();
    journal.finish(&action);
    // Someone replaced b by another file. The new one is made while the old one still exists
    // and then renamed over it, so the two cannot share an inode number (a file system may
    // hand out the number of a file that was just deleted again).
    fs::write(work.join("c"), "other").unwrap();
    fs::rename(work.join("c"), work.join("b")).unwrap();
    let report = journal.undo(action.id, NEVER).unwrap();
    assert_eq!(report.failed, 1);
    assert!(
        report.lines[0].contains("not the file that was moved"),
        "{:?}",
        report.lines
    );
    assert_eq!(fs::read_to_string(work.join("b")).unwrap(), "other");
}

#[test]
fn forget_before_removes_old_rows() {
    let dir = Scratch::new();
    let journal = dir.journal();
    let work = dir.path("work");
    write_with_mode(&work.join("f"), "x", 0o644);
    let action = journal.begin(&work, "rm f", Origin::Typed).unwrap();
    journal.trash(&action, &work.join("f"), NEVER).unwrap();
    journal.finish(&action);
    journal.forget_before(0);
    assert_eq!(journal.list(10).len(), 1);
    journal.forget_before(i64::MAX);
    assert!(journal.list(10).is_empty());
}

#[test]
fn a_purged_trash_is_reported_not_guessed() {
    let dir = Scratch::new();
    let journal = dir.journal();
    let work = dir.path("work");
    write_with_mode(&work.join("f"), "x", 0o644);
    let action = journal.begin(&work, "rm f", Origin::Typed).unwrap();
    journal.trash(&action, &work.join("f"), NEVER).unwrap();
    journal.finish(&action);
    trash::remove_tree(&action.dir).unwrap();
    let report = journal.undo(action.id, NEVER).unwrap();
    assert_eq!(report.failed, 1);
    assert!(
        report.lines[0].contains("no longer in the trash"),
        "{:?}",
        report.lines
    );
}

/// The row of a move is on disk before the file is touched (write-ahead). Seen from a second
/// connection while a copy to another file system is under way.
#[test]
fn the_row_is_written_before_the_files_are_moved() {
    use std::os::unix::fs::MetadataExt;
    let shm = std::path::PathBuf::from(format!("/dev/shm/omsh-journal-{}", std::process::id()));
    let _ = fs::remove_dir_all(&shm);
    let scratch = Scratch::new();
    let work = scratch.path("work");
    fs::create_dir_all(&work).unwrap();
    if fs::create_dir_all(&shm).is_err()
        || fs::metadata(&shm).unwrap().dev() == fs::metadata(&work).unwrap().dev()
    {
        // No second file system here: the copy path cannot be reached.
        use std::io::Write;
        let _ = std::io::stderr().write_all(
            b"note: the_row_is_written_before_the_files_are_moved SKIPPED: no second file system\n",
        );
        assert!(
            std::env::var_os("CI").is_none_or(|v| v != "true"),
            "CI must run this test"
        );
        return;
    }
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = trash::remove_tree(&self.0);
        }
    }
    let _cleanup = Cleanup(shm.clone());
    let journal = Journal::open(&shm.join("data")).unwrap();
    write_with_mode(&work.join("tree/a"), "a", 0o644);
    write_with_mode(&work.join("tree/b"), "b", 0o644);
    let action = journal.begin(&work, "rm -r tree", Origin::Typed).unwrap();
    let seen = std::cell::RefCell::new(Vec::new());
    let watcher = rusqlite::Connection::open(shm.join("data/undo.db")).unwrap();
    let spy = || {
        let state: Option<String> = watcher
            .query_row("SELECT state FROM item WHERE kind = 'trash'", [], |row| {
                row.get(0)
            })
            .ok();
        seen.borrow_mut().push(state);
        false
    };
    assert_eq!(
        journal.trash(&action, &work.join("tree"), &spy).unwrap(),
        None
    );
    let seen = seen.into_inner();
    assert!(!seen.is_empty(), "the copy looked at the clock");
    assert!(
        seen.iter().all(|state| state.as_deref() == Some("moving")),
        "the row was `moving` while the files were copied: {seen:?}"
    );
    assert_eq!(journal.items(action.id)[0].state, "done");
}
