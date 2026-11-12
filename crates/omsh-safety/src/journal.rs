//! The undo journal (§12.5): what the trash and the plan steps changed, and how to undo it.
//!
//! The journal is its own SQLite file, `undo.db`, next to `history.db`. It is written in the
//! calling thread and every change is on disk (`synchronous = FULL`) before the file it
//! describes is touched: the row of a move is written first, as `moving`, and set to `done`
//! afterwards. A crash in between leaves a `moving` row, and [`Journal::undo`] settles it by
//! looking at the disk. Files that are removed or replaced by an undo go to the trash again,
//! under a new action, so an undo never loses data either.

use std::ffi::OsStr;
use std::fs::{self, DirBuilder};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, params};

use crate::trash::{
    self, Kind, Meta, MoveError, RestoreError, RestoreNote, restore as restore_file,
};

const SCHEMA_VERSION: i64 = 1;
/// How long opening waits for another shell that holds the database.
const OPEN_PATIENCE: Duration = Duration::from_secs(2);
/// An action still `open` after this long was left by a shell that died.
const STALE_OPEN_MS: i64 = 60_000;

/// What caused an action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// A line the person typed, answered with "move to the trash".
    Typed,
    /// A step of an AI plan.
    Plan,
    /// An `undo`: the files it replaced or removed.
    Undo,
}

impl Origin {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Typed => "typed",
            Self::Plan => "plan",
            Self::Undo => "undo",
        }
    }

    fn parse(text: &str) -> Self {
        match text {
            "plan" => Self::Plan,
            "undo" => Self::Undo,
            _ => Self::Typed,
        }
    }

    /// A word for listings.
    #[must_use]
    pub const fn name(self) -> &'static str {
        self.as_str()
    }
}

/// What an item of an action is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemKind {
    /// Moved into the trash. `path` is the original, `other` the place in the trash.
    Trash,
    /// Renamed. `path` is where it was, `other` where it went.
    Rename,
    /// A folder that did not exist before. `path` is the folder.
    Mkdir,
    /// A file whose old content is kept. `path` is the file, `other` the copy of the old content.
    Write,
    /// A file that did not exist before. `path` is the file.
    Created,
}

impl ItemKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Trash => "trash",
            Self::Rename => "rename",
            Self::Mkdir => "mkdir",
            Self::Write => "write",
            Self::Created => "created",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "trash" => Self::Trash,
            "rename" => Self::Rename,
            "mkdir" => Self::Mkdir,
            "write" => Self::Write,
            "created" => Self::Created,
            _ => return None,
        })
    }
}

/// An action being recorded.
#[derive(Debug)]
pub struct Action {
    pub id: i64,
    /// The action's folder in the trash.
    pub dir: PathBuf,
    backups: AtomicU32,
}

impl Action {
    /// A place for the old content of a file that is about to be replaced: a fresh folder in
    /// the action's own, so that names never clash. The folder is made.
    ///
    /// # Errors
    ///
    /// The error of making the folder, in words.
    pub fn backup_folder(&self) -> Result<PathBuf, String> {
        let n = self.backups.fetch_add(1, Ordering::Relaxed);
        let folder = self.dir.join(format!("b{n}"));
        private_dir(&folder)?;
        Ok(folder)
    }
}

/// One line of the list of actions.
#[derive(Clone, Debug)]
pub struct ActionInfo {
    pub id: i64,
    pub at_ms: i64,
    pub cwd: PathBuf,
    pub command: String,
    pub origin: Origin,
    pub state: String,
    pub items: u32,
}

/// One thing an action did.
#[derive(Clone, Debug)]
pub struct ItemInfo {
    pub seq: i64,
    pub kind: ItemKind,
    pub path: PathBuf,
    pub other: Option<PathBuf>,
    pub state: String,
}

/// What an undo did.
#[derive(Debug, Default)]
pub struct UndoReport {
    /// One line for each item, in the order they were undone.
    pub lines: Vec<String>,
    pub undone: usize,
    pub failed: usize,
}

/// Why an undo did not start.
#[derive(Debug, PartialEq, Eq)]
pub enum UndoError {
    NoSuchAction,
    /// Already undone, or being undone by another shell.
    NotUndoable(String),
    Database(String),
}

/// The journal.
pub struct Journal {
    connection: Mutex<Connection>,
    root: PathBuf,
}

fn bytes(path: &Path) -> &[u8] {
    path.as_os_str().as_bytes()
}

fn path_of(blob: Vec<u8>) -> PathBuf {
    PathBuf::from(OsStr::from_bytes(&blob))
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

fn db(error: &rusqlite::Error) -> String {
    format!("the undo journal: {error}")
}

fn private_dir(path: &Path) -> Result<(), String> {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .map_err(|e| format!("{}: {e}", path.display()))
}

fn is_busy(error: &rusqlite::Error) -> bool {
    matches!(
        error.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
    )
}

impl Journal {
    /// Opens `undo.db` in `data_dir` (made if needed, mode 0700) and the trash next to it.
    ///
    /// # Errors
    ///
    /// A short message: the folder cannot be made, the file cannot be opened, or it was
    /// written by a newer version.
    pub fn open(data_dir: &Path) -> Result<Self, String> {
        private_dir(data_dir)?;
        let root = data_dir.join("trash");
        private_dir(&root)?;
        let file = data_dir.join("undo.db");
        let started = Instant::now();
        let connection = loop {
            match Self::connect(&file) {
                Err(error) if is_busy(&error) && started.elapsed() < OPEN_PATIENCE => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(error) => return Err(format!("{}: {error}", file.display())),
                Ok(connection) => break connection,
            }
        };
        let _ = fs::set_permissions(&file, fs::Permissions::from_mode(0o600));
        Ok(Self {
            connection: Mutex::new(connection),
            root,
        })
    }

    fn connect(file: &Path) -> rusqlite::Result<Connection> {
        let connection = Connection::open(file)?;
        connection.busy_timeout(Duration::from_secs(2))?;
        connection.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(rusqlite::Error::InvalidParameterName(format!(
                "written by a newer version (schema {version})"
            )));
        }
        if version < SCHEMA_VERSION {
            let transaction = connection.unchecked_transaction()?;
            transaction.execute_batch(
                "CREATE TABLE IF NOT EXISTS action (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     at_ms INTEGER NOT NULL,
                     cwd BLOB NOT NULL,
                     command TEXT NOT NULL,
                     origin TEXT NOT NULL,
                     state TEXT NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS item (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     action_id INTEGER NOT NULL,
                     seq INTEGER NOT NULL,
                     kind TEXT NOT NULL,
                     path BLOB NOT NULL,
                     other BLOB,
                     file_type TEXT,
                     mode INTEGER,
                     uid INTEGER,
                     gid INTEGER,
                     mtime_s INTEGER,
                     mtime_ns INTEGER,
                     dev INTEGER,
                     ino INTEGER,
                     state TEXT NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS item_action ON item(action_id, seq);
                 CREATE INDEX IF NOT EXISTS action_at ON action(at_ms);",
            )?;
            transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            transaction.commit()?;
        }
        Ok(connection)
    }

    /// The trash folder.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.connection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Starts an action: a row, and a folder in the trash for what it moves.
    ///
    /// # Errors
    ///
    /// A message if the row or the folder cannot be made.
    pub fn begin(&self, cwd: &Path, command: &str, origin: Origin) -> Result<Action, String> {
        begin(&self.lock(), &self.root, cwd, command, origin)
    }

    /// Moves `path` into the trash as part of `action`. The row is written before the move.
    ///
    /// # Errors
    ///
    /// A message; the file is where it was, unless it says otherwise.
    pub fn trash(
        &self,
        action: &Action,
        path: &Path,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Option<String>, String> {
        trash_item(&self.lock(), action, path, cancelled)
    }

    /// Notes a rename that has happened: `from` is now `to`.
    ///
    /// # Errors
    ///
    /// A message if the row cannot be written.
    pub fn note_rename(&self, action: &Action, from: &Path, to: &Path) -> Result<(), String> {
        let meta = trash::meta(to).ok();
        note(
            &self.lock(),
            action,
            ItemKind::Rename,
            from,
            Some(to),
            meta.as_ref(),
        )
    }

    /// Notes a folder that has been made.
    ///
    /// # Errors
    ///
    /// A message if the row cannot be written.
    pub fn note_mkdir(&self, action: &Action, path: &Path) -> Result<(), String> {
        note(&self.lock(), action, ItemKind::Mkdir, path, None, None)
    }

    /// Notes a file that has been written: `backup` is a copy of what was in it before, or
    /// `None` if it did not exist.
    ///
    /// # Errors
    ///
    /// A message if the row cannot be written.
    pub fn note_write(
        &self,
        action: &Action,
        path: &Path,
        backup: Option<(&Path, &Meta)>,
    ) -> Result<(), String> {
        match backup {
            Some((copy, meta)) => note(
                &self.lock(),
                action,
                ItemKind::Write,
                path,
                Some(copy),
                Some(meta),
            ),
            None => note(&self.lock(), action, ItemKind::Created, path, None, None),
        }
    }

    /// Ends an action. One that recorded nothing is removed. Returns how many items it has.
    pub fn finish(&self, action: &Action) -> u32 {
        let connection = self.lock();
        let items: u32 = connection
            .query_row(
                "SELECT COUNT(*) FROM item WHERE action_id = ?1",
                [action.id],
                |row| row.get(0),
            )
            .unwrap_or(0);
        if items == 0 {
            let _ = connection.execute("DELETE FROM action WHERE id = ?1", [action.id]);
            let _ = trash::remove_tree(&action.dir);
        } else {
            let _ = connection.execute(
                "UPDATE action SET state = 'done' WHERE id = ?1 AND state = 'open'",
                [action.id],
            );
        }
        items
    }

    /// The newest actions, newest first.
    #[must_use]
    pub fn list(&self, limit: u32) -> Vec<ActionInfo> {
        let connection = self.lock();
        query_actions(
            &connection,
            "SELECT a.id, a.at_ms, a.cwd, a.command, a.origin, a.state,
                    (SELECT COUNT(*) FROM item i WHERE i.action_id = a.id)
             FROM action a ORDER BY a.id DESC LIMIT ?1",
            params![limit],
        )
    }

    /// One action by number.
    #[must_use]
    pub fn get(&self, id: i64) -> Option<ActionInfo> {
        let connection = self.lock();
        query_actions(
            &connection,
            "SELECT a.id, a.at_ms, a.cwd, a.command, a.origin, a.state,
                    (SELECT COUNT(*) FROM item i WHERE i.action_id = a.id)
             FROM action a WHERE a.id = ?1",
            params![id],
        )
        .into_iter()
        .next()
    }

    /// The newest action that `undo` without a number would undo: not itself an undo, and not
    /// undone yet.
    #[must_use]
    pub fn latest_undoable(&self) -> Option<ActionInfo> {
        let connection = self.lock();
        query_actions(
            &connection,
            "SELECT a.id, a.at_ms, a.cwd, a.command, a.origin, a.state,
                    (SELECT COUNT(*) FROM item i WHERE i.action_id = a.id)
             FROM action a
             WHERE a.origin != 'undo' AND a.state IN ('done', 'partial', 'open')
             ORDER BY a.id DESC LIMIT 1",
            [],
        )
        .into_iter()
        .next()
    }

    /// The items of an action, in the order they happened.
    #[must_use]
    pub fn items(&self, id: i64) -> Vec<ItemInfo> {
        let connection = self.lock();
        items_of(&connection, id)
            .into_iter()
            .map(|row| ItemInfo {
                seq: row.seq,
                kind: row.kind,
                path: row.path,
                other: row.other,
                state: row.state,
            })
            .collect()
    }

    /// Removes the rows of actions older than `before_ms`.
    pub fn forget_before(&self, before_ms: i64) {
        let connection = self.lock();
        let _ = connection.execute(
            "DELETE FROM item WHERE action_id IN (SELECT id FROM action WHERE at_ms < ?1)",
            [before_ms],
        );
        let _ = connection.execute("DELETE FROM action WHERE at_ms < ?1", [before_ms]);
    }

    /// Undoes action `id`: its items, last first. The action is claimed first, so two shells
    /// cannot undo it twice. What is replaced or removed goes to the trash under a new action.
    ///
    /// # Errors
    ///
    /// See [`UndoError`].
    pub fn undo(&self, id: i64, cancelled: &dyn Fn() -> bool) -> Result<UndoReport, UndoError> {
        let connection = self.lock();
        let Some(info) = query_actions(
            &connection,
            "SELECT a.id, a.at_ms, a.cwd, a.command, a.origin, a.state, 0
             FROM action a WHERE a.id = ?1",
            params![id],
        )
        .into_iter()
        .next() else {
            return Err(UndoError::NoSuchAction);
        };
        let claimed = connection
            .execute(
                "UPDATE action SET state = 'undoing'
                 WHERE id = ?1 AND (state IN ('done', 'partial')
                                    OR (state = 'open' AND at_ms < ?2))",
                params![id, now_ms() - STALE_OPEN_MS],
            )
            .map_err(|e| UndoError::Database(db(&e)))?;
        if claimed == 0 {
            return Err(UndoError::NotUndoable(match info.state.as_str() {
                "undone" => format!("#{id} has been undone already"),
                "undoing" => format!("#{id} is being undone by another shell"),
                "open" => format!("#{id} is still being recorded by another shell"),
                other => format!("#{id} cannot be undone (state {other})"),
            }));
        }
        let mut report = UndoReport::default();
        let mut replaced: Option<Action> = None;
        for item in items_of(&connection, id)
            .into_iter()
            .rev()
            .filter(|item| item.state != "undone")
        {
            if cancelled() {
                report
                    .lines
                    .push("stopped; the rest is not undone".to_owned());
                report.failed += 1;
                break;
            }
            let result = undo_item(
                &connection,
                &self.root,
                &info,
                &item,
                &mut replaced,
                cancelled,
            );
            if result.done {
                report.undone += 1;
                let _ =
                    connection.execute("UPDATE item SET state = 'undone' WHERE id = ?1", [item.id]);
            } else {
                report.failed += 1;
            }
            report.lines.push(result.line);
        }
        let state = if report.failed == 0 {
            "undone"
        } else {
            "partial"
        };
        let _ = connection.execute(
            "UPDATE action SET state = ?2 WHERE id = ?1",
            params![id, state],
        );
        if let Some(action) = replaced {
            let items: u32 = connection
                .query_row(
                    "SELECT COUNT(*) FROM item WHERE action_id = ?1",
                    [action.id],
                    |row| row.get(0),
                )
                .unwrap_or(0);
            if items == 0 {
                let _ = connection.execute("DELETE FROM action WHERE id = ?1", [action.id]);
                let _ = trash::remove_tree(&action.dir);
            } else {
                let _ = connection.execute(
                    "UPDATE action SET state = 'done' WHERE id = ?1",
                    [action.id],
                );
            }
        }
        Ok(report)
    }
}

fn query_actions(
    connection: &Connection,
    sql: &str,
    params: impl rusqlite::Params,
) -> Vec<ActionInfo> {
    let Ok(mut statement) = connection.prepare(sql) else {
        return Vec::new();
    };
    let Ok(rows) = statement.query_map(params, |row| {
        Ok(ActionInfo {
            id: row.get(0)?,
            at_ms: row.get(1)?,
            cwd: path_of(row.get(2)?),
            command: row.get(3)?,
            origin: Origin::parse(&row.get::<_, String>(4)?),
            state: row.get(5)?,
            items: row.get(6)?,
        })
    }) else {
        return Vec::new();
    };
    rows.flatten().collect()
}

struct Row {
    id: i64,
    seq: i64,
    kind: ItemKind,
    path: PathBuf,
    other: Option<PathBuf>,
    meta: Option<Meta>,
    state: String,
}

fn items_of(connection: &Connection, action: i64) -> Vec<Row> {
    let Ok(mut statement) = connection.prepare(
        "SELECT id, seq, kind, path, other, file_type, mode, uid, gid, mtime_s, mtime_ns,
                dev, ino, state
         FROM item WHERE action_id = ?1 ORDER BY seq",
    ) else {
        return Vec::new();
    };
    let Ok(rows) = statement.query_map([action], |row| {
        let kind: String = row.get(2)?;
        let file_type: Option<String> = row.get(5)?;
        let meta = match (
            file_type.as_deref().and_then(Kind::parse),
            row.get::<_, Option<i64>>(6)?,
        ) {
            (Some(kind), Some(mode)) => Some(Meta {
                kind,
                mode: u32::try_from(mode).unwrap_or(0),
                uid: u32::try_from(row.get::<_, i64>(7)?).unwrap_or(0),
                gid: u32::try_from(row.get::<_, i64>(8)?).unwrap_or(0),
                mtime_s: row.get(9)?,
                mtime_ns: row.get(10)?,
                dev: u64::try_from(row.get::<_, i64>(11)?).unwrap_or(0),
                ino: u64::try_from(row.get::<_, i64>(12)?).unwrap_or(0),
            }),
            _ => None,
        };
        Ok((
            Row {
                id: row.get(0)?,
                seq: row.get(1)?,
                kind: ItemKind::parse(&kind).unwrap_or(ItemKind::Created),
                path: path_of(row.get(3)?),
                other: row.get::<_, Option<Vec<u8>>>(4)?.map(path_of),
                meta,
                state: row.get(13)?,
            },
            kind,
        ))
    }) else {
        return Vec::new();
    };
    // A row of a kind this version does not know is left out rather than guessed at.
    rows.flatten()
        .filter(|(_, kind)| ItemKind::parse(kind).is_some())
        .map(|(row, _)| row)
        .collect()
}

fn begin(
    connection: &Connection,
    root: &Path,
    cwd: &Path,
    command: &str,
    origin: Origin,
) -> Result<Action, String> {
    let at = now_ms();
    connection
        .execute(
            "INSERT INTO action (at_ms, cwd, command, origin, state)
             VALUES (?1, ?2, ?3, ?4, 'open')",
            params![at, bytes(cwd), command, origin.as_str()],
        )
        .map_err(|e| db(&e))?;
    let id = connection.last_insert_rowid();
    let dir = root.join(format!("{}-{id}", at / 1000));
    private_dir(&dir)?;
    Ok(Action {
        id,
        dir,
        backups: AtomicU32::new(0),
    })
}

fn next_seq(connection: &Connection, action: i64) -> Result<i64, String> {
    connection
        .query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM item WHERE action_id = ?1",
            [action],
            |row| row.get(0),
        )
        .map_err(|e| db(&e))
}

/// Writes a row of an action. `at` is the action's number and the item's place in it.
fn insert_item(
    connection: &Connection,
    at: (i64, i64),
    kind: ItemKind,
    path: &Path,
    other: Option<&Path>,
    meta: Option<&Meta>,
    state: &str,
) -> Result<i64, String> {
    connection
        .execute(
            "INSERT INTO item (action_id, seq, kind, path, other, file_type, mode, uid, gid,
                               mtime_s, mtime_ns, dev, ino, state)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                at.0,
                at.1,
                kind.as_str(),
                bytes(path),
                other.map(bytes),
                meta.map(|m| m.kind.as_str()),
                meta.map(|m| i64::from(m.mode)),
                meta.map(|m| i64::from(m.uid)),
                meta.map(|m| i64::from(m.gid)),
                meta.map(|m| m.mtime_s),
                meta.map(|m| m.mtime_ns),
                meta.map(|m| i64::try_from(m.dev).unwrap_or(0)),
                meta.map(|m| i64::try_from(m.ino).unwrap_or(0)),
                state,
            ],
        )
        .map_err(|e| db(&e))?;
    Ok(connection.last_insert_rowid())
}

fn note(
    connection: &Connection,
    action: &Action,
    kind: ItemKind,
    path: &Path,
    other: Option<&Path>,
    meta: Option<&Meta>,
) -> Result<(), String> {
    let seq = next_seq(connection, action.id)?;
    insert_item(
        connection,
        (action.id, seq),
        kind,
        path,
        other,
        meta,
        "done",
    )?;
    Ok(())
}

/// Moves `path` into the folder of `action`. `Ok(Some(warning))` if the move is done but the
/// original could not be removed completely.
fn trash_item(
    connection: &Connection,
    action: &Action,
    path: &Path,
    cancelled: &dyn Fn() -> bool,
) -> Result<Option<String>, String> {
    let meta = trash::meta(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let name = path
        .file_name()
        .ok_or_else(|| format!("{}: has no name", path.display()))?;
    let seq = next_seq(connection, action.id)?;
    let folder = action.dir.join(seq.to_string());
    private_dir(&folder)?;
    let target = folder.join(name);
    // The row first: a crash after the move still leaves a way back.
    let item = insert_item(
        connection,
        (action.id, seq),
        ItemKind::Trash,
        path,
        Some(&target),
        Some(&meta),
        "moving",
    )?;
    let finish = |state: &str| {
        let _ = connection.execute(
            "UPDATE item SET state = ?2 WHERE id = ?1",
            params![item, state],
        );
    };
    match trash::move_to(path, &target, cancelled) {
        Ok(_) => {
            finish("done");
            Ok(None)
        }
        Err(MoveError::Partial(error)) => {
            finish("done");
            Ok(Some(format!(
                "{}: copied to the trash, but the original could not be removed completely: {error}",
                path.display()
            )))
        }
        Err(MoveError::Failed(error)) => {
            let _ = connection.execute("DELETE FROM item WHERE id = ?1", [item]);
            let _ = trash::remove_tree(&folder);
            Err(format!("{}: {error}", path.display()))
        }
    }
}

struct ItemResult {
    line: String,
    done: bool,
}

impl ItemResult {
    fn ok(line: String) -> Self {
        Self { line, done: true }
    }

    fn no(line: String) -> Self {
        Self { line, done: false }
    }
}

fn restore_words(error: &RestoreError, path: &Path) -> String {
    match error {
        RestoreError::Exists => format!(
            "not restored: {} exists now; the file stays in the trash",
            path.display()
        ),
        RestoreError::Move(error) => format!("not restored: {}: {}", path.display(), error.error()),
        RestoreError::Io(error) => format!("not restored: {}: {error}", path.display()),
    }
}

fn notes_words(notes: &[RestoreNote]) -> String {
    let mut out = String::new();
    for note in notes {
        out.push_str(match note {
            RestoreNote::MadeFolders => " (made the missing folders)",
            RestoreNote::OwnerNotRestored => " (the owner could not be set back)",
        });
    }
    out
}

/// Whether the file now at the new name is the one that was moved there.
///
/// Device and inode alone are not enough: a file system may hand the number of a deleted file
/// to the next file it creates (ext4 does it at once), so a file that someone deleted and
/// replaced would pass. The **birth time** tells them apart: the file that was moved existed
/// before the action was recorded (`at_ms`), a replacement was born after it. The 1 ms is
/// because `at_ms` is cut to whole milliseconds. A file edited after the move keeps its birth
/// time, so it still goes back.
///
/// Where the file system has no birth time the answer comes from the modification time that
/// was recorded: a file edited after the move is then refused too (the safe side).
fn is_same_file(
    now: &trash::Meta,
    then: &trash::Meta,
    born: &std::io::Result<SystemTime>,
    at_ms: i64,
) -> bool {
    if now.dev != then.dev || now.ino != then.ino {
        return false;
    }
    match born {
        Ok(born) => {
            let limit = UNIX_EPOCH + Duration::from_millis(u64::try_from(at_ms).unwrap_or(0) + 1);
            *born <= limit
        }
        Err(_) => now.mtime_s == then.mtime_s && now.mtime_ns == then.mtime_ns,
    }
}

fn undo_item(
    connection: &Connection,
    root: &Path,
    info: &ActionInfo,
    item: &Row,
    replaced: &mut Option<Action>,
    cancelled: &dyn Fn() -> bool,
) -> ItemResult {
    let path = &item.path;
    match item.kind {
        ItemKind::Trash => {
            let (Some(from), Some(meta)) = (&item.other, &item.meta) else {
                return ItemResult::no(format!("not restored: {}: no record", path.display()));
            };
            if !trash::exists(from) {
                // A move that never happened leaves the original in place.
                if item.state == "moving" && trash::exists(path) {
                    return ItemResult::ok(format!("{} was never moved", path.display()));
                }
                return ItemResult::no(format!(
                    "not restored: {} is no longer in the trash (it was emptied)",
                    path.display()
                ));
            }
            match restore_file(from, path, meta, cancelled) {
                Ok(notes) => ItemResult::ok(format!(
                    "restored {}{}",
                    path.display(),
                    notes_words(&notes)
                )),
                Err(error) => ItemResult::no(restore_words(&error, path)),
            }
        }
        ItemKind::Rename => {
            let Some(to) = &item.other else {
                return ItemResult::no(format!("not renamed back: {}: no record", path.display()));
            };
            let same = trash::meta(to)
                .ok()
                .zip(item.meta.as_ref())
                .is_some_and(|(now, then)| {
                    let born = fs::symlink_metadata(to).and_then(|data| data.created());
                    is_same_file(&now, then, &born, info.at_ms)
                });
            if !same {
                return ItemResult::no(format!(
                    "not renamed back: {} is not the file that was moved there",
                    to.display()
                ));
            }
            if trash::exists(path) {
                return ItemResult::no(format!("not renamed back: {} exists now", path.display()));
            }
            match trash::move_to(to, path, cancelled) {
                Ok(_) => ItemResult::ok(format!(
                    "renamed {} back to {}",
                    to.display(),
                    path.display()
                )),
                Err(error) => ItemResult::no(format!(
                    "not renamed back: {}: {}",
                    to.display(),
                    error.error()
                )),
            }
        }
        ItemKind::Mkdir => match fs::remove_dir(path) {
            Ok(()) => ItemResult::ok(format!("removed the folder {}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                ItemResult::ok(format!("the folder {} was gone already", path.display()))
            }
            Err(error) => ItemResult::no(format!(
                "folder {} not removed: {}",
                path.display(),
                if error.raw_os_error() == Some(39) {
                    "it is not empty".to_owned()
                } else {
                    error.to_string()
                }
            )),
        },
        ItemKind::Write | ItemKind::Created => {
            // What is there now goes to the trash first: the undo loses nothing.
            if trash::exists(path) {
                let action = match replaced {
                    Some(action) => action,
                    None => match begin(
                        connection,
                        root,
                        &info.cwd,
                        &format!("undo #{}", info.id),
                        Origin::Undo,
                    ) {
                        Ok(action) => replaced.insert(action),
                        Err(error) => return ItemResult::no(format!("not undone: {error}")),
                    },
                };
                if let Err(error) = trash_item(connection, action, path, cancelled) {
                    return ItemResult::no(format!("not undone: {error}"));
                }
            }
            if item.kind == ItemKind::Created {
                return ItemResult::ok(format!(
                    "removed {} (the new file is in the trash)",
                    path.display()
                ));
            }
            let (Some(backup), Some(meta)) = (&item.other, &item.meta) else {
                return ItemResult::no(format!("not restored: {}: no record", path.display()));
            };
            if !trash::exists(backup) {
                return ItemResult::no(format!(
                    "not restored: the old content of {} is no longer in the trash",
                    path.display()
                ));
            }
            match restore_file(backup, path, meta, cancelled) {
                Ok(notes) => ItemResult::ok(format!(
                    "restored the old content of {}{}",
                    path.display(),
                    notes_words(&notes)
                )),
                Err(error) => ItemResult::no(restore_words(&error, path)),
            }
        }
    }
}

/// Used by tests and by `omsh-cli`: does `command` fit on a line of a listing.
#[must_use]
pub fn one_line(command: &str, width: usize) -> String {
    let flat: String = command
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if flat.chars().count() <= width {
        return flat;
    }
    let mut cut: String = flat.chars().take(width).collect();
    cut.push_str("...");
    cut
}

#[cfg(test)]
mod tests;
