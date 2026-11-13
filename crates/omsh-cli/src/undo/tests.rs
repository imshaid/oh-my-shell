use std::collections::VecDeque;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use omsh_exec::{JournalCommand, ReadError, Terminal};
use omsh_safety::journal::Origin;

use super::{Undo, parse_rm, purge_now, purge_old};

struct Dir(PathBuf);

impl Dir {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("omsh-undo-{}-{n}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    fn undo(&self) -> Arc<Undo> {
        Undo::new(self.path("data"))
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = omsh_safety::trash::remove_tree(&self.0);
    }
}

#[derive(Default)]
struct Tty {
    answers: VecDeque<&'static str>,
    out: String,
    err: String,
    prompts: Vec<String>,
}

impl Tty {
    fn answering(answers: &[&'static str]) -> Self {
        Self {
            answers: answers.iter().copied().collect(),
            ..Self::default()
        }
    }
}

impl Terminal for Tty {
    fn input_is_terminal(&self) -> bool {
        true
    }
    fn out(&mut self, text: &str) {
        self.out.push_str(text);
    }
    fn err(&mut self, text: &str) {
        self.err.push_str(text);
    }
    fn read_line(&mut self, prompt: &str, _: bool, line: &mut Vec<u8>) -> Result<(), ReadError> {
        self.prompts.push(prompt.to_owned());
        line.clear();
        match self.answers.pop_front() {
            Some(text) => {
                line.extend_from_slice(text.as_bytes());
                Ok(())
            }
            None => Err(ReadError::EndOfInput),
        }
    }
    fn interrupted(&self) -> bool {
        false
    }
}

fn words(list: &[&str]) -> Vec<Vec<u8>> {
    list.iter().map(|w| w.as_bytes().to_vec()).collect()
}

fn trash(undo: &Undo, cwd: &Path, args: &[&str], tty: &mut Tty) -> u8 {
    undo.trash(&words(args), cwd.as_os_str().as_encoded_bytes(), tty)
}

#[test]
fn rm_options_are_read_like_rm_reads_them() {
    let parsed = parse_rm(&words(&["-rf", "a", "--", "-b", "--verbose"])).unwrap();
    assert!(parsed.recursive && parsed.force && !parsed.verbose && !parsed.dir);
    assert_eq!(parsed.operands, words(&["a", "-b", "--verbose"]));
    let parsed = parse_rm(&words(&["-R", "-d", "-v", "--preserve-root", "-"])).unwrap();
    assert!(parsed.recursive && parsed.dir && parsed.verbose);
    assert_eq!(parsed.operands, words(&["-"]));
    assert!(parse_rm(&words(&["-i", "a"])).unwrap_err().contains("-i"));
    assert!(parse_rm(&words(&["--interactive"])).is_err());
}

#[test]
fn a_file_goes_to_the_trash_and_the_line_is_one_action() {
    let dir = Dir::new();
    let undo = dir.undo();
    fs::write(dir.path("a"), "a").unwrap();
    fs::create_dir(dir.path("b")).unwrap();
    fs::write(dir.path("b/f"), "f").unwrap();
    undo.begin_line("rm a; rm -r b", Origin::Typed);
    let mut tty = Tty::default();
    assert_eq!(trash(&undo, &dir.0, &["a"], &mut tty), 0);
    assert_eq!(trash(&undo, &dir.0, &["-r", "b"], &mut tty), 0);
    let summary = undo.end_line().unwrap();
    assert_eq!(summary.items, 2);
    assert_eq!(summary.id, 1);
    assert_eq!(
        summary.line(),
        "oh-my-shell: moved 2 items to the trash; \"undo\" brings them back (#1)"
    );
    assert!(!dir.path("a").exists() && !dir.path("b").exists());
    assert!(tty.err.is_empty() && tty.out.is_empty());
    assert_eq!(undo.journal().unwrap().list(10).len(), 1);
}

#[test]
fn it_mirrors_what_rm_says_and_returns() {
    let dir = Dir::new();
    let undo = dir.undo();
    fs::create_dir(dir.path("d")).unwrap();
    fs::write(dir.path("d/f"), "f").unwrap();
    fs::create_dir(dir.path("empty")).unwrap();
    let mut tty = Tty::default();
    // A folder without -r.
    assert_eq!(trash(&undo, &dir.0, &["d"], &mut tty), 1);
    assert!(
        tty.err.contains("cannot remove 'd': Is a directory"),
        "{}",
        tty.err
    );
    assert!(dir.path("d/f").exists());
    // -d: only an empty one.
    let mut tty = Tty::default();
    assert_eq!(trash(&undo, &dir.0, &["-d", "d", "empty"], &mut tty), 1);
    assert!(tty.err.contains("'d': Directory not empty"), "{}", tty.err);
    assert!(!dir.path("empty").exists() && dir.path("d").exists());
    // A missing file: an error, or nothing with -f.
    let mut tty = Tty::default();
    assert_eq!(trash(&undo, &dir.0, &["nope"], &mut tty), 1);
    assert!(tty.err.contains("'nope': No such file or directory"));
    let mut tty = Tty::default();
    assert_eq!(trash(&undo, &dir.0, &["-f", "nope"], &mut tty), 0);
    assert!(tty.err.is_empty());
    assert_eq!(trash(&undo, &dir.0, &["-f"], &mut tty), 0);
    assert_eq!(trash(&undo, &dir.0, &[], &mut tty), 1);
    assert!(tty.err.contains("missing operand"));
    // One bad name does not stop the others, and the status says so.
    fs::write(dir.path("x"), "x").unwrap();
    let mut tty = Tty::default();
    assert_eq!(trash(&undo, &dir.0, &["nope", "x"], &mut tty), 1);
    assert!(!dir.path("x").exists());
    // -v
    fs::write(dir.path("y"), "y").unwrap();
    let mut tty = Tty::default();
    assert_eq!(trash(&undo, &dir.0, &["-v", "y"], &mut tty), 0);
    assert_eq!(tty.out, "moved 'y' to the trash\n");
    // An option it does not do deletes nothing.
    fs::write(dir.path("z"), "z").unwrap();
    let mut tty = Tty::default();
    assert_eq!(trash(&undo, &dir.0, &["-i", "z"], &mut tty), 2);
    assert!(dir.path("z").exists());
}

#[test]
fn it_refuses_the_root_dot_dotdot_the_home_folder_and_system_folders() {
    let dir = Dir::new();
    let undo = dir.undo();
    fs::create_dir(dir.path("sub")).unwrap();
    for operand in ["/", ".", "..", "sub/..", "/etc", "/usr/", "/home"] {
        let mut tty = Tty::default();
        let status = trash(&undo, &dir.path("sub"), &["-rf", operand], &mut tty);
        assert_eq!(status, 1, "{operand}: {}", tty.err);
        assert!(tty.err.contains("cannot move"), "{operand}: {}", tty.err);
    }
    assert!(dir.path("sub").exists());
    assert!(Path::new("/etc").exists());
    assert!(undo.journal().unwrap().list(10).is_empty());
}

#[test]
fn nothing_is_deleted_when_the_journal_cannot_be_opened() {
    let dir = Dir::new();
    // A file where the data folder should be.
    fs::write(dir.path("data"), "in the way").unwrap();
    let undo = dir.undo();
    fs::write(dir.path("keep"), "k").unwrap();
    let mut tty = Tty::default();
    assert_eq!(trash(&undo, &dir.0, &["keep"], &mut tty), 1);
    assert!(
        tty.err.contains("the trash is not available") && tty.err.contains("nothing was deleted"),
        "{}",
        tty.err
    );
    assert!(dir.path("keep").exists());
    // `undo` says so too.
    let mut tty = Tty::default();
    assert_eq!(undo.undo(&[], b"/", &mut tty), 1);
    assert!(tty.err.contains("not available"), "{}", tty.err);
}

#[test]
fn a_secret_in_the_line_is_not_kept_in_the_journal() {
    let dir = Dir::new();
    let undo = dir.undo();
    fs::write(dir.path("a"), "a").unwrap();
    undo.begin_line(
        concat!("API_KEY=gh", "p_abcdefghijklmnopqrstuvwxyz0123456789 rm a"),
        Origin::Typed,
    );
    let mut tty = Tty::default();
    assert_eq!(trash(&undo, &dir.0, &["a"], &mut tty), 0);
    undo.end_line().unwrap();
    let command = undo.journal().unwrap().list(1)[0].command.clone();
    assert!(!command.contains("ghp_"), "{command}");
    assert!(command.contains("[MASKED]"), "{command}");
}

#[test]
fn undo_asks_then_restores_with_the_original_permissions() {
    let dir = Dir::new();
    let undo = dir.undo();
    fs::write(dir.path("secret"), "s").unwrap();
    fs::set_permissions(dir.path("secret"), fs::Permissions::from_mode(0o600)).unwrap();
    fs::create_dir(dir.path("tree")).unwrap();
    fs::set_permissions(dir.path("tree"), fs::Permissions::from_mode(0o750)).unwrap();
    undo.begin_line("rm -rf secret tree", Origin::Typed);
    let mut tty = Tty::default();
    trash(&undo, &dir.0, &["-rf", "secret", "tree"], &mut tty);
    undo.end_line().unwrap();

    // `n` changes nothing.
    let mut tty = Tty::answering(&["n"]);
    assert_eq!(undo.undo(&[], b"/", &mut tty), 1);
    assert!(tty.err.contains("undo #1"), "{}", tty.err);
    assert!(tty.err.contains("- bring back "), "{}", tty.err);
    assert_eq!(tty.prompts, ["Undo it? [y/N] "]);
    assert!(!dir.path("secret").exists());

    let mut tty = Tty::answering(&["y"]);
    assert_eq!(undo.undo(&[], b"/", &mut tty), 0, "{}{}", tty.out, tty.err);
    assert!(tty.out.contains("restored "), "{}", tty.out);
    let mode = |name: &str| fs::metadata(dir.path(name)).unwrap().permissions().mode() & 0o7777;
    assert_eq!(mode("secret"), 0o600);
    assert_eq!(mode("tree"), 0o750);

    // Nothing is left to undo.
    let mut tty = Tty::default();
    assert_eq!(undo.undo(&[], b"/", &mut tty), 1);
    assert!(tty.err.contains("nothing to undo"), "{}", tty.err);
}

#[test]
fn undo_by_number_lists_and_reports_bad_numbers() {
    let dir = Dir::new();
    let undo = dir.undo();
    for name in ["one", "two"] {
        fs::write(dir.path(name), name).unwrap();
        undo.begin_line(&format!("rm {name}"), Origin::Typed);
        trash(&undo, &dir.0, &[name], &mut Tty::default());
        undo.end_line().unwrap();
    }
    let mut tty = Tty::default();
    assert_eq!(undo.undo(&words(&["--list"]), b"/", &mut tty), 0);
    let lines: Vec<&str> = tty.out.lines().collect();
    assert_eq!(lines.len(), 2, "{}", tty.out);
    assert!(
        lines[0].starts_with("#2  ") && lines[0].ends_with("rm two"),
        "{}",
        lines[0]
    );
    assert!(lines[0].contains("typed  1 item  "), "{}", lines[0]);
    assert!(lines[1].starts_with("#1  "));
    // The older one first, by number.
    let mut tty = Tty::answering(&["yes"]);
    assert_eq!(undo.undo(&words(&["#1"]), b"/", &mut tty), 0);
    assert!(dir.path("one").exists() && !dir.path("two").exists());
    let mut tty = Tty::default();
    assert_eq!(undo.undo(&words(&["99"]), b"/", &mut tty), 1);
    assert!(tty.err.contains("no action #99"));
    assert_eq!(undo.undo(&words(&["x"]), b"/", &mut Tty::default()), 2);
    assert_eq!(undo.undo(&words(&["1", "2"]), b"/", &mut Tty::default()), 2);
    // Undone: listed with its state, and a second try says so.
    let mut tty = Tty::default();
    undo.undo(&words(&["-l"]), b"/", &mut tty);
    assert!(tty.out.contains("[undone]"), "{}", tty.out);
    let mut tty = Tty::default();
    assert_eq!(undo.undo(&words(&["1"]), b"/", &mut tty), 1);
    assert!(tty.err.contains("has been undone already"), "{}", tty.err);
}

#[test]
fn the_list_of_an_empty_journal_says_so() {
    let dir = Dir::new();
    let undo = dir.undo();
    let mut tty = Tty::default();
    assert_eq!(undo.undo(&words(&["--list"]), b"/", &mut tty), 0);
    assert_eq!(tty.out, "nothing has been recorded\n");
}

#[test]
fn undo_that_cannot_restore_a_file_says_which_and_exits_1() {
    let dir = Dir::new();
    let undo = dir.undo();
    fs::write(dir.path("a"), "old").unwrap();
    undo.begin_line("rm a", Origin::Typed);
    trash(&undo, &dir.0, &["a"], &mut Tty::default());
    undo.end_line().unwrap();
    fs::write(dir.path("a"), "new").unwrap();
    let mut tty = Tty::answering(&["y"]);
    assert_eq!(undo.undo(&[], b"/", &mut tty), 1);
    assert!(tty.out.contains("exists now"), "{}", tty.out);
    assert!(
        tty.err.contains("1 of 1 could not be undone"),
        "{}",
        tty.err
    );
    assert_eq!(fs::read_to_string(dir.path("a")).unwrap(), "new");
}

// -- retention --------------------------------------------------------------------------------

fn age(root: &Path, name: &str, days: i64) -> PathBuf {
    let old = omsh_safety::trash::now_secs() - days * 86_400;
    let folder = root.join(format!("{old}-{name}"));
    fs::create_dir_all(folder.join("1")).unwrap();
    fs::write(folder.join("1/file"), "x").unwrap();
    folder
}

#[test]
fn old_trash_is_announced_once_then_emptied() {
    let dir = Dir::new();
    let root = dir.path("data/trash");
    fs::create_dir_all(&root).unwrap();
    let old = age(&root, "1", 9);
    let fresh = age(&root, "2", 1);
    let mut said = Vec::new();
    // The work itself is joined through `purge_now`, so the test does not race a thread.
    let (olds, leftovers) =
        omsh_safety::trash::old_folders(&root, omsh_safety::trash::now_secs() - 8 * 86_400);
    assert_eq!((olds.len(), leftovers.len()), (1, 0));
    purge_old(&dir.path("data"), 8, &mut |text| said.push(text.to_owned()));
    assert_eq!(
        said,
        ["oh-my-shell: emptying 1 trash entry older than 8 days\n"]
    );
    // A second shell starting now finds nothing to claim: no second notice.
    let mut again = Vec::new();
    purge_old(&dir.path("data"), 8, &mut |text| {
        again.push(text.to_owned())
    });
    assert!(again.is_empty(), "{again:?}");
    // The thread removes the claimed folder; wait for it a little.
    for _ in 0..200 {
        if !root
            .join(format!(
                ".purging-{}",
                old.file_name().unwrap().to_string_lossy()
            ))
            .exists()
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(!old.exists());
    assert!(fresh.exists(), "a recent entry stays");
}

#[test]
fn leftovers_of_a_killed_shell_are_removed_without_a_word_and_the_rows_are_forgotten() {
    let dir = Dir::new();
    let undo = dir.undo();
    fs::write(dir.path("a"), "a").unwrap();
    undo.begin_line("rm a", Origin::Typed);
    trash(&undo, &dir.0, &["a"], &mut Tty::default());
    undo.end_line().unwrap();
    let root = dir.path("data/trash");
    let leftover = root.join(".purging-5-5");
    fs::create_dir_all(leftover.join("1")).unwrap();
    // Make the real action old enough, as the thread would find it.
    let action_dir = fs::read_dir(&root)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| !p.file_name().unwrap().to_string_lossy().starts_with('.'))
        .unwrap();
    let mut said = Vec::new();
    purge_old(&dir.path("data"), 8, &mut |text| said.push(text.to_owned()));
    assert!(said.is_empty(), "{said:?}");
    for _ in 0..200 {
        if !leftover.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(!leftover.exists());
    assert!(action_dir.exists(), "a recent action is not touched");
    // The rows of purged folders go with them.
    purge_now(&dir.path("data"), &[], Some(i64::MAX / 2000));
    assert!(undo.journal().unwrap().list(10).is_empty());
}

#[test]
fn purging_forgets_only_the_rows_older_than_the_cutoff() {
    let dir = Dir::new();
    let undo = dir.undo();
    fs::write(dir.path("a"), "a").unwrap();
    undo.begin_line("rm a", Origin::Typed);
    trash(&undo, &dir.0, &["a"], &mut Tty::default());
    undo.end_line().unwrap();
    // A cutoff in 1970: today's action is newer and stays.
    purge_now(&dir.path("data"), &[], Some(1));
    assert_eq!(undo.journal().unwrap().list(10).len(), 1);
}
