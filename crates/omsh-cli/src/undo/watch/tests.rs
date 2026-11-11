use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use omsh_exec::Shell;
use omsh_safety::journal::{ItemKind, Origin};
use omsh_safety::{Context, analyze};

use super::{MAX_BACKUP, Watch};
use crate::probe::ShellProbe;
use crate::undo::Undo;

struct Dir(PathBuf);

impl Dir {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("omsh-watch-{}-{n}", std::process::id()));
        fs::create_dir_all(path.join("work")).unwrap();
        Self(path)
    }

    fn work(&self, name: &str) -> PathBuf {
        self.0.join("work").join(name)
    }

    fn undo(&self) -> Arc<Undo> {
        Undo::new(self.0.join("data"))
    }

    fn cwd(&self) -> PathBuf {
        self.0.join("work")
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = omsh_safety::trash::remove_tree(&self.0);
    }
}

/// Runs `line` the way a plan step runs: watch before, run with `sh`, watch after. Returns the
/// notes, and leaves the line's action in the journal.
fn step(dir: &Dir, undo: &Undo, line: &str) -> Vec<String> {
    let cwd = dir.cwd();
    let prepared = Shell::new().prepare(line, true).unwrap();
    let ctx = Context {
        cwd: Some(&cwd),
        home: Some(Path::new("/home/me")),
        probe: &ShellProbe,
    };
    let analysis = analyze(prepared.list(), prepared.text(), &ctx);
    undo.begin_line(line, Origin::Plan);
    let watch = Watch::before(undo, &analysis.changes, &cwd);
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(line)
        .current_dir(&cwd)
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    let _ = status;
    let notes = watch.after(undo, &cwd);
    undo.end_line();
    notes
}

fn kinds(undo: &Undo) -> Vec<ItemKind> {
    let journal = undo.journal().unwrap();
    journal
        .latest_undoable()
        .map(|action| journal.items(action.id).iter().map(|i| i.kind).collect())
        .unwrap_or_default()
}

fn undo_latest(undo: &Undo) -> Vec<String> {
    let journal = undo.journal().unwrap();
    let id = journal.latest_undoable().unwrap().id;
    let report = journal.undo(id, &|| false).unwrap();
    assert_eq!(report.failed, 0, "{:?}", report.lines);
    report.lines
}

#[test]
fn mkdir_p_records_every_folder_it_made_and_undo_removes_the_innermost_first() {
    let dir = Dir::new();
    let undo = dir.undo();
    fs::create_dir(dir.work("exists")).unwrap();
    step(&dir, &undo, "mkdir -p exists/a/b/c");
    assert_eq!(kinds(&undo), [ItemKind::Mkdir; 3]);
    undo_latest(&undo);
    assert!(dir.work("exists").exists());
    assert!(!dir.work("exists/a").exists());
}

#[test]
fn mkdir_of_an_existing_folder_records_nothing() {
    let dir = Dir::new();
    let undo = dir.undo();
    fs::create_dir(dir.work("here")).unwrap();
    step(&dir, &undo, "mkdir here || true");
    assert!(kinds(&undo).is_empty());
    assert!(undo.journal().map_or(true, |j| j.list(5).is_empty()));
}

#[test]
fn mv_into_a_folder_made_in_the_same_step_is_followed() {
    let dir = Dir::new();
    let undo = dir.undo();
    fs::write(dir.work("a.txt"), "a").unwrap();
    step(&dir, &undo, "mkdir out && mv a.txt out/");
    assert_eq!(kinds(&undo), [ItemKind::Mkdir, ItemKind::Rename]);
    assert!(dir.work("out/a.txt").exists());
    undo_latest(&undo);
    assert!(dir.work("a.txt").exists());
    assert!(!dir.work("out").exists());
}

#[test]
fn mv_of_several_files_and_patterns_into_a_folder() {
    let dir = Dir::new();
    let undo = dir.undo();
    fs::create_dir(dir.work("dest")).unwrap();
    for name in ["a.log", "b.log", "c.txt"] {
        fs::write(dir.work(name), name).unwrap();
    }
    step(&dir, &undo, "mv *.log dest");
    assert_eq!(kinds(&undo), [ItemKind::Rename, ItemKind::Rename]);
    assert!(dir.work("dest/a.log").exists() && dir.work("c.txt").exists());
    undo_latest(&undo);
    assert!(dir.work("a.log").exists() && dir.work("b.log").exists());
    assert!(!dir.work("dest/a.log").exists());
}

#[test]
fn mv_over_a_file_keeps_the_old_content_and_the_undo_brings_both_back() {
    let dir = Dir::new();
    let undo = dir.undo();
    fs::write(dir.work("new"), "new").unwrap();
    fs::write(dir.work("old"), "old").unwrap();
    fs::set_permissions(dir.work("old"), fs::Permissions::from_mode(0o640)).unwrap();
    step(&dir, &undo, "mv new old");
    assert_eq!(kinds(&undo), [ItemKind::Write, ItemKind::Rename]);
    assert_eq!(fs::read_to_string(dir.work("old")).unwrap(), "new");
    undo_latest(&undo);
    assert_eq!(fs::read_to_string(dir.work("new")).unwrap(), "new");
    assert_eq!(fs::read_to_string(dir.work("old")).unwrap(), "old");
    assert_eq!(
        fs::metadata(dir.work("old")).unwrap().permissions().mode() & 0o7777,
        0o640
    );
}

#[test]
fn a_failed_mv_records_nothing() {
    let dir = Dir::new();
    let undo = dir.undo();
    fs::write(dir.work("keep"), "k").unwrap();
    let notes = step(&dir, &undo, "mv missing keep || true");
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(fs::read_to_string(dir.work("keep")).unwrap(), "k");
    assert!(undo.journal().map_or(true, |j| j.list(5).is_empty()));
}

#[test]
fn writes_keep_the_old_content_and_new_files_are_removed_by_undo() {
    let dir = Dir::new();
    let undo = dir.undo();
    fs::write(dir.work("cfg"), "old").unwrap();
    fs::set_permissions(dir.work("cfg"), fs::Permissions::from_mode(0o600)).unwrap();
    step(
        &dir,
        &undo,
        "echo one > cfg; echo two >> cfg; echo n > fresh",
    );
    // `cfg` once (the second write is the same file), `fresh` once.
    assert_eq!(kinds(&undo), [ItemKind::Write, ItemKind::Created]);
    assert_eq!(fs::read_to_string(dir.work("cfg")).unwrap(), "one\ntwo\n");
    undo_latest(&undo);
    assert_eq!(fs::read_to_string(dir.work("cfg")).unwrap(), "old");
    assert_eq!(
        fs::metadata(dir.work("cfg")).unwrap().permissions().mode() & 0o7777,
        0o600
    );
    assert!(!dir.work("fresh").exists());
}

#[test]
fn a_write_that_leaves_the_file_as_it_was_is_not_recorded() {
    let dir = Dir::new();
    let undo = dir.undo();
    fs::write(dir.work("cfg"), "same").unwrap();
    // The redirect is on a command that fails before writing, but the shell still opens the
    // file with `>>` and writes nothing.
    step(&dir, &undo, ": >> cfg");
    assert!(kinds(&undo).is_empty());
    assert!(undo.journal().map_or(true, |j| j.list(5).is_empty()));
}

#[test]
fn devices_and_unknown_words_are_not_followed_and_say_so() {
    let dir = Dir::new();
    let undo = dir.undo();
    let notes = step(
        &dir,
        &undo,
        "echo x > /dev/null; for f in a; do mkdir $f; done",
    );
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(
        notes[0].contains("not undoable: mkdir") && notes[0].contains("only known when"),
        "{notes:?}"
    );
    // The folder was made all the same; it is just not journaled.
    assert!(dir.work("a").exists());
    assert!(kinds(&undo).is_empty());
}

#[test]
fn a_file_over_the_backup_limit_is_said_not_kept() {
    let dir = Dir::new();
    let undo = dir.undo();
    let big = fs::File::create(dir.work("big")).unwrap();
    big.set_len(MAX_BACKUP + 1).unwrap();
    drop(big);
    let notes = step(&dir, &undo, "echo x > big");
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(
        notes[0].contains("is over 10 MiB and is not kept"),
        "{notes:?}"
    );
    assert!(kinds(&undo).is_empty(), "no entry that could not be undone");
}

#[test]
fn a_command_through_sudo_is_not_followed() {
    let dir = Dir::new();
    let undo = dir.undo();
    let cwd = dir.cwd();
    let line = "sudo mkdir /x";
    let prepared = Shell::new().prepare(line, true).unwrap();
    let ctx = Context {
        cwd: Some(&cwd),
        home: None,
        probe: &ShellProbe,
    };
    let analysis = analyze(prepared.list(), prepared.text(), &ctx);
    let watch = Watch::before(&undo, &analysis.changes, &cwd);
    let notes = watch.after(&undo, &cwd);
    assert_eq!(notes.len(), 1);
    assert!(notes[0].contains("runs through sudo"), "{notes:?}");
}

#[test]
fn a_mv_that_does_not_move_anything_records_nothing() {
    let dir = Dir::new();
    let undo = dir.undo();
    fs::write(dir.work("new"), "new").unwrap();
    fs::write(dir.work("old"), "old").unwrap();
    // `-n` leaves an existing destination alone: both files are still where they were, and a
    // file of the same kind is at the destination all the same.
    let notes = step(&dir, &undo, "mv -n new old");
    assert!(notes.is_empty(), "{notes:?}");
    assert!(kinds(&undo).is_empty());
    assert_eq!(fs::read_to_string(dir.work("old")).unwrap(), "old");
    assert!(undo.journal().map_or(true, |j| j.list(5).is_empty()));
}
