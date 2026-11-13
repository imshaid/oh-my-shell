//! Tests of `omsh uninstall` with a scripted person, a table for keys, and real files in a
//! scratch folder. A test with a real terminal is in `omsh-tests/tests/uninstall.rs`.

use std::collections::{HashMap, VecDeque};
use std::os::unix::fs::symlink;
use std::sync::{Arc, Mutex};

use omsh_ai::{KeyError, KeyStore, Secret};

use super::*;

// -- fakes --------------------------------------------------------------------------------

/// A person who answers with the given keys, in order.
struct Person {
    answers: VecDeque<Result<Key, ReadError>>,
    asked: Vec<String>,
    out: String,
    err: String,
    is_terminal: bool,
}

impl Person {
    fn says(keys: &str) -> Self {
        let answers = keys
            .chars()
            .map(|c| match c {
                '\n' => Ok(Key::Enter),
                '\u{1b}' => Ok(Key::Escape),
                '!' => Err(ReadError::Interrupted),
                c => Ok(Key::Char(c)),
            })
            .collect();
        Self {
            answers,
            asked: Vec::new(),
            out: String::new(),
            err: String::new(),
            is_terminal: true,
        }
    }
    fn all(&self) -> String {
        format!("{}{}", self.out, self.err)
    }
}

impl Terminal for Person {
    fn input_is_terminal(&self) -> bool {
        self.is_terminal
    }
    fn out(&mut self, text: &str) {
        self.out.push_str(text);
    }
    fn err(&mut self, text: &str) {
        self.err.push_str(text);
    }
    fn read_line(&mut self, _: &str, _: bool, _: &mut Vec<u8>) -> Result<(), ReadError> {
        panic!("only single keys are asked");
    }
    fn read_key(&mut self, prompt: &str) -> Result<Key, ReadError> {
        self.asked.push(prompt.to_owned());
        self.err.push_str(prompt);
        self.answers
            .pop_front()
            .expect("a question nobody answered")
    }
    fn interrupted(&self) -> bool {
        false
    }
}

/// Keys in a table; remembers what was deleted.
#[derive(Clone, Default)]
struct Table(Arc<Mutex<HashMap<&'static str, String>>>);

impl KeyStore for Table {
    fn place(&self) -> String {
        "the table".to_owned()
    }
    fn get(&self, p: Provider) -> Result<Option<Secret>, KeyError> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .get(p.as_str())
            .and_then(|k| Secret::new(k.clone())))
    }
    fn set(&self, p: Provider, key: &Secret) -> Result<(), KeyError> {
        self.0
            .lock()
            .unwrap()
            .insert(p.as_str(), key.expose().to_owned());
        Ok(())
    }
    fn delete(&self, p: Provider) -> Result<bool, KeyError> {
        Ok(self.0.lock().unwrap().remove(p.as_str()).is_some())
    }
}

struct Home(PathBuf);

impl Home {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("omsh-uninstall-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(fs::canonicalize(dir).unwrap())
    }
    fn p(&self, rel: &str) -> PathBuf {
        self.0.join(rel)
    }
    fn put(&self, rel: &str, text: &str) -> PathBuf {
        let path = self.p(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        path
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A home with an installed program, its link, and files of every kind.
fn world(name: &str) -> (Home, PathBuf, Table) {
    let home = Home::new(name);
    let exe = home.put(".local/bin/oh-my-shell", "program");
    symlink("oh-my-shell", home.p(".local/bin/omsh")).unwrap();
    home.put(".config/oh-my-shell/config.toml", "# c");
    home.put(".config/oh-my-shell/init.omsh", "alias a=b");
    home.put(".config/oh-my-shell/themes/x.toml", "t");
    home.put(".config/oh-my-shell/credentials", "openai=k");
    home.put(".local/share/oh-my-shell/history.db", "h");
    home.put(".local/share/oh-my-shell/history.db-wal", "h");
    home.put(".local/share/oh-my-shell/undo.db", "u");
    home.put(".local/share/oh-my-shell/trash/1/file", "gone");
    home.put(".local/state/oh-my-shell/update-check", "checked=1\n");
    let table = Table::default();
    table.0.lock().unwrap().insert("openai", "k".to_owned());
    (home, exe, table)
}

fn env(home: &Home, table: &Table) -> UninstallEnv {
    let root = home.0.clone();
    UninstallEnv {
        paths: Paths::from_lookup(|name| (name == "HOME").then(|| root.clone().into_os_string())),
        home: Some(home.0.clone()),
        cargo_home: None,
        login_shell: Box::new(|| Some(PathBuf::from("/bin/bash"))),
        etc_shells: home.p("etc-shells"),
        keys: Keys::with(
            Box::new(|| Err(KeyError::Unavailable("no keyring here".to_owned()))),
            Box::new(table.clone()),
        ),
        ai_set_up: false,
        as_tool: true,
    }
}

fn exists(path: PathBuf) -> bool {
    path.symlink_metadata().is_ok()
}

// -- refusals -----------------------------------------------------------------------------

#[test]
fn inside_the_shell_it_is_refused_and_nothing_is_asked() {
    let (home, exe, table) = world("inside");
    let mut env = env(&home, &table);
    env.as_tool = false;
    let mut person = Person::says("yyyyy");
    let status = run(&[], &mut person, &env, Ok(exe.clone()));
    assert_eq!(status, 1);
    assert!(person.err.contains("Exit this shell"), "{}", person.all());
    assert!(person.asked.is_empty());
    assert!(exists(exe));
}

#[test]
fn without_a_terminal_it_is_refused() {
    let (home, exe, table) = world("noterm");
    let mut person = Person::says("yyyyy");
    person.is_terminal = false;
    let status = run(&[], &mut person, &env(&home, &table), Ok(exe.clone()));
    assert_eq!(status, 1);
    assert!(person.err.contains("needs a terminal"), "{}", person.all());
    assert!(exists(exe));
}

#[test]
fn the_login_shell_is_refused_also_through_a_link() {
    let (home, exe, table) = world("login");
    for login in [exe.clone(), home.p(".local/bin/omsh")] {
        let mut env = env(&home, &table);
        env.login_shell = Box::new(move || Some(login.clone()));
        let mut person = Person::says("yyyyy");
        let status = run(&[], &mut person, &env, Ok(exe.clone()));
        assert_eq!(status, 1);
        assert!(person.err.contains("login shell"), "{}", person.all());
        assert!(person.err.contains("chsh"), "{}", person.all());
        assert!(person.asked.is_empty());
        assert!(exists(exe.clone()));
    }
}

#[test]
fn another_login_shell_is_no_problem() {
    let (home, exe, table) = world("loginother");
    let mut env = env(&home, &table);
    env.login_shell = Box::new(|| Some(PathBuf::from("/bin/zsh-not-here")));
    let mut person = Person::says("nnnnn");
    assert_eq!(run(&[], &mut person, &env, Ok(exe)), 0);
}

#[test]
fn packages_cargo_and_builds_are_refused_with_their_own_way() {
    let (home, _exe, table) = world("places");
    let mut env = env(&home, &table);
    env.cargo_home = Some(PathBuf::from("/opt/cargo"));
    for (path, word) in [
        ("/usr/bin/oh-my-shell", "apt remove"),
        ("/opt/cargo/bin/oh-my-shell", "cargo uninstall"),
        ("/home/u/x/target/release/oh-my-shell", "build from source"),
    ] {
        let mut person = Person::says("yyyyy");
        let status = run(&[], &mut person, &env, Ok(PathBuf::from(path)));
        assert_eq!(status, 1, "{path}");
        assert!(person.err.contains(word), "{path}: {}", person.all());
        assert!(person.asked.is_empty(), "{path}");
    }
}

#[test]
fn a_replaced_program_is_refused() {
    let (home, _exe, table) = world("replaced");
    let mut person = Person::says("y");
    let status = run(
        &[],
        &mut person,
        &env(&home, &table),
        Err("it was replaced".to_owned()),
    );
    assert_eq!(status, 1);
    assert!(person.err.contains("replaced"));
}

#[test]
fn bad_arguments_are_a_usage_error() {
    let (home, exe, table) = world("args");
    let mut person = Person::says("");
    assert_eq!(
        run(&["--yes"], &mut person, &env(&home, &table), Ok(exe)),
        2
    );
    assert!(person.err.contains("usage: omsh uninstall"));
}

// -- the questions ------------------------------------------------------------------------

#[test]
fn yes_to_everything_removes_everything_that_is_ours() {
    let (home, exe, table) = world("all");
    let mut env = env(&home, &table);
    env.ai_set_up = true;
    let mut person = Person::says("yyyyy");
    let status = run(&[], &mut person, &env, Ok(exe.clone()));
    assert_eq!(status, 0, "{}", person.all());
    assert_eq!(person.asked.len(), 5, "{:?}", person.asked);
    for gone in [
        ".local/bin/oh-my-shell",
        ".local/bin/omsh",
        ".config/oh-my-shell",
        ".local/share/oh-my-shell",
        ".local/state/oh-my-shell",
    ] {
        assert!(
            !exists(home.p(gone)),
            "{gone} should be gone: {}",
            person.all()
        );
    }
    assert!(table.0.lock().unwrap().is_empty(), "the key was deleted");
}

#[test]
fn no_to_everything_removes_nothing() {
    let (home, exe, table) = world("none");
    let mut person = Person::says("nnnnn");
    let status = run(&[], &mut person, &env(&home, &table), Ok(exe.clone()));
    assert_eq!(status, 0);
    assert!(
        person.out.contains("Nothing was removed"),
        "{}",
        person.all()
    );
    for stays in [
        ".local/bin/oh-my-shell",
        ".local/bin/omsh",
        ".config/oh-my-shell/config.toml",
        ".config/oh-my-shell/credentials",
        ".local/share/oh-my-shell/history.db",
        ".local/share/oh-my-shell/trash/1/file",
        ".local/state/oh-my-shell/update-check",
    ] {
        assert!(exists(home.p(stays)), "{stays}");
    }
}

#[test]
fn enter_is_no() {
    let (home, exe, table) = world("enter");
    let mut person = Person::says("\n\n\n\n\n");
    run(&[], &mut person, &env(&home, &table), Ok(exe.clone()));
    assert!(exists(exe));
}

#[test]
fn each_answer_is_for_its_own_question() {
    // program: no, settings: yes, history: no, trash: yes
    let (home, exe, table) = world("mixed");
    let mut person = Person::says("nynyn");
    let status = run(&[], &mut person, &env(&home, &table), Ok(exe.clone()));
    assert_eq!(status, 0, "{}", person.all());
    assert!(exists(exe), "program kept");
    assert!(!exists(home.p(".config/oh-my-shell/config.toml")));
    assert!(!exists(home.p(".config/oh-my-shell/init.omsh")));
    assert!(!exists(home.p(".config/oh-my-shell/themes")));
    assert!(
        exists(home.p(".config/oh-my-shell/credentials")),
        "keys were not chosen"
    );
    assert!(
        exists(home.p(".local/share/oh-my-shell/history.db")),
        "history kept"
    );
    assert!(!exists(home.p(".local/share/oh-my-shell/trash")));
    assert!(!exists(home.p(".local/share/oh-my-shell/undo.db")));
    assert!(
        exists(home.p(".local/state/oh-my-shell/update-check")),
        "state goes with the program"
    );
    assert!(person.out.contains("Kept:"), "{}", person.all());
}

#[test]
fn escape_or_ctrl_c_at_a_later_question_removes_nothing_at_all() {
    for (keys, status) in [("yy\u{1b}", 1), ("yyn!", 130), ("\u{1b}", 1), ("!", 130)] {
        let (home, exe, table) = world("cancel");
        let mut person = Person::says(keys);
        let got = run(&[], &mut person, &env(&home, &table), Ok(exe.clone()));
        assert_eq!(got, status, "{keys:?}");
        assert!(
            person.err.contains("Nothing was removed"),
            "{keys:?}: {}",
            person.all()
        );
        for stays in [
            ".local/bin/oh-my-shell",
            ".config/oh-my-shell/config.toml",
            ".local/share/oh-my-shell/history.db",
            ".local/state/oh-my-shell/update-check",
        ] {
            assert!(exists(home.p(stays)), "{keys:?}: {stays}");
        }
    }
}

#[test]
fn the_end_of_input_cancels() {
    let (home, exe, table) = world("eof");
    let mut person = Person::says("");
    person.answers.push_back(Err(ReadError::EndOfInput));
    let status = run(&[], &mut person, &env(&home, &table), Ok(exe.clone()));
    assert_eq!(status, 1);
    assert!(exists(exe));
}

#[test]
fn a_question_is_asked_only_for_what_exists() {
    let home = Home::new("sparse");
    let exe = home.put(".local/bin/oh-my-shell", "program");
    let table = Table::default();
    let mut person = Person::says("y");
    let status = run(&[], &mut person, &env(&home, &table), Ok(exe.clone()));
    assert_eq!(status, 0, "{}", person.all());
    assert_eq!(
        person.asked.len(),
        1,
        "only the program: {:?}",
        person.asked
    );
    assert!(!exists(exe));
    assert!(
        !person.asked[0].contains("link"),
        "no link here: {}",
        person.asked[0]
    );
}

#[test]
fn the_key_question_is_asked_when_the_ai_is_set_up_even_without_a_file() {
    let home = Home::new("keyq");
    let exe = home.put(".local/bin/oh-my-shell", "program");
    let table = Table::default();
    table.0.lock().unwrap().insert("anthropic", "k".to_owned());
    let mut env = env(&home, &table);
    env.ai_set_up = true;
    let mut person = Person::says("ny");
    run(&[], &mut person, &env, Ok(exe));
    assert_eq!(person.asked.len(), 2, "{:?}", person.asked);
    assert!(table.0.lock().unwrap().is_empty());
}

#[test]
fn the_trash_question_says_what_cannot_be_restored() {
    let (home, exe, table) = world("trashq");
    let mut person = Person::says("nnnnn");
    run(&[], &mut person, &env(&home, &table), Ok(exe));
    let asked = person.asked.join("\n");
    assert!(asked.contains("1 saved deletion"), "{asked}");
    assert!(asked.contains("can no longer be restored"), "{asked}");
}

// -- what is removed, and what is not -----------------------------------------------------

#[test]
fn a_link_that_is_not_ours_is_left_alone() {
    let (home, exe, table) = world("foreignlink");
    fs::remove_file(home.p(".local/bin/omsh")).unwrap();
    symlink("/usr/bin/true", home.p(".local/bin/omsh")).unwrap();
    let mut person = Person::says("ynnnn");
    run(&[], &mut person, &env(&home, &table), Ok(exe));
    assert!(
        exists(home.p(".local/bin/omsh")),
        "somebody else's link stays"
    );
}

#[test]
fn a_symlinked_config_folder_is_emptied_but_its_target_folder_stays() {
    let (home, exe, table) = world("stow");
    let real = home.p("dotfiles/oh-my-shell");
    fs::create_dir_all(real.parent().unwrap()).unwrap();
    fs::rename(home.p(".config/oh-my-shell"), &real).unwrap();
    symlink(&real, home.p(".config/oh-my-shell")).unwrap();
    let mut person = Person::says("nynnn");
    run(&[], &mut person, &env(&home, &table), Ok(exe));
    assert!(!exists(real.join("config.toml")));
    assert!(exists(real.join("credentials")));
    assert!(exists(real), "the folder the link points to is not deleted");
}

#[test]
fn files_that_are_not_ours_in_a_shared_folder_stay() {
    let (home, exe, table) = world("shared");
    home.put(".local/bin/other-tool", "x");
    home.put(".config/other/config", "x");
    let mut person = Person::says("yyyyn");
    run(&[], &mut person, &env(&home, &table), Ok(exe));
    assert!(exists(home.p(".local/bin/other-tool")));
    assert!(exists(home.p(".config/other/config")));
}

#[test]
fn a_file_in_the_data_folder_that_is_not_history_or_trash_keeps_the_folder() {
    let (home, exe, table) = world("datafile");
    home.put(".local/share/oh-my-shell/notes", "mine");
    let mut person = Person::says("nnyyn");
    run(&[], &mut person, &env(&home, &table), Ok(exe));
    assert!(exists(home.p(".local/share/oh-my-shell/notes")));
}

#[test]
fn etc_shells_is_only_talked_about() {
    let (home, exe, table) = world("shells");
    fs::write(
        home.p("etc-shells"),
        format!("/bin/sh\n{}\n", exe.display()),
    )
    .unwrap();
    let before = fs::read_to_string(home.p("etc-shells")).unwrap();
    let mut person = Person::says("ynnnn");
    run(&[], &mut person, &env(&home, &table), Ok(exe));
    assert!(person.out.contains("is listed in"), "{}", person.all());
    assert!(person.out.contains("remove-shell"), "{}", person.all());
    assert_eq!(fs::read_to_string(home.p("etc-shells")).unwrap(), before);
}

#[test]
fn a_failed_removal_is_reported_and_the_rest_goes_on() {
    if nix::unistd::geteuid().is_root() {
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    let (home, exe, table) = world("failing");
    // The history folder cannot be changed, the settings can.
    let data = home.p(".local/share/oh-my-shell");
    fs::set_permissions(&data, fs::Permissions::from_mode(0o555)).unwrap();
    let mut person = Person::says("nyynn");
    let status = run(&[], &mut person, &env(&home, &table), Ok(exe));
    fs::set_permissions(&data, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(status, 1, "{}", person.all());
    assert!(person.out.contains("Could not remove"), "{}", person.all());
    assert!(
        !exists(home.p(".config/oh-my-shell/config.toml")),
        "the rest went on"
    );
}

#[test]
fn only_folders_named_after_the_program_are_removed_whole() {
    assert!(is_ours(Path::new("/home/u/.local/state/oh-my-shell")));
    assert!(!is_ours(Path::new("/home/u/.local/state")));
    assert!(!is_ours(Path::new("/")));
    assert!(!is_ours(Path::new(
        "/home/u/.local/state/oh-my-shell-backup"
    )));
}
