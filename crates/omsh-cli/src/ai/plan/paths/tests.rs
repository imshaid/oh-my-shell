use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use omsh_safety::{Context, FsProbe, analyze};
use omsh_syntax::parse;

use super::*;

/// A folder for the made-up files of one test, removed at the end.
struct Dir(PathBuf);

impl Dir {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("omsh-paths-{}-{n}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        // Links in the temp folder (`/tmp` may be one) must not confuse the `..` rule.
        Self(fs::canonicalize(path).unwrap())
    }
    fn dir(&self, name: &str) -> PathBuf {
        let path = self.0.join(name);
        fs::create_dir_all(&path).unwrap();
        path
    }
    fn file(&self, name: &str) {
        let path = self.0.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "x").unwrap();
    }
    fn text(&self) -> &str {
        self.0.to_str().unwrap()
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn opens(line: &str, cwd: &Path) -> Vec<Opened> {
    let list = parse(line).unwrap();
    let ctx = Context {
        cwd: Some(cwd),
        home: Some(Path::new("/home/me")),
        probe: &FsProbe,
    };
    analyze(&list, line, &ctx).opens
}

/// The notes of the steps `lines`, each analyzed from `cwd`.
fn notes_of(lines: &[&str], cwd: &Path) -> Vec<Vec<Note>> {
    notes_with(lines, cwd, false)
}

fn notes_with(lines: &[&str], cwd: &Path, cdpath: bool) -> Vec<Vec<Note>> {
    let found: Vec<Vec<Opened>> = lines.iter().map(|line| opens(line, cwd)).collect();
    let steps: Vec<StepPaths<'_>> = lines
        .iter()
        .zip(&found)
        .map(|(command, opens)| StepPaths { command, opens })
        .collect();
    notes(&steps, cdpath)
}

fn only(notes: Vec<Vec<Note>>) -> Note {
    let mut all: Vec<Note> = notes.into_iter().flatten().collect();
    assert_eq!(all.len(), 1, "{all:?}");
    all.remove(0)
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|name| (*name).to_owned()).collect()
}

// -- where a path goes wrong ------------------------------------------------------------------

#[test]
fn the_first_part_that_is_missing_is_found() {
    let dir = Dir::new();
    dir.dir("a/b");
    let base = dir.text();
    assert_eq!(first_missing(&format!("{base}/a/b")), None);
    let missing = first_missing(&format!("{base}/a/zz/c/d")).unwrap();
    assert_eq!(missing.parent, dir.0.join("a"));
    assert_eq!(missing.name, "zz");
    assert_eq!(missing.rest, ["c", "d"]);
    let missing = first_missing(&format!("{base}/nope")).unwrap();
    assert_eq!(missing.parent, dir.0);
    assert!(missing.rest.is_empty());
}

#[test]
fn a_broken_link_is_there_and_a_file_in_the_middle_is_not_a_missing_part() {
    let dir = Dir::new();
    symlink("nowhere", dir.0.join("dangling")).unwrap();
    dir.file("plain");
    let base = dir.text();
    assert_eq!(first_missing(&format!("{base}/dangling")), None);
    // `plain/x`: the file system says "not a directory", which is not "no such file".
    assert_eq!(first_missing(&format!("{base}/plain/x")), None);
}

#[test]
fn a_folder_that_cannot_be_searched_gives_no_note() {
    let dir = Dir::new();
    let locked = dir.dir("locked");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let denied =
        fs::symlink_metadata(locked.join("x")).is_err_and(|e| e.kind() != ErrorKind::NotFound);
    let result = first_missing(&format!("{}/locked/x", dir.text()));
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    if denied {
        assert_eq!(result, None, "permission denied is not 'no such file'");
    }
}

// -- the real case ----------------------------------------------------------------------------

#[test]
fn the_folder_that_was_meant_is_offered() {
    let dir = Dir::new();
    dir.dir("night-before");
    let work = dir.dir("work");
    let (shown, near) = only(notes_of(&["code ../night-before-codebase"], &work));
    assert_eq!(shown, "../night-before-codebase");
    assert_eq!(near, ["../night-before"]);
}

#[test]
fn a_path_that_exists_gets_no_note_and_neither_does_a_missing_one_with_nothing_near() {
    let dir = Dir::new();
    dir.dir("here");
    let work = dir.dir("work");
    assert!(
        notes_of(&["code ../here", "cd ..", "ls ../work"], &work)
            .iter()
            .all(Vec::is_empty)
    );
    let (shown, near) = only(notes_of(&["cat zzzzzz"], &work));
    assert_eq!(shown, "zzzzzz");
    assert!(near.is_empty(), "{near:?}");
}

#[test]
fn only_the_listed_commands_are_noted() {
    let dir = Dir::new();
    let work = dir.dir("work");
    for line in [
        "mkdir nothing",
        "touch nothing",
        "cp a nothing",
        "mv a nothing",
        "echo hi > nothing/x",
        "tee nothing",
        "vim nothing",
    ] {
        let all = notes_of(&[line], &work);
        assert!(all.iter().all(Vec::is_empty), "{line}: {all:?}");
    }
    for line in [
        "cd nothing",
        "ls nothing",
        "cat nothing",
        "less nothing",
        "head nothing",
        "tail nothing",
        "code nothing",
        "xdg-open nothing",
    ] {
        let all = notes_of(&[line], &work);
        assert_eq!(all.concat().len(), 1, "{line}: {all:?}");
    }
}

#[test]
fn a_missing_middle_part_is_replaced_only_if_the_rest_exists_under_the_candidate() {
    let dir = Dir::new();
    dir.dir("projects/src");
    dir.dir("projekt/other");
    let (_, near) = only(notes_of(&["ls projekts/src"], &dir.0));
    assert_eq!(near, ["projects/src"], "projekt has no src");
}

#[test]
fn cd_is_offered_folders_only() {
    let dir = Dir::new();
    dir.file("notes.md");
    dir.dir("notes-old");
    let (_, near) = only(notes_of(&["cd notes"], &dir.0));
    assert_eq!(near, ["notes-old"]);
    let (_, near) = only(notes_of(&["cat notes"], &dir.0));
    assert_eq!(near, ["notes.md", "notes-old"], "a shorter tail is closer");
}

#[test]
fn a_hidden_name_is_offered_only_for_a_hidden_name() {
    let dir = Dir::new();
    dir.dir(".config");
    let (_, near) = only(notes_of(&["ls config"], &dir.0));
    assert!(near.is_empty(), "{near:?}");
    let (_, near) = only(notes_of(&["ls .confg"], &dir.0));
    assert_eq!(near, [".config"]);
}

#[test]
fn a_link_to_a_folder_counts_as_a_folder_for_cd() {
    let dir = Dir::new();
    let real = dir.dir("real-target");
    symlink(&real, dir.0.join("shortcut")).unwrap();
    let (_, near) = only(notes_of(&["cd shortcu"], &dir.0));
    assert_eq!(near, ["shortcut"]);
}

#[test]
fn a_name_with_a_blank_or_a_quote_is_quoted_and_a_tilde_stays_outside() {
    let dir = Dir::new();
    dir.dir("my notes");
    dir.dir("it's");
    let (_, near) = only(notes_of(&["ls my-notes"], &dir.0));
    assert_eq!(near, ["'my notes'"]);
    let (_, near) = only(notes_of(&["ls its"], &dir.0));
    assert_eq!(near, ["'it'\\''s'"]);
    assert_eq!(quoted("~/my notes"), "~/'my notes'");
    assert_eq!(quoted("~/plain"), "~/plain");
    assert_eq!(quoted("~"), "~");
    assert_eq!(quoted("a b"), "'a b'");
    assert_eq!(quoted("../x-y_z.1"), "../x-y_z.1");
}

#[test]
fn a_name_with_a_control_character_is_never_offered() {
    let dir = Dir::new();
    fs::create_dir_all(dir.0.join("projec\u{1b}t")).unwrap();
    dir.dir("projecs");
    let (_, near) = only(notes_of(&["ls project"], &dir.0));
    assert_eq!(near, ["projecs"]);
}

// -- false alarms -----------------------------------------------------------------------------

#[test]
fn a_name_that_an_earlier_step_or_the_same_step_mentions_is_not_noted() {
    let dir = Dir::new();
    let work = dir.dir("work");
    for steps in [
        &["git clone https://example.org/repo.git", "cd repo"][..],
        &["mkdir build", "cd build"],
        &["mkdir -p build && cd build"],
        &["tar xf x.tgz", "cd x"],
        &["cargo new foo; code foo"],
        &["[ -d foo ] || mkdir foo; ls foo"],
    ] {
        let all = notes_of(steps, &work);
        assert!(all.iter().all(Vec::is_empty), "{steps:?}: {all:?}");
    }
    // Mentioned only later is not enough: the path is wrong when the step starts.
    let all = notes_of(&["cd foo", "mkdir foo"], &work);
    assert_eq!(all.concat().len(), 1, "{all:?}");
}

#[test]
fn a_place_is_noted_once_because_the_name_is_then_mentioned() {
    let dir = Dir::new();
    let work = dir.dir("work");
    dir.dir("night-before");
    // The words after the `cd` are under the same missing folder; they do not say it again.
    let all = notes_of(&["cd ../night-before-codebase && ls src && code ."], &work);
    assert_eq!(all[0].len(), 1, "{all:?}");
    // The same word twice in one step, or in two steps, is one note too.
    let all = notes_of(&["cat zzz zzz"], &work);
    assert_eq!(all.concat().len(), 1);
    let all = notes_of(&["cat zzz", "ls ./zzz"], &work);
    assert_eq!(all.concat().len(), 1);
}

#[test]
fn cdpath_may_find_a_bare_cd_name() {
    let dir = Dir::new();
    let all = notes_with(&["cd nothing"], &dir.0, true);
    assert!(all.concat().is_empty(), "{all:?}");
    // A name that says where it is, and other commands, are still looked at.
    let all = notes_with(&["cd ./nothing", "ls nothing2"], &dir.0, true);
    assert_eq!(all.concat().len(), 2, "{all:?}");
}

#[test]
fn dotdot_from_a_working_directory_that_is_a_link_is_not_judged() {
    let dir = Dir::new();
    let real = dir.dir("real/deep");
    symlink(&real, dir.0.join("link")).unwrap();
    let link = dir.0.join("link");
    // Lexically `..` is the folder holding `link`; for the file system it is `real`.
    let all = notes_of(&["ls ../elsewhere"], &link);
    assert!(all.concat().is_empty(), "{all:?}");
    // Without `..` the same link is fine.
    let all = notes_of(&["ls elsewhere"], &link);
    assert_eq!(all.concat().len(), 1, "{all:?}");
}

#[test]
fn a_step_gets_at_most_three_notes() {
    let dir = Dir::new();
    let all = notes_of(&["ls a1 b2 c3 d4 e5"], &dir.0);
    assert_eq!(all[0].len(), MAX_NOTES, "{all:?}");
}

#[test]
fn a_folder_with_too_many_entries_gives_a_note_with_no_suggestion() {
    let dir = Dir::new();
    let big = dir.dir("big");
    for i in 0..=MAX_ENTRIES {
        fs::write(big.join(format!("f{i}")), "").unwrap();
    }
    fs::create_dir_all(big.join("night-before")).unwrap();
    let (_, near) = only(notes_of(&["ls big/night-before-codebase"], &dir.0));
    assert!(near.is_empty(), "{near:?}");
    assert!(read_names(&big).is_none());
}

#[test]
fn the_tilde_is_kept_in_the_suggestion_when_the_home_folder_is_known() {
    let dir = Dir::new();
    dir.dir("night-before");
    // Home is made up (`/home/me`); build the word by hand against this folder as home.
    let list = parse("code ~/night-before-codebase").unwrap();
    let ctx = Context {
        cwd: Some(dir.0.as_path()),
        home: Some(dir.0.as_path()),
        probe: &FsProbe,
    };
    let line = "code ~/night-before-codebase";
    let found = analyze(&list, line, &ctx).opens;
    let steps = [StepPaths {
        command: line,
        opens: &found,
    }];
    let all = notes(&steps, false);
    assert_eq!(
        all[0],
        [(
            "~/night-before-codebase".to_owned(),
            names(&["~/night-before"])
        )]
    );
}

// -- ranking ----------------------------------------------------------------------------------

#[test]
fn the_tiers_are_the_same_name_then_few_edits_then_a_tail() {
    let list = names(&[
        "night-before-codebase-2",
        "night-before",
        "nightbefore",
        "Night_Before_Codebase",
        "night-bifore-codebase",
        "unrelated",
    ]);
    // Typed with a slip; the same name apart from separators and capitals comes first.
    assert_eq!(
        ranked("night-before-codebase", &list),
        [
            "Night_Before_Codebase",
            "night-bifore-codebase",
            "night-before-codebase-2",
            "night-before"
        ]
    );
}

#[test]
fn separators_and_capitals_alone_are_the_best_match() {
    assert_eq!(
        ranked("nightbefore", &names(&["night-before", "nightbeforf"])),
        ["night-before", "nightbeforf"]
    );
    assert_eq!(ranked("projects", &names(&["Projects"])), ["Projects"]);
}

#[test]
fn a_tail_needs_a_boundary_and_a_name_of_three_characters() {
    assert_eq!(
        ranked("notes", &names(&["notes.md", "notesabcd"])),
        ["notes.md"]
    );
    assert_eq!(
        ranked("ab", &names(&["ab-cd", "ab.txt"])),
        Vec::<String>::new()
    );
    assert_eq!(
        ranked("night-before-codebase", &names(&["night"])),
        ["night"]
    );
    assert_eq!(
        ranked("night-before-codebase", &names(&["ni"])),
        Vec::<String>::new()
    );
}

#[test]
fn closer_comes_first_within_a_tier_and_then_name_order() {
    assert_eq!(
        ranked(
            "readme",
            &names(&["readmes", "readm", "reedmx", "readme-b", "readme-a"])
        ),
        ["readm", "readmes", "readme-a", "readme-b", "reedmx"]
    );
}

#[test]
fn the_name_itself_is_never_offered() {
    assert_eq!(ranked("abc", &names(&["abc", "abd"])), ["abd"]);
}
