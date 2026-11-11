//! Following the `mkdir`, `mv` and redirections of a plan step, so that `undo` can reverse them
//! (§12.5).
//!
//! The analyzer says which commands of the step *may* change files ([`Change`]). Before the
//! step runs, [`Watch::before`] looks at the disk: what exists now, and a copy of every file
//! the step is about to overwrite (up to [`MAX_BACKUP`]). After it ran, [`Watch::after`]
//! looks again and records only what really changed, so a step that failed halfway, or a
//! `mv` that did nothing, leaves no false entry. Whatever cannot be followed (a word only
//! known when it runs, a file too big to copy) is said, never silently skipped.

use std::fs;
use std::path::{Path, PathBuf};

use omsh_safety::blast::Target;
use omsh_safety::changes::{Change, ChangeKind};
use omsh_safety::paths;
use omsh_safety::trash::{self, Kind, Meta};

use super::Undo;

/// The biggest file whose old content is kept when a step overwrites it.
pub const MAX_BACKUP: u64 = 10 * 1024 * 1024;

/// A copy of a file's old content.
struct Backup {
    file: PathBuf,
    folder: PathBuf,
    meta: Meta,
}

enum Entry {
    /// Folders that did not exist, outermost first.
    Mkdir { missing: Vec<PathBuf> },
    Move {
        source: PathBuf,
        source_meta: Meta,
        /// The destination as the command spells it.
        destination: PathBuf,
        /// `-T`: never a folder to go into.
        no_target_dir: bool,
        /// The destination was a folder before the step, so the source goes into it.
        into_folder: bool,
        backup: Option<Backup>,
    },
    Write {
        path: PathBuf,
        before: Option<(Meta, u64)>,
        backup: Option<Backup>,
    },
}

/// What was there before a step.
pub struct Watch {
    entries: Vec<Entry>,
    notes: Vec<String>,
}

/// The paths a word stands for, from `cwd`.
fn resolve(target: &Target, change: &Change) -> Result<Vec<PathBuf>, String> {
    if target.unknown {
        return Err(format!("{} is only known when the step runs", target.src));
    }
    let mut text = target.text.clone();
    if text == "~" || text.starts_with("~/") {
        let home = change
            .home
            .as_deref()
            .ok_or_else(|| format!("the home folder is not known for {}", target.src))?;
        text = format!("{home}{}", &text[1..]);
    }
    let base = if text.starts_with('/') {
        "/".to_owned()
    } else {
        change
            .cwd
            .clone()
            .ok_or_else(|| format!("the folder {} is relative to is not known", target.src))?
    };
    if target.glob && target.text == text {
        let names = omsh_exec::glob(text.as_bytes(), &target.active, Path::new(&base));
        return Ok(names
            .into_iter()
            .map(|name| PathBuf::from(paths::resolve(&base, &String::from_utf8_lossy(&name))))
            .collect());
    }
    Ok(vec![PathBuf::from(paths::resolve(&base, &text))])
}

fn one(target: &Target, change: &Change) -> Result<PathBuf, String> {
    let mut all = resolve(target, change)?;
    if target.glob || all.len() != 1 {
        return Err(format!("{} is a pattern", target.src));
    }
    all.pop().ok_or_else(|| "no path".to_owned())
}

fn size_of(path: &Path) -> u64 {
    fs::symlink_metadata(path).map_or(0, |data| data.len())
}

impl Watch {
    /// Looks at the disk before the step runs.
    #[must_use]
    pub fn before(undo: &Undo, changes: &[Change], cwd: &Path) -> Self {
        let mut watch = Self {
            entries: Vec::new(),
            notes: Vec::new(),
        };
        let mut writes: Vec<PathBuf> = Vec::new();
        for change in changes {
            if let Some(why) = &change.skip {
                watch
                    .notes
                    .push(format!("not undoable: {} ({why})", change.what()));
                continue;
            }
            let result = match change.kind {
                ChangeKind::Mkdir { parents } => watch.mkdir(change, parents),
                ChangeKind::Move { no_target_dir } => watch.moves(undo, change, no_target_dir, cwd),
                ChangeKind::Write => watch.write(undo, change, cwd, &mut writes),
            };
            if let Err(why) = result {
                watch
                    .notes
                    .push(format!("not undoable: {} ({why})", change.what()));
            }
        }
        watch
    }

    fn mkdir(&mut self, change: &Change, parents: bool) -> Result<(), String> {
        for target in &change.targets {
            for path in resolve(target, change)? {
                let mut missing = Vec::new();
                let mut at = Some(path.as_path());
                while let Some(dir) = at {
                    if trash::exists(dir) {
                        break;
                    }
                    missing.push(dir.to_path_buf());
                    if !parents {
                        break;
                    }
                    at = dir.parent();
                }
                missing.reverse();
                if !missing.is_empty() {
                    self.entries.push(Entry::Mkdir { missing });
                }
            }
        }
        Ok(())
    }

    /// A copy of the file at `path`, if it is a regular file small enough.
    fn backup(&mut self, undo: &Undo, path: &Path, meta: &Meta, cwd: &Path) -> Option<Backup> {
        if meta.kind != Kind::File {
            return None;
        }
        if size_of(path) > MAX_BACKUP {
            self.notes.push(format!(
                "not undoable: the old content of {} is over 10 MiB and is not kept",
                path.display()
            ));
            return None;
        }
        let folder = undo
            .with_action(cwd, "plan step", |_, action| action.backup_folder())
            .and_then(|folder| folder)
            .map_err(|why| {
                self.notes
                    .push(format!("not undoable: no place for a backup ({why})"));
            })
            .ok()?;
        let file = folder.join(path.file_name()?);
        if let Err(error) = fs::copy(path, &file) {
            self.notes.push(format!(
                "not undoable: the old content of {} could not be kept ({error})",
                path.display()
            ));
            let _ = trash::remove_tree(&folder);
            return None;
        }
        Some(Backup {
            file,
            folder,
            meta: *meta,
        })
    }

    fn moves(
        &mut self,
        undo: &Undo,
        change: &Change,
        no_target_dir: bool,
        cwd: &Path,
    ) -> Result<(), String> {
        let (last, sources) = change
            .targets
            .split_last()
            .ok_or_else(|| "no destination".to_owned())?;
        let destination = one(last, change)?;
        let dest_is_dir =
            !no_target_dir && trash::meta(&destination).is_ok_and(|meta| meta.kind == Kind::Dir);
        let mut all = Vec::new();
        for target in sources {
            all.extend(resolve(target, change)?);
        }
        if all.len() > 1 && !dest_is_dir {
            return Err("several sources and the destination is not a folder".to_owned());
        }
        for source in all {
            let Ok(source_meta) = trash::meta(&source) else {
                continue;
            };
            let final_path = if dest_is_dir {
                match source.file_name() {
                    Some(name) => destination.join(name),
                    None => continue,
                }
            } else {
                destination.clone()
            };
            // What is overwritten is looked at now. Where the source will end up is worked
            // out after the step, because an earlier command of the step may make the folder.
            let backup = match trash::meta(&final_path) {
                Ok(existing) if existing.kind == Kind::File => {
                    self.backup(undo, &final_path, &existing, cwd)
                }
                Ok(_) | Err(_) => None,
            };
            self.entries.push(Entry::Move {
                source,
                source_meta,
                destination: destination.clone(),
                no_target_dir,
                into_folder: dest_is_dir,
                backup,
            });
        }
        Ok(())
    }

    fn write(
        &mut self,
        undo: &Undo,
        change: &Change,
        cwd: &Path,
        seen: &mut Vec<PathBuf>,
    ) -> Result<(), String> {
        let target = change.targets.first().ok_or_else(|| "no file".to_owned())?;
        let path = one(target, change)?;
        if seen.contains(&path) {
            return Ok(());
        }
        seen.push(path.clone());
        match trash::meta(&path) {
            Ok(meta) if meta.kind == Kind::File => {
                let size = size_of(&path);
                let backup = self.backup(undo, &path, &meta, cwd);
                if backup.is_some() {
                    self.entries.push(Entry::Write {
                        path,
                        before: Some((meta, size)),
                        backup,
                    });
                }
            }
            Ok(_) => return Err(format!("{} is not a plain file", path.display())),
            Err(_) => self.entries.push(Entry::Write {
                path,
                before: None,
                backup: None,
            }),
        }
        Ok(())
    }

    /// Looks again after the step ran and records what changed. Returns the lines to show
    /// about what could not be followed.
    pub fn after(self, undo: &Undo, cwd: &Path) -> Vec<String> {
        let Self { entries, mut notes } = self;
        for entry in entries {
            match entry {
                Entry::Mkdir { missing } => {
                    for dir in missing {
                        if trash::meta(&dir).is_ok_and(|meta| meta.kind == Kind::Dir) {
                            record(undo, cwd, &mut notes, |journal, action| {
                                journal.note_mkdir(action, &dir)
                            });
                        }
                    }
                }
                Entry::Move {
                    source,
                    source_meta,
                    destination,
                    no_target_dir,
                    into_folder,
                    backup,
                } => {
                    let inside = source.file_name().map(|name| destination.join(name));
                    let candidates: Vec<PathBuf> = match (into_folder, no_target_dir) {
                        (true, _) => inside.into_iter().collect(),
                        (false, true) => vec![destination],
                        (false, false) => inside.into_iter().chain([destination]).collect(),
                    };
                    let landed = if trash::exists(&source) {
                        None
                    } else {
                        candidates.into_iter().find(|candidate| {
                            trash::meta(candidate).is_ok_and(|meta| meta.kind == source_meta.kind)
                        })
                    };
                    match (landed, backup) {
                        (Some(landed), backup) => {
                            if let Some(backup) = &backup {
                                record_backup(undo, cwd, &mut notes, &landed, backup);
                            }
                            record(undo, cwd, &mut notes, |journal, action| {
                                journal.note_rename(action, &source, &landed)
                            });
                        }
                        (None, Some(backup)) => {
                            let _ = trash::remove_tree(&backup.folder);
                        }
                        (None, None) => {}
                    }
                }
                Entry::Write {
                    path,
                    before,
                    backup,
                } => {
                    let now = trash::meta(&path)
                        .ok()
                        .filter(|meta| meta.kind == Kind::File);
                    let Some(now) = now else {
                        if let Some(backup) = backup {
                            let _ = trash::remove_tree(&backup.folder);
                        }
                        continue;
                    };
                    match (before, backup) {
                        (None, _) => record(undo, cwd, &mut notes, |journal, action| {
                            journal.note_write(action, &path, None)
                        }),
                        (Some((was, size)), Some(backup)) => {
                            let changed = (was.mtime_s, was.mtime_ns, was.ino, size)
                                != (now.mtime_s, now.mtime_ns, now.ino, size_of(&path));
                            if changed {
                                record_backup(undo, cwd, &mut notes, &path, &backup);
                            } else {
                                let _ = trash::remove_tree(&backup.folder);
                            }
                        }
                        (Some(_), None) => {}
                    }
                }
            }
        }
        notes
    }
}

fn record_backup(undo: &Undo, cwd: &Path, notes: &mut Vec<String>, path: &Path, backup: &Backup) {
    record(undo, cwd, notes, |journal, action| {
        journal.note_write(action, path, Some((&backup.file, &backup.meta)))
    });
}

fn record(
    undo: &Undo,
    cwd: &Path,
    notes: &mut Vec<String>,
    f: impl FnOnce(&omsh_safety::journal::Journal, &omsh_safety::journal::Action) -> Result<(), String>,
) {
    match undo.with_action(cwd, "plan step", f) {
        Ok(Ok(())) => {}
        Ok(Err(why)) | Err(why) => notes.push(format!("not recorded for undo: {why}")),
    }
}

#[cfg(test)]
mod tests;
