use std::cell::RefCell;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use super::*;

/// A folder of our own, with a `bin` folder that may hold a program called `eza`.
struct Folder(PathBuf);

impl Folder {
    fn new(name: &str, with_eza: bool) -> Self {
        let dir =
            std::env::temp_dir().join(format!("omsh-first-run-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("bin")).unwrap();
        if with_eza {
            let eza = dir.join("bin/eza");
            fs::write(&eza, "#!/bin/sh\n").unwrap();
            fs::set_permissions(&eza, fs::Permissions::from_mode(0o755)).unwrap();
        }
        Self(dir)
    }

    fn shell(&self) -> Shell {
        let mut shell = Shell::new();
        let mut io = Io::process();
        let line = format!("PATH='{}'", self.0.join("bin").display());
        shell.run_source(&line, false, &mut io);
        shell
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn ls_becomes_an_alias_of_eza_only_when_eza_is_on_the_path() {
    let with = Folder::new("with", true);
    let mut shell = with.shell();
    assert!(define_ls_alias(&mut shell));
    assert!(
        shell
            .alias_list()
            .contains(&("ls".to_owned(), "eza".to_owned()))
    );

    let without = Folder::new("without", false);
    let mut shell = without.shell();
    assert!(!define_ls_alias(&mut shell));
    assert!(shell.alias_list().is_empty(), "{:?}", shell.alias_list());
}

#[test]
fn a_file_called_eza_that_cannot_be_run_is_not_eza() {
    let folder = Folder::new("plain-file", true);
    fs::set_permissions(folder.0.join("bin/eza"), fs::Permissions::from_mode(0o644)).unwrap();
    let mut shell = folder.shell();
    assert!(!define_ls_alias(&mut shell));
}

#[test]
fn the_settings_path_is_shown_with_a_tilde_inside_the_home_folder() {
    let home = PathBuf::from("/home/someone");
    assert_eq!(
        shown_path(&home.join(".config/oh-my-shell/config.toml"), Some(&home)),
        "~/.config/oh-my-shell/config.toml"
    );
    assert_eq!(
        shown_path(&PathBuf::from("/etc/x/config.toml"), Some(&home)),
        "/etc/x/config.toml"
    );
    assert_eq!(
        shown_path(&PathBuf::from("/etc/x/config.toml"), None),
        "/etc/x/config.toml"
    );
}

#[test]
fn an_answer_is_saved_to_one_key_and_a_failed_save_is_said() {
    let folder = Folder::new("save", false);
    let file = folder.0.join("config.toml");
    fs::write(&file, omsh_config::TEMPLATE).unwrap();
    let said = RefCell::new(Vec::new());
    let say = |line: &str| said.borrow_mut().push(line.to_owned());

    assert!(save(Some(&file), "update.check", &Change::Bool(true), &say));
    assert!(save(
        Some(&file),
        "ui.ls",
        &Change::Text("eza".to_owned()),
        &say
    ));
    let loaded = omsh_config::parse(&fs::read_to_string(&file).unwrap());
    assert!(loaded.config.update.check);
    assert_eq!(loaded.config.ui.ls, LsProgram::Eza);
    assert!(said.borrow().is_empty());

    // Everything else in the file is as it was: only two lines differ from the template.
    let changed = fs::read_to_string(&file).unwrap();
    let before: Vec<&str> = omsh_config::TEMPLATE.lines().collect();
    let after: Vec<&str> = changed.lines().collect();
    let new_lines = after.iter().filter(|line| !before.contains(line)).count();
    assert!(new_lines <= 4, "{changed}");

    // A file that is not there to be changed is refused with a reason.
    let missing_dir = folder.0.join("nowhere/config.toml");
    fs::write(folder.0.join("nowhere"), "a file, not a folder").unwrap();
    assert!(!save(
        Some(&missing_dir),
        "ui.ls",
        &Change::Text("eza".into()),
        &say
    ));
    assert_eq!(said.borrow().len(), 1);
    assert!(
        said.borrow()[0].contains("could not save ui.ls"),
        "{:?}",
        said.borrow()
    );
}
