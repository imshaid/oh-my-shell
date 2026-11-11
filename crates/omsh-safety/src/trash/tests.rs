use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use super::{
    Kind, MoveError, Moved, RestoreError, claim_for_purge, meta, move_by_copy, move_to,
    old_folders, remove_tree, restore,
};

/// A folder in the temporary directory, made writable and removed at the end.
pub(crate) struct Scratch(pub PathBuf);

impl Scratch {
    pub(crate) fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("omsh-trash-{}-{n}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    pub(crate) fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = remove_tree(&self.0);
    }
}

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777
}

const NEVER: &dyn Fn() -> bool = &|| false;

#[test]
fn a_rename_moves_a_file_and_keeps_its_mode() {
    let dir = Scratch::new();
    fs::write(dir.path("a"), "hello").unwrap();
    fs::set_permissions(dir.path("a"), fs::Permissions::from_mode(0o640)).unwrap();
    let moved = move_to(&dir.path("a"), &dir.path("b"), NEVER).unwrap();
    assert_eq!(moved, Moved::Renamed);
    assert!(!dir.path("a").exists());
    assert_eq!(fs::read_to_string(dir.path("b")).unwrap(), "hello");
    assert_eq!(mode(&dir.path("b")), 0o640);
}

#[test]
fn a_folder_without_write_permission_can_be_renamed_into_another_folder() {
    let dir = Scratch::new();
    fs::create_dir_all(dir.path("from/locked")).unwrap();
    fs::create_dir(dir.path("to")).unwrap();
    fs::write(dir.path("from/locked/f"), "x").unwrap();
    fs::set_permissions(dir.path("from/locked"), fs::Permissions::from_mode(0o500)).unwrap();
    move_to(&dir.path("from/locked"), &dir.path("to/locked"), NEVER).unwrap();
    assert_eq!(mode(&dir.path("to/locked")), 0o500);
    assert!(dir.path("to/locked/f").exists());
}

#[test]
fn a_copy_move_keeps_modes_times_links_and_content() {
    let dir = Scratch::new();
    let tree = dir.path("tree");
    fs::create_dir_all(tree.join("sub/deep")).unwrap();
    fs::write(tree.join("a.txt"), "alpha").unwrap();
    fs::write(tree.join("sub/b.txt"), vec![7_u8; 200_000]).unwrap();
    symlink("a.txt", tree.join("link")).unwrap();
    symlink("/nonexistent/target", tree.join("broken")).unwrap();
    fs::set_permissions(tree.join("a.txt"), fs::Permissions::from_mode(0o640)).unwrap();
    fs::set_permissions(tree.join("sub/b.txt"), fs::Permissions::from_mode(0o600)).unwrap();
    fs::set_permissions(tree.join("sub/deep"), fs::Permissions::from_mode(0o500)).unwrap();
    fs::set_permissions(tree.join("sub"), fs::Permissions::from_mode(0o750)).unwrap();
    let before = meta(&tree.join("a.txt")).unwrap();
    let moved = move_by_copy(&tree, &dir.path("copy"), NEVER).unwrap();
    assert_eq!(moved, Moved::Copied);
    assert!(!tree.exists(), "the original is removed");
    let copy = dir.path("copy");
    assert_eq!(fs::read_to_string(copy.join("a.txt")).unwrap(), "alpha");
    assert_eq!(fs::read(copy.join("sub/b.txt")).unwrap().len(), 200_000);
    assert_eq!(
        fs::read_link(copy.join("link")).unwrap(),
        Path::new("a.txt")
    );
    assert_eq!(
        fs::read_link(copy.join("broken")).unwrap(),
        Path::new("/nonexistent/target")
    );
    assert_eq!(mode(&copy.join("a.txt")), 0o640);
    assert_eq!(mode(&copy.join("sub/b.txt")), 0o600);
    assert_eq!(mode(&copy.join("sub/deep")), 0o500);
    assert_eq!(mode(&copy.join("sub")), 0o750);
    let after = meta(&copy.join("a.txt")).unwrap();
    assert_eq!(
        (after.mtime_s, after.mtime_ns),
        (before.mtime_s, before.mtime_ns)
    );
}

#[test]
fn a_copy_that_fails_leaves_the_original_and_no_copy() {
    let dir = Scratch::new();
    let tree = dir.path("tree");
    fs::create_dir(&tree).unwrap();
    fs::write(tree.join("good"), "ok").unwrap();
    // A pipe cannot be copied.
    let status = std::process::Command::new("mkfifo")
        .arg(tree.join("pipe"))
        .status();
    if !status.is_ok_and(|s| s.success()) {
        return;
    }
    let error = move_by_copy(&tree, &dir.path("copy"), NEVER).unwrap_err();
    assert!(matches!(error, MoveError::Failed(_)), "{error:?}");
    assert!(tree.join("good").exists() && tree.join("pipe").exists());
    assert!(!dir.path("copy").exists(), "the half-made copy is removed");
}

#[test]
fn ctrl_c_during_a_copy_leaves_the_original() {
    let dir = Scratch::new();
    let tree = dir.path("tree");
    fs::create_dir(&tree).unwrap();
    for i in 0..5 {
        fs::write(tree.join(format!("f{i}")), "x").unwrap();
    }
    let calls = std::cell::Cell::new(0);
    let cancel = || {
        calls.set(calls.get() + 1);
        calls.get() > 3
    };
    let error = move_by_copy(&tree, &dir.path("copy"), &cancel).unwrap_err();
    assert!(matches!(&error, MoveError::Failed(e) if e.kind() == std::io::ErrorKind::Interrupted));
    assert_eq!(fs::read_dir(&tree).unwrap().count(), 5);
    assert!(!dir.path("copy").exists());
}

#[test]
fn a_folder_that_cannot_be_removed_is_not_copied_away() {
    let dir = Scratch::new();
    fs::create_dir_all(dir.path("locked/inner")).unwrap();
    fs::write(dir.path("locked/inner/f"), "x").unwrap();
    // Nobody may write to the parent: the original could not be removed after the copy.
    fs::set_permissions(dir.path("locked"), fs::Permissions::from_mode(0o500)).unwrap();
    if nix::unistd::geteuid().is_root() {
        fs::set_permissions(dir.path("locked"), fs::Permissions::from_mode(0o700)).unwrap();
        return;
    }
    let error = move_by_copy(&dir.path("locked/inner"), &dir.path("copy"), NEVER).unwrap_err();
    assert!(matches!(error, MoveError::Failed(_)), "{error:?}");
    assert!(dir.path("locked/inner/f").exists());
    assert!(!dir.path("copy").exists());
    fs::set_permissions(dir.path("locked"), fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn restore_puts_back_mode_and_time_and_makes_missing_folders() {
    let dir = Scratch::new();
    let original = dir.path("up/here/file");
    fs::create_dir_all(original.parent().unwrap()).unwrap();
    fs::write(&original, "data").unwrap();
    fs::set_permissions(&original, fs::Permissions::from_mode(0o640)).unwrap();
    let recorded = meta(&original).unwrap();
    assert_eq!(recorded.kind, Kind::File);
    fs::create_dir(dir.path("bin")).unwrap();
    move_to(&original, &dir.path("bin/file"), NEVER).unwrap();
    // The trash may have changed the mode on its way (a copy through a umask, say).
    fs::set_permissions(dir.path("bin/file"), fs::Permissions::from_mode(0o666)).unwrap();
    fs::remove_dir_all(dir.path("up")).unwrap();
    let notes = restore(&dir.path("bin/file"), &original, &recorded, NEVER).unwrap();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert_eq!(mode(&original), 0o640);
    assert_eq!(fs::read_to_string(&original).unwrap(), "data");
    assert_eq!(meta(&original).unwrap().mtime_s, recorded.mtime_s);
}

#[test]
fn restore_refuses_when_something_is_at_the_path() {
    let dir = Scratch::new();
    fs::write(dir.path("a"), "new").unwrap();
    fs::write(dir.path("t"), "old").unwrap();
    let recorded = meta(&dir.path("t")).unwrap();
    let error = restore(&dir.path("t"), &dir.path("a"), &recorded, NEVER).unwrap_err();
    assert!(matches!(error, RestoreError::Exists));
    assert_eq!(fs::read_to_string(dir.path("a")).unwrap(), "new");
    assert!(
        dir.path("t").exists(),
        "the trashed file stays in the trash"
    );
}

#[test]
fn restore_of_a_folder_keeps_its_own_mode() {
    let dir = Scratch::new();
    fs::create_dir_all(dir.path("d/inner")).unwrap();
    fs::write(dir.path("d/inner/f"), "x").unwrap();
    fs::set_permissions(dir.path("d"), fs::Permissions::from_mode(0o750)).unwrap();
    let recorded = meta(&dir.path("d")).unwrap();
    move_by_copy(&dir.path("d"), &dir.path("trashed"), NEVER).unwrap();
    restore(&dir.path("trashed"), &dir.path("d"), &recorded, NEVER).unwrap();
    assert_eq!(mode(&dir.path("d")), 0o750);
    assert!(dir.path("d/inner/f").exists());
}

#[test]
fn remove_tree_removes_folders_without_permissions() {
    let dir = Scratch::new();
    fs::create_dir_all(dir.path("a/b")).unwrap();
    fs::write(dir.path("a/b/f"), "x").unwrap();
    fs::set_permissions(dir.path("a/b"), fs::Permissions::from_mode(0o000)).unwrap();
    fs::set_permissions(dir.path("a"), fs::Permissions::from_mode(0o500)).unwrap();
    remove_tree(&dir.path("a")).unwrap();
    assert!(!dir.path("a").exists());
    remove_tree(&dir.path("a")).unwrap();
}

#[test]
fn meta_refuses_a_pipe() {
    let dir = Scratch::new();
    let status = std::process::Command::new("mkfifo")
        .arg(dir.path("p"))
        .status();
    if status.is_ok_and(|s| s.success()) {
        assert!(meta(&dir.path("p")).is_err());
    }
}

#[test]
fn old_folders_and_claims() {
    let dir = Scratch::new();
    for name in ["100-1", "200-2", "900-3", ".purging-50-0", "notes", "x-1"] {
        fs::create_dir(dir.path(name)).unwrap();
    }
    let (old, leftovers) = old_folders(&dir.0, 500);
    let names = |paths: &[PathBuf]| -> Vec<String> {
        paths
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    };
    assert_eq!(names(&old), ["100-1", "200-2"]);
    assert_eq!(names(&leftovers), [".purging-50-0"]);
    // Two shells claim the same folder: one wins.
    let first = claim_for_purge(&old[0]);
    let second = claim_for_purge(&old[0]);
    assert!(first.is_some());
    assert!(second.is_none());
    assert!(dir.path(".purging-100-1").exists());
}

#[test]
fn a_copy_that_differs_is_not_accepted() {
    use super::verify_tree;
    let dir = Scratch::new();
    let make = |name: &str| {
        let root = dir.path(name);
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("a"), "same").unwrap();
        fs::write(root.join("sub/b"), "inner").unwrap();
        symlink("a", root.join("l")).unwrap();
        root
    };
    let original = make("original");
    let good = make("good");
    verify_tree(&original, &good).unwrap();
    // One byte of content.
    let content = make("content");
    fs::write(content.join("sub/b"), "inneR").unwrap();
    assert!(verify_tree(&original, &content).is_err());
    // A different length.
    let longer = make("longer");
    fs::write(longer.join("a"), "same!").unwrap();
    assert!(verify_tree(&original, &longer).is_err());
    // A missing, and an extra, name.
    let missing = make("missing");
    fs::remove_file(missing.join("sub/b")).unwrap();
    assert!(verify_tree(&original, &missing).is_err());
    let extra = make("extra");
    fs::write(extra.join("more"), "x").unwrap();
    assert!(verify_tree(&original, &extra).is_err());
    // A link that points elsewhere, and a file where a link was.
    let link = make("link");
    fs::remove_file(link.join("l")).unwrap();
    symlink("elsewhere", link.join("l")).unwrap();
    assert!(verify_tree(&original, &link).is_err());
    let kind = make("kind");
    fs::remove_file(kind.join("l")).unwrap();
    fs::write(kind.join("l"), "a").unwrap();
    assert!(verify_tree(&original, &kind).is_err());
}
