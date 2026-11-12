use std::fs::{self, File};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use super::*;

fn chars(text: &str) -> Vec<char> {
    text.chars().collect()
}

fn dist(a: &str, b: &str, limit: usize) -> Option<usize> {
    distance(&chars(a), &chars(b), limit)
}

#[test]
fn distances_count_insertions_deletions_replacements_and_swaps() {
    assert_eq!(dist("git", "git", 3), Some(0));
    assert_eq!(dist("gti", "git", 3), Some(1), "a swap is one step");
    assert_eq!(dist("gt", "git", 3), Some(1), "an insertion");
    assert_eq!(dist("gitt", "git", 3), Some(1), "a deletion");
    assert_eq!(dist("gat", "git", 3), Some(1), "a replacement");
    assert_eq!(dist("kitten", "sitting", 3), Some(3));
    assert_eq!(dist("", "abc", 3), Some(3));
    assert_eq!(dist("abc", "", 3), Some(3));
    // Optimal string alignment never edits the same piece twice, so this is 3 and not 2.
    assert_eq!(dist("ca", "abc", 3), Some(3));
    assert_eq!(dist("ab", "ba", 3), Some(1));
    assert_eq!(dist("abcd", "badc", 3), Some(2), "two swaps");
}

#[test]
fn a_distance_over_the_limit_is_not_reported() {
    assert_eq!(dist("kitten", "sitting", 2), None);
    assert_eq!(dist("a", "abcd", 2), None, "the length alone rules it out");
    assert_eq!(dist("abc", "abd", 0), None);
    assert_eq!(dist("abc", "abc", 0), Some(0));
}

#[test]
fn letters_other_than_ascii_count_as_one_character() {
    assert_eq!(dist("café", "cafe", 2), Some(1));
    assert_eq!(dist("বাংলা", "বাংলো", 2), Some(1));
}

#[test]
fn the_allowed_edits_depend_on_the_length() {
    assert_eq!(allowed_edits(""), None);
    assert_eq!(allowed_edits("g"), None);
    assert_eq!(allowed_edits("gt"), Some(1));
    assert_eq!(allowed_edits("gtii"), Some(1));
    assert_eq!(allowed_edits("gtiit"), Some(2));
    assert_eq!(allowed_edits("é"), None, "characters, not bytes");
    assert_eq!(allowed_edits("éé"), Some(1));
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("omsh-suggest-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    /// A program (executable) in the directory.
    fn program(&self, name: &str) {
        let path = self.0.join(name);
        File::create(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// A file that is not executable.
    fn plain(&self, name: &str) {
        File::create(self.0.join(name)).unwrap();
    }

    fn path(&self) -> Vec<u8> {
        self.0.to_string_lossy().into_owned().into_bytes()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn suggest(name: &str, s: &Scratch, shell: &[&str]) -> Vec<String> {
    let shell: Vec<String> = shell.iter().map(|n| (*n).to_owned()).collect();
    suggest_command(name, &s.path(), &shell)
}

#[test]
fn a_swapped_name_finds_the_program() {
    let s = Scratch::new("swap");
    s.program("git");
    s.program("ls");
    s.program("grep");
    assert_eq!(suggest("gti", &s, &[]), ["git"]);
    assert_eq!(suggest("sl", &s, &[]), ["ls"]);
}

#[test]
fn builtins_functions_and_aliases_are_candidates() {
    let s = Scratch::new("shell");
    s.program("git");
    assert_eq!(suggest("ecoh", &s, &["echo", "cd"]), ["echo"]);
    assert_eq!(suggest("greeet", &s, &["greet"]), ["greet"]);
}

#[test]
fn only_the_closest_names_are_kept_closest_first_and_at_most_three() {
    let s = Scratch::new("closest");
    for name in ["gist", "gitk", "gitg", "git-a", "gits"] {
        s.program(name);
    }
    // `gti`: one swap away is `git` (absent); `gits` and `gist` are two edits, too far for a
    // 3-letter name. With `git` present only it is offered.
    s.program("git");
    assert_eq!(suggest("gti", &s, &[]), ["git"]);

    // Several at the same distance: sorted, three at most.
    let s = Scratch::new("many");
    for name in ["bat", "cat", "eat", "fat", "hat"] {
        s.program(name);
    }
    assert_eq!(suggest("xat", &s, &[]), ["bat", "cat", "eat"]);
}

#[test]
fn a_nearer_name_hides_the_farther_ones() {
    let s = Scratch::new("nearer");
    s.program("python");
    s.program("pythom3");
    s.program("pyton");
    // `pythno` is one swap from `python`; `pyton` is two edits away and is left out.
    assert_eq!(suggest("pythno", &s, &[]), ["python"]);
}

#[test]
fn a_file_that_would_not_run_is_not_offered() {
    let s = Scratch::new("plain");
    s.plain("git");
    s.program("gist");
    // `git` is not a program here; `gist` is two edits from `gti`, over the limit for 3 letters.
    assert_eq!(suggest("gti", &s, &[]), Vec::<String>::new());
    // A non-program of the same distance is skipped for the next one.
    let s = Scratch::new("plain2");
    s.plain("bat");
    s.program("cat");
    assert_eq!(suggest("xat", &s, &[]), ["cat"]);
}

#[test]
fn a_name_in_two_places_is_offered_once() {
    let s = Scratch::new("twice");
    s.program("echo");
    assert_eq!(suggest("ecoh", &s, &["echo"]), ["echo"]);
}

#[test]
fn capitals_do_not_count_as_a_difference_but_the_same_name_is_not_a_suggestion() {
    let s = Scratch::new("caps");
    s.program("ls");
    assert_eq!(suggest("LS", &s, &[]), ["ls"]);
    assert_eq!(suggest("Ls", &s, &["ls"]), ["ls"]);
    // The name itself, if it is a candidate, is never its own suggestion.
    assert_eq!(suggest("ls", &s, &[]), Vec::<String>::new());
}

#[test]
fn short_or_far_names_get_nothing() {
    let s = Scratch::new("far");
    s.program("a");
    s.program("git");
    assert_eq!(suggest("x", &s, &[]), Vec::<String>::new());
    assert_eq!(suggest("", &s, &[]), Vec::<String>::new());
    assert_eq!(suggest("zzzz", &s, &[]), Vec::<String>::new());
    // Three edits from `git` is too far for a short name.
    assert_eq!(suggest("xyz", &s, &[]), Vec::<String>::new());
}

#[test]
fn a_longer_name_may_be_two_edits_away() {
    let s = Scratch::new("long");
    s.program("firefox");
    assert_eq!(suggest("fierfox", &s, &[]), ["firefox"]);
    assert_eq!(suggest("fiirefxo", &s, &[]), ["firefox"]);
    assert_eq!(suggest("wirefax", &s, &[]), ["firefox"], "two replacements");
    assert_eq!(
        suggest("wirefaz", &s, &[]),
        Vec::<String>::new(),
        "three replacements"
    );
}

#[test]
fn a_name_with_blanks_or_control_characters_is_not_a_typo() {
    let s = Scratch::new("odd");
    s.program("git");
    assert_eq!(suggest("gi t", &s, &[]), Vec::<String>::new());
    assert_eq!(suggest("gi\x1bt", &s, &[]), Vec::<String>::new());
}

#[test]
fn the_names_of_a_directory_with_control_characters_are_never_offered() {
    let s = Scratch::new("ctl");
    s.program("g\x07t");
    s.program("git");
    assert_eq!(suggest("gti", &s, &[]), ["git"]);
}

#[test]
fn an_empty_or_missing_path_still_offers_the_shells_names() {
    assert_eq!(suggest_command("ecoh", b"", &["echo".to_owned()]), ["echo"]);
    assert_eq!(
        suggest_command("ecoh", b"/no/such/dir-omsh", &["echo".to_owned()]),
        ["echo"]
    );
}

#[test]
fn among_equally_close_names_the_shells_own_come_first() {
    let s = Scratch::new("own-first");
    for name in ["chg", "who"] {
        s.program(name);
    }
    // `cho` is one edit from all three; `echo` is a builtin, so it leads, then the programs in
    // alphabetical order.
    assert_eq!(suggest("cho", &s, &["echo"]), ["echo", "chg", "who"]);
    // A nearer program still beats a farther builtin.
    s.program("cha");
    assert_eq!(suggest("chaa", &s, &["echoo"]), ["cha"]);
}
