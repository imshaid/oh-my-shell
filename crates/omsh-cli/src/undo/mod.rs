//! The trash and `undo` (S18, §12.4 and §12.5), as the shell uses them.
//!
//! [`Undo`] owns the journal (opened on first use, never at start-up) and the *session* of the
//! line being run: all `rm`s that go through the trash on one line, and the plan step's
//! `mkdir`, `mv` and redirections that [`watch`] follows, are one action, so one `undo` brings
//! the whole line back. The shell's builtins `__omsh_trash` (what an `rm` is rewritten to) and
//! `undo` call into it through [`JournalCommand`].

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use chrono::{Local, TimeZone};
use omsh_exec::{JournalCommand, Terminal};
use omsh_safety::journal::{Action, ItemKind, Journal, Origin, UndoError, one_line};
use omsh_safety::paths;
use omsh_safety::trash::{self, Kind};

pub mod watch;

/// How many items `undo` lists before it asks, and how wide a command is in `undo --list`.
const SHOWN_ITEMS: usize = 10;
const LIST_COMMANDS: usize = 70;
const LIST_ROWS: u32 = 20;

/// The line being run.
struct Session {
    text: String,
    origin: Origin,
    action: Option<Action>,
}

/// What a finished line recorded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    pub id: i64,
    pub items: u32,
    pub trashed: u32,
}

impl Summary {
    /// The sentence printed after the line.
    #[must_use]
    pub fn line(&self) -> String {
        let plural = |n: u32, one: &str, many: &str| {
            if n == 1 {
                format!("{n} {one}")
            } else {
                format!("{n} {many}")
            }
        };
        if self.trashed == self.items {
            format!(
                "oh-my-shell: moved {} to the trash; \"undo\" brings {} back (#{})",
                plural(self.trashed, "item", "items"),
                if self.items == 1 { "it" } else { "them" },
                self.id
            )
        } else {
            format!(
                "oh-my-shell: recorded {} ({} moved to the trash); \"undo\" reverses them (#{})",
                plural(self.items, "change", "changes"),
                self.trashed,
                self.id
            )
        }
    }
}

/// The trash and the journal of one shell.
pub struct Undo {
    data_dir: PathBuf,
    journal: OnceLock<Result<Journal, String>>,
    session: Mutex<Option<Session>>,
}

impl Undo {
    #[must_use]
    pub fn new(data_dir: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            data_dir,
            journal: OnceLock::new(),
            session: Mutex::new(None),
        })
    }

    /// The journal, opened now if it was not. The failure to open is kept: it is not tried
    /// again, so a broken file system costs one message and not one per `rm`.
    ///
    /// # Errors
    ///
    /// The reason the journal cannot be used.
    pub fn journal(&self) -> Result<&Journal, &str> {
        self.journal
            .get_or_init(|| Journal::open(&self.data_dir))
            .as_ref()
            .map_err(String::as_str)
    }

    fn session(&self) -> std::sync::MutexGuard<'_, Option<Session>> {
        self.session.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A line is about to run. `text` is the line (masked here: it is kept in the database).
    pub fn begin_line(&self, text: &str, origin: Origin) {
        *self.session() = Some(Session {
            text: omsh_secrets::mask(text),
            origin,
            action: None,
        });
    }

    /// The line has run. Returns what it recorded, if anything.
    pub fn end_line(&self) -> Option<Summary> {
        let session = self.session().take()?;
        let action = session.action?;
        let journal = self.journal().ok()?;
        let items = journal.finish(&action);
        if items == 0 {
            return None;
        }
        let trashed = journal
            .items(action.id)
            .iter()
            .filter(|item| item.kind == ItemKind::Trash)
            .count();
        Some(Summary {
            id: action.id,
            items,
            trashed: u32::try_from(trashed).unwrap_or(u32::MAX),
        })
    }

    /// Runs `f` with the journal and the action of the line being run, which is begun now if it
    /// was not. Without a line (a `__omsh_trash` typed by hand), a line is made up from `words`.
    ///
    /// # Errors
    ///
    /// The reason the journal or the action cannot be had.
    pub fn with_action<R>(
        &self,
        cwd: &Path,
        words: &str,
        f: impl FnOnce(&Journal, &Action) -> R,
    ) -> Result<R, String> {
        let journal = self.journal().map_err(str::to_owned)?;
        let mut guard = self.session();
        let session = guard.get_or_insert_with(|| Session {
            text: omsh_secrets::mask(words),
            origin: Origin::Typed,
            action: None,
        });
        if session.action.is_none() {
            session.action = Some(journal.begin(cwd, &session.text, session.origin)?);
        }
        let action = session.action.as_ref().ok_or("no action")?;
        Ok(f(journal, action))
    }
}

// -- __omsh_trash -----------------------------------------------------------------------------

#[derive(Debug, Default, PartialEq, Eq)]
struct RmArgs {
    force: bool,
    recursive: bool,
    dir: bool,
    verbose: bool,
    operands: Vec<Vec<u8>>,
}

/// Reads the words of an `rm` the way the trash understands them.
fn parse_rm(args: &[Vec<u8>]) -> Result<RmArgs, String> {
    let mut out = RmArgs::default();
    let mut options_end = false;
    for arg in args {
        let text = String::from_utf8_lossy(arg);
        if options_end || arg.len() < 2 || !text.starts_with('-') {
            out.operands.push(arg.clone());
        } else if text == "--" {
            options_end = true;
        } else if let Some(long) = text.strip_prefix("--") {
            match long {
                "force" => out.force = true,
                "recursive" => out.recursive = true,
                "dir" => out.dir = true,
                "verbose" => out.verbose = true,
                "preserve-root" => {}
                _ => return Err(format!("the trash does not do the option --{long}")),
            }
        } else {
            for letter in text[1..].chars() {
                match letter {
                    'f' => out.force = true,
                    'r' | 'R' => out.recursive = true,
                    'd' => out.dir = true,
                    'v' => out.verbose = true,
                    other => {
                        return Err(format!("the trash does not do the option -{other}"));
                    }
                }
            }
        }
    }
    Ok(out)
}

fn shown(path: &[u8]) -> String {
    let text = String::from_utf8_lossy(path);
    one_line(&text, 120)
}

impl Undo {
    fn trash_args(&self, args: &[Vec<u8>], cwd: &[u8], terminal: &mut dyn Terminal) -> u8 {
        let parsed = match parse_rm(args) {
            Ok(parsed) => parsed,
            Err(why) => {
                terminal.err(&format!("oh-my-shell: rm: {why}; nothing was deleted\n"));
                return 2;
            }
        };
        if parsed.operands.is_empty() {
            if parsed.force {
                return 0;
            }
            terminal.err("oh-my-shell: rm: missing operand\n");
            return 1;
        }
        let cwd = Path::new(OsStr::from_bytes(cwd));
        let home = std::env::var("HOME").ok();
        let words = format!(
            "rm {}",
            parsed
                .operands
                .iter()
                .map(|operand| shown(operand))
                .collect::<Vec<_>>()
                .join(" ")
        );
        let mut status = 0;
        for operand in &parsed.operands {
            if omsh_exec::interrupted() {
                return 130;
            }
            let name = shown(operand);
            let given = Path::new(OsStr::from_bytes(operand));
            // `/`, `.`, `..` and `a/..` have no name of their own.
            if given.file_name().is_none() {
                terminal.err(&format!(
                    "oh-my-shell: rm: cannot move '{name}' to the trash: refusing to remove '.', '..' or '/'\n"
                ));
                status = 1;
                continue;
            }
            let path = cwd.join(given);
            let text = path.to_string_lossy();
            if paths::classify(&text, false, cwd.to_str(), home.as_deref()).is_critical() {
                terminal.err(&format!(
                    "oh-my-shell: rm: cannot move '{name}' to the trash: it is the home folder or a system folder\n"
                ));
                status = 1;
                continue;
            }
            match trash::meta(&path) {
                Ok(meta) => {
                    if meta.kind == Kind::Dir && !parsed.recursive {
                        let empty = std::fs::read_dir(&path)
                            .is_ok_and(|mut entries| entries.next().is_none());
                        if !(parsed.dir && empty) {
                            terminal.err(&format!(
                                "oh-my-shell: rm: cannot remove '{name}': {}\n",
                                if parsed.dir {
                                    "Directory not empty"
                                } else {
                                    "Is a directory"
                                }
                            ));
                            status = 1;
                            continue;
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    if !parsed.force {
                        terminal.err(&format!(
                            "oh-my-shell: rm: cannot remove '{name}': No such file or directory\n"
                        ));
                        status = 1;
                    }
                    continue;
                }
                Err(error) => {
                    terminal.err(&format!(
                        "oh-my-shell: rm: cannot remove '{name}': {error}\n"
                    ));
                    status = 1;
                    continue;
                }
            }
            let moved = self.with_action(cwd, &words, |journal, action| {
                journal.trash(action, &path, &omsh_exec::interrupted)
            });
            match moved {
                Err(why) => {
                    terminal.err(&format!(
                        "oh-my-shell: the trash is not available: {why}; nothing was deleted\n"
                    ));
                    return 1;
                }
                Ok(Err(why)) => {
                    terminal.err(&format!(
                        "oh-my-shell: rm: cannot move to the trash: {why}\n"
                    ));
                    status = 1;
                }
                Ok(Ok(warning)) => {
                    if let Some(warning) = warning {
                        terminal.err(&format!("oh-my-shell: rm: {warning}\n"));
                        status = 1;
                    }
                    if parsed.verbose {
                        terminal.out(&format!("moved '{name}' to the trash\n"));
                    }
                }
            }
        }
        status
    }
}

// -- undo -------------------------------------------------------------------------------------

fn local_time(ms: i64) -> String {
    Local.timestamp_millis_opt(ms).single().map_or_else(
        || "????-??-?? ??:??".to_owned(),
        |time| time.format("%Y-%m-%d %H:%M").to_string(),
    )
}

fn describe(item: &omsh_safety::journal::ItemInfo) -> String {
    let path = item.path.display();
    match item.kind {
        ItemKind::Trash => format!("bring back {path}"),
        ItemKind::Rename => format!(
            "rename {} back to {path}",
            item.other
                .as_deref()
                .map_or_else(String::new, |other| other.display().to_string())
        ),
        ItemKind::Mkdir => format!("remove the folder {path} (only if it is empty)"),
        ItemKind::Write => format!("put back the old content of {path}"),
        ItemKind::Created => format!("remove {path} (the file goes to the trash)"),
    }
}

impl Undo {
    fn undo_args(&self, args: &[Vec<u8>], terminal: &mut dyn Terminal) -> u8 {
        let words: Vec<String> = args
            .iter()
            .map(|arg| String::from_utf8_lossy(arg).into_owned())
            .collect();
        let journal = match self.journal() {
            Ok(journal) => journal,
            Err(why) => {
                terminal.err(&format!("oh-my-shell: undo: not available: {why}\n"));
                return 1;
            }
        };
        let id = match words.as_slice() {
            [] => None,
            [flag] if flag == "--list" || flag == "-l" => return list(journal, terminal),
            [number] => match number.trim_start_matches('#').parse::<i64>() {
                Ok(id) if id > 0 => Some(id),
                _ => return usage(terminal),
            },
            _ => return usage(terminal),
        };
        let info = match id {
            Some(id) => match journal.get(id) {
                Some(info) => info,
                None => {
                    terminal.err(&format!("oh-my-shell: undo: there is no action #{id}\n"));
                    return 1;
                }
            },
            None => match journal.latest_undoable() {
                Some(info) => info,
                None => {
                    terminal.err("oh-my-shell: undo: nothing to undo\n");
                    return 1;
                }
            },
        };
        // Said before the question: there is nothing to ask about.
        match info.state.as_str() {
            "undone" => {
                terminal.err(&format!(
                    "oh-my-shell: undo: #{} has been undone already\n",
                    info.id
                ));
                return 1;
            }
            "undoing" => {
                terminal.err(&format!(
                    "oh-my-shell: undo: #{} is being undone by another shell\n",
                    info.id
                ));
                return 1;
            }
            _ => {}
        }
        let todo: Vec<_> = journal
            .items(info.id)
            .into_iter()
            .filter(|item| item.state != "undone")
            .collect();
        let mut text = format!(
            "oh-my-shell: undo #{} ({}): {}\n  run in {}\n",
            info.id,
            local_time(info.at_ms),
            one_line(&info.command, 120),
            info.cwd.display()
        );
        for item in todo.iter().rev().take(SHOWN_ITEMS) {
            text.push_str(&format!("  - {}\n", one_line(&describe(item), 200)));
        }
        if todo.len() > SHOWN_ITEMS {
            text.push_str(&format!("  ... and {} more\n", todo.len() - SHOWN_ITEMS));
        }
        terminal.err(&text);
        let mut answer = Vec::new();
        match terminal.read_line("Undo it? [y/N] ", false, &mut answer) {
            Ok(()) => {
                let answer = String::from_utf8_lossy(&answer);
                let answer = answer.trim();
                if !(answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes")) {
                    terminal.err("oh-my-shell: nothing was undone\n");
                    return 1;
                }
            }
            Err(_) => {
                terminal.err("\noh-my-shell: nothing was undone\n");
                return 1;
            }
        }
        match journal.undo(info.id, &omsh_exec::interrupted) {
            Ok(report) => {
                for line in &report.lines {
                    terminal.out(&format!("{line}\n"));
                }
                if report.failed == 0 {
                    0
                } else {
                    terminal.err(&format!(
                        "oh-my-shell: undo: {} of {} could not be undone; \"undo {}\" tries the rest again\n",
                        report.failed,
                        report.failed + report.undone,
                        info.id
                    ));
                    1
                }
            }
            Err(UndoError::NotUndoable(why)) => {
                terminal.err(&format!("oh-my-shell: undo: {why}\n"));
                1
            }
            Err(UndoError::NoSuchAction) => {
                terminal.err("oh-my-shell: undo: there is no such action\n");
                1
            }
            Err(UndoError::Database(why)) => {
                terminal.err(&format!("oh-my-shell: undo: {why}\n"));
                1
            }
        }
    }
}

fn usage(terminal: &mut dyn Terminal) -> u8 {
    terminal.err("usage: undo [--list | NUMBER]\n");
    2
}

fn list(journal: &Journal, terminal: &mut dyn Terminal) -> u8 {
    let actions = journal.list(LIST_ROWS);
    if actions.is_empty() {
        terminal.out("nothing has been recorded\n");
        return 0;
    }
    let mut out = String::new();
    for action in actions {
        let state = match action.state.as_str() {
            "done" => String::new(),
            other => format!(" [{other}]"),
        };
        out.push_str(&format!(
            "#{}  {}  {}  {} item{}{state}  {}\n",
            action.id,
            local_time(action.at_ms),
            action.origin.name(),
            action.items,
            if action.items == 1 { "" } else { "s" },
            one_line(&action.command, LIST_COMMANDS)
        ));
    }
    terminal.out(&out);
    0
}

impl JournalCommand for Undo {
    fn trash(&self, args: &[Vec<u8>], cwd: &[u8], terminal: &mut dyn Terminal) -> u8 {
        self.trash_args(args, cwd, terminal)
    }

    fn undo(&self, args: &[Vec<u8>], _cwd: &[u8], terminal: &mut dyn Terminal) -> u8 {
        self.undo_args(args, terminal)
    }
}

// -- retention (§12.4) ------------------------------------------------------------------------

/// Empties the trash of what is older than `days`: claims the old folders, says so in one line
/// through `say`, and removes them on a thread of its own. Cheap when there is nothing: one
/// `read_dir`, and no database is opened.
pub fn purge_old(data_dir: &Path, days: u32, say: &mut dyn FnMut(&str)) {
    let root = data_dir.join("trash");
    let cutoff = trash::now_secs() - i64::from(days) * 86_400;
    let (old, leftovers) = trash::old_folders(&root, cutoff);
    let claimed: Vec<PathBuf> = old
        .iter()
        .filter_map(|f| trash::claim_for_purge(f))
        .collect();
    if !claimed.is_empty() {
        say(&format!(
            "oh-my-shell: emptying {} trash {} older than {days} days\n",
            claimed.len(),
            if claimed.len() == 1 {
                "entry"
            } else {
                "entries"
            }
        ));
    }
    if claimed.is_empty() && leftovers.is_empty() {
        return;
    }
    let data_dir = data_dir.to_path_buf();
    let work: Vec<PathBuf> = claimed.iter().chain(&leftovers).cloned().collect();
    let forget = !claimed.is_empty();
    let spawned = std::thread::Builder::new()
        .name("trash-purge".to_owned())
        .spawn(move || purge_now(&data_dir, &work, forget.then_some(cutoff)));
    drop(spawned);
}

/// The work of [`purge_old`], on the calling thread.
pub fn purge_now(data_dir: &Path, folders: &[PathBuf], forget_before_secs: Option<i64>) {
    for folder in folders {
        let _ = trash::remove_tree(folder);
    }
    if let Some(secs) = forget_before_secs
        && data_dir.join("undo.db").exists()
        && let Ok(journal) = Journal::open(data_dir)
    {
        journal.forget_before(secs * 1000);
    }
}

#[cfg(test)]
mod tests;
