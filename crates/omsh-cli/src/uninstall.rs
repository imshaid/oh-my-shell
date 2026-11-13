//! `omsh uninstall` (S29b, blueprint §18).
//!
//! It removes the program and, only where the person says yes, the files Oh My Shell made:
//! settings, history, the trash with its undo journal, and the AI keys. It asks every question
//! first and removes nothing until all are answered, so Esc or Ctrl+C at any question leaves
//! everything as it was.
//!
//! What it refuses, and why:
//!
//! - **Inside a running Oh My Shell.** That shell has the history and the undo journal open;
//!   its writer could bring files back after they are gone. Exit and run it from another shell
//!   (`omsh uninstall` is then the program, not the builtin). Decided by the owner, 2026-11-13.
//! - **Without a terminal.** A question needs a person.
//! - **While Oh My Shell is the login shell.** Removing it would lock the account out of a
//!   shell. It says how to change the login shell first.
//! - **A copy that a package manager or `cargo` installed, or a build folder.** Those are
//!   removed the way they were installed.
//!
//! It never edits `/etc/shells` (D13: the shell never edits files that are not its own); it
//! says what to remove there.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use omsh_ai::Keys;
use omsh_config::{Paths, Provider};
use omsh_exec::{Key, ReadError, Terminal};

use crate::update::place::{self, Place};

/// The folder name under each XDG directory. Every removal is inside a folder with this name.
const APP: &str = "oh-my-shell";
/// The file with the AI keys, in the settings folder.
const CREDENTIALS: &str = "credentials";
const USAGE_STATUS: u8 = 2;
const CANCELLED_BY_INTERRUPT: u8 = 130;

/// What `omsh uninstall` needs from outside.
pub struct UninstallEnv {
    pub paths: Paths,
    pub home: Option<PathBuf>,
    pub cargo_home: Option<PathBuf>,
    /// The login shell of this user, from the account database. A function, so that the
    /// database is read when the command runs and not at every start.
    pub login_shell: Box<dyn Fn() -> Option<PathBuf> + Send + Sync>,
    /// `/etc/shells`, looked at (never changed) to say what to remove there.
    pub etc_shells: PathBuf,
    pub keys: Keys,
    /// `[ai] provider` is set: the key question is worth asking even without a credentials file.
    pub ai_set_up: bool,
    /// Started as the `omsh` program. False inside the interactive shell, where it is refused.
    pub as_tool: bool,
}

const USAGE: &str = "\
usage: omsh uninstall
  Removes the program and the `omsh` link. It asks before it removes your settings, history,
  trash and AI keys. Run it from another shell, not from inside Oh My Shell.
";

/// What the person agreed to remove.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Choice {
    pub program: bool,
    pub settings: bool,
    pub history: bool,
    pub trash: bool,
    pub keys: bool,
}

/// The files of one category that exist now.
#[derive(Debug, Default)]
struct Found {
    settings: Vec<PathBuf>,
    credentials: Option<PathBuf>,
    history: Vec<PathBuf>,
    trash: Vec<PathBuf>,
    trash_actions: usize,
}

/// `omsh uninstall`.
pub fn run(
    args: &[&str],
    terminal: &mut dyn Terminal,
    env: &UninstallEnv,
    exe: Result<PathBuf, String>,
) -> u8 {
    match args {
        [] => {}
        ["-h" | "--help"] => {
            terminal.out(USAGE);
            return 0;
        }
        _ => {
            terminal.err(&format!("omsh: uninstall: bad arguments\n{USAGE}"));
            return USAGE_STATUS;
        }
    }
    if !env.as_tool {
        return refuse(
            terminal,
            "it cannot run inside Oh My Shell, which has its history and undo journal open. \
             Exit this shell and run `omsh uninstall` from another shell (bash, fish, ...)",
        );
    }
    if !terminal.input_is_terminal() {
        return refuse(terminal, "it asks questions, so it needs a terminal");
    }
    let exe = match exe {
        Ok(exe) => exe,
        Err(why) => return refuse(terminal, &why),
    };
    match place::classify(&exe, env.home.as_deref(), env.cargo_home.as_deref()) {
        Place::Own => {}
        Place::Package => {
            return refuse(
                terminal,
                "this copy was installed by a package. Remove it with: sudo apt remove oh-my-shell. \
                 Your files in your home folder stay; delete them by hand if you want",
            );
        }
        Place::Cargo => {
            return refuse(
                terminal,
                "this copy was installed by cargo. Remove it with: cargo uninstall omsh-cli",
            );
        }
        Place::Build => {
            return refuse(
                terminal,
                "this is a build from source (a target/ folder), not an installed copy",
            );
        }
    }
    let login_shell = (env.login_shell)();
    if is_login_shell(login_shell.as_deref(), &exe) {
        return refuse(
            terminal,
            &format!(
                "Oh My Shell is your login shell ({}). Change it first, for example: \
                 chsh -s /bin/bash   Then log in again and run this again",
                login_shell
                    .as_deref()
                    .map_or_else(String::new, |p| p.display().to_string())
            ),
        );
    }

    let found = look_around(env);
    let choice = match ask(terminal, env, &exe, &found) {
        Ok(choice) => choice,
        Err(status) => {
            terminal.err("Cancelled. Nothing was removed.\n");
            return status;
        }
    };
    if choice == Choice::default() {
        terminal.out("Nothing was removed.\n");
        return 0;
    }
    let report = remove(env, &exe, &found, choice);
    terminal.out(&report.text);
    shells_note(terminal, &env.etc_shells, &exe, choice.program);
    u8::from(report.failed)
}

fn refuse(terminal: &mut dyn Terminal, why: &str) -> u8 {
    terminal.err(&format!("omsh: uninstall: {why}\n"));
    1
}

/// Whether `login_shell` is this very program (the same file, through any link).
fn is_login_shell(login_shell: Option<&Path>, exe: &Path) -> bool {
    let Some(login_shell) = login_shell else {
        return false;
    };
    match (fs::canonicalize(login_shell), fs::canonicalize(exe)) {
        (Ok(a), Ok(b)) => a == b,
        _ => login_shell == exe,
    }
}

/// Which of the files exist now.
fn look_around(env: &UninstallEnv) -> Found {
    let mut found = Found::default();
    if let Some(dir) = env.paths.config_dir()
        && let Ok(entries) = fs::read_dir(dir)
    {
        for entry in entries.flatten() {
            if entry.file_name() == CREDENTIALS {
                found.credentials = Some(entry.path());
            } else {
                found.settings.push(entry.path());
            }
        }
        found.settings.sort();
    }
    if let Some(dir) = env.paths.data_dir() {
        for name in ["history.db", "history.db-wal", "history.db-shm"] {
            if dir.join(name).symlink_metadata().is_ok() {
                found.history.push(dir.join(name));
            }
        }
        for name in [
            "undo.db",
            "undo.db-wal",
            "undo.db-shm",
            "undo.db-journal",
            "trash",
        ] {
            if dir.join(name).symlink_metadata().is_ok() {
                found.trash.push(dir.join(name));
            }
        }
        found.trash_actions = fs::read_dir(dir.join("trash")).map_or(0, Iterator::count);
    }
    found
}

/// Asks every question. `Err` is the exit status of a cancel.
fn ask(
    terminal: &mut dyn Terminal,
    env: &UninstallEnv,
    exe: &Path,
    found: &Found,
) -> Result<Choice, u8> {
    terminal.err(
        "Oh My Shell uninstall. Nothing is removed until every question is answered; \
         Esc or Ctrl+C stops with nothing removed.\n\n",
    );
    let mut choice = Choice::default();
    let link = link_beside(exe);
    let program = match &link {
        Some(link) => format!(
            "Remove the program {} and the link {}?",
            exe.display(),
            link.display()
        ),
        None => format!("Remove the program {}?", exe.display()),
    };
    choice.program = question(terminal, &program)?;
    if !found.settings.is_empty() {
        let dir = env
            .paths
            .config_dir()
            .map_or_else(String::new, |d| d.display().to_string());
        choice.settings = question(
            terminal,
            &format!("Remove your settings (config.toml, init.omsh, themes) in {dir}?"),
        )?;
    }
    if !found.history.is_empty() {
        choice.history = question(
            terminal,
            &format!(
                "Remove your command history ({})?",
                found.history[0].display()
            ),
        )?;
    }
    if !found.trash.is_empty() {
        let what = if found.trash_actions > 0 {
            format!(
                "{} saved deletion(s); files in it can no longer be restored with undo",
                found.trash_actions
            )
        } else {
            "nothing saved in it".to_owned()
        };
        choice.trash = question(
            terminal,
            &format!("Remove the trash and the undo journal ({what})?"),
        )?;
    }
    if found.credentials.is_some() || env.ai_set_up {
        choice.keys = question(
            terminal,
            "Remove the stored AI keys (the keyring entries oh-my-shell/<provider> and the \
             credentials file)?",
        )?;
    }
    Ok(choice)
}

/// One yes/no question, default no.
fn question(terminal: &mut dyn Terminal, text: &str) -> Result<bool, u8> {
    match terminal.read_key(&format!("{text} [y/N] ")) {
        Ok(Key::Char('y' | 'Y')) => {
            terminal.err("yes\n");
            Ok(true)
        }
        Ok(Key::Char(_) | Key::Enter) => {
            terminal.err("no\n");
            Ok(false)
        }
        Ok(Key::Escape) => {
            terminal.err("\n");
            Err(1)
        }
        Err(ReadError::Interrupted) => {
            terminal.err("\n");
            Err(CANCELLED_BY_INTERRUPT)
        }
        Err(ReadError::EndOfInput | ReadError::Failed) => {
            terminal.err("\n");
            Err(1)
        }
    }
}

/// The `omsh` link next to the program, if there is one and it is ours.
fn link_beside(exe: &Path) -> Option<PathBuf> {
    let link = exe.parent()?.join("omsh");
    let target = fs::read_link(&link).ok()?;
    let ours = target == Path::new(APP) || target == exe || target.file_name()? == APP;
    ours.then_some(link)
}

struct Report {
    text: String,
    failed: bool,
}

/// Removes what was chosen. Each removal is reported; one that fails does not stop the others.
fn remove(env: &UninstallEnv, exe: &Path, found: &Found, choice: Choice) -> Report {
    let mut text = String::new();
    let mut failed = false;
    let mut done = |what: &str, result: io::Result<bool>| match result {
        Ok(true) => text.push_str(&format!("Removed {what}\n")),
        Ok(false) => {}
        Err(error) => {
            failed = true;
            text.push_str(&format!("Could not remove {what}: {error}\n"));
        }
    };

    if choice.program {
        if let Some(link) = link_beside(exe) {
            done(&link.display().to_string(), remove_path(&link));
        }
        done(&exe.display().to_string(), remove_path(exe));
        if let Some(state) = env.paths.state_dir()
            && is_ours(state)
        {
            done(&state.display().to_string(), remove_path(state));
        }
    }
    if choice.settings {
        for path in &found.settings {
            done(&path.display().to_string(), remove_path(path));
        }
    }
    if choice.history {
        for path in &found.history {
            done(&path.display().to_string(), remove_path(path));
        }
    }
    if choice.trash {
        for path in &found.trash {
            done(&path.display().to_string(), remove_path(path));
        }
    }
    if choice.keys {
        for provider in Provider::ALL {
            match env.keys.delete(provider) {
                Ok(true) => text.push_str(&format!("Removed the key for {}\n", provider.label())),
                Ok(false) => {}
                Err(error) => {
                    failed = true;
                    text.push_str(&format!(
                        "Could not remove the key for {}: {error}\n",
                        provider.label()
                    ));
                }
            }
        }
        // The file itself, when nothing else is in it that the stores keep.
        if let Some(path) = &found.credentials {
            let _ = remove_path(path);
        }
    }
    // Folders that are empty now go too; a folder that still holds something stays.
    for dir in [env.paths.config_dir(), env.paths.data_dir()]
        .into_iter()
        .flatten()
    {
        if is_ours(dir) && fs::remove_dir(dir).is_ok() {
            text.push_str(&format!("Removed the empty folder {}\n", dir.display()));
        }
    }
    kept_note(&mut text, env, choice, found);
    Report { text, failed }
}

/// What stays, so the person knows.
fn kept_note(text: &mut String, env: &UninstallEnv, choice: Choice, found: &Found) {
    let mut kept = Vec::new();
    if !choice.settings && !found.settings.is_empty() {
        kept.extend(env.paths.config_dir().map(|d| d.display().to_string()));
    }
    if !choice.history && !found.history.is_empty() {
        kept.extend(found.history.first().map(|p| p.display().to_string()));
    }
    if !choice.trash && !found.trash.is_empty() {
        kept.extend(
            env.paths
                .data_dir()
                .map(|d| format!("{}/trash", d.display())),
        );
    }
    if !choice.keys && found.credentials.is_some() {
        kept.extend(found.credentials.as_ref().map(|p| p.display().to_string()));
    }
    kept.dedup();
    if !kept.is_empty() {
        text.push_str(&format!("Kept: {}\n", kept.join(", ")));
    }
}

/// Whether `path` is the Oh My Shell folder of an XDG directory (a safety net: nothing else is
/// ever removed as a whole folder).
fn is_ours(path: &Path) -> bool {
    path.file_name().is_some_and(|name| name == APP)
}

/// Removes a file, a link, or a folder with its contents; a link is removed itself and not
/// followed. `Ok(false)` when it was not there.
fn remove_path(path: &Path) -> io::Result<bool> {
    let meta = match path.symlink_metadata() {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    if meta.is_dir() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(true)
}

/// Tells what to do about `/etc/shells`, which this command never edits.
fn shells_note(terminal: &mut dyn Terminal, etc_shells: &Path, exe: &Path, removed: bool) {
    if !removed {
        return;
    }
    let listed = fs::read_to_string(etc_shells)
        .is_ok_and(|text| text.lines().any(|line| Path::new(line.trim()) == exe));
    if listed {
        terminal.out(&format!(
            "{} is listed in {}. Take that line out yourself, for example: sudo remove-shell {}\n",
            exe.display(),
            etc_shells.display(),
            exe.display()
        ));
    }
    terminal.out("If a terminal profile still starts oh-my-shell, change it.\n");
}

#[cfg(test)]
mod tests;
