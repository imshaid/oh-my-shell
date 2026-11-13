//! The history database and the thread that writes to it.
//!
//! **Two connections.** SQLite in WAL mode lets many readers and one writer work at the same
//! time, also from different processes. A [`Store`] has a connection for writing, owned by its
//! writer thread, and one for reading, behind a lock, used by whoever asks ([`Store::query`]).
//!
//! **The writer thread.** The shell never writes to the database itself. It puts a message on
//! a channel ([`Store::start`], [`Store::finish`], ...) and goes on; the thread takes every
//! message that is waiting and writes them in one transaction. A slow disk or a database that
//! another shell is writing to therefore cannot hold up a command.
//!
//! **Settling.** [`Store::settle`] waits, for a limited time, until the thread has written
//! everything that was queued. Whoever needs to see what was just written (the `history`
//! listing, the Up arrow) settles first, so the command that just ran is always there; the
//! limit keeps a stuck database from freezing the shell. The prompt itself never waits: a
//! person (or a program) may type ahead the moment a command ends, and a keystroke that
//! arrives before the editor is reading can be lost.
//!
//! **In memory.** [`Store::in_memory`] keeps the history in memory only (`--no-config`, or no
//! usable data directory). There is one connection then, shared by the thread and the readers.
//! A store whose file cannot be used falls back to this and says why once
//! ([`Store::take_problem`]).

use std::collections::HashMap;
use std::fs::{DirBuilder, Permissions};
use std::hash::{BuildHasher, RandomState};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rusqlite::types::Value;
use rusqlite::{
    Connection, ErrorCode, OpenFlags, Transaction, TransactionBehavior, params, params_from_iter,
};

/// How long the shell waits for the writer when it needs to see what it has just written (a
/// listing, the Up arrow). Usually well under a millisecond. The limit is for a database that
/// is slow to open (a new file on a slow disk: several syncs) or that another shell holds; it
/// is as long as the writer itself keeps trying to open it, so that a shell that is not
/// held up for ever never answers from a database it has not opened yet.
pub const SETTLE: Duration = WRITE_PATIENCE;

/// The version of the tables, kept in SQLite's `user_version`.
const SCHEMA_VERSION: i64 = 1;

/// How long a write waits for another process that holds the database.
const WRITE_PATIENCE: Duration = Duration::from_secs(2);

/// The pause between two tries to open a database that is busy.
const OPEN_RETRY: Duration = Duration::from_millis(10);

/// How long a read waits. Short: a read happens while a person waits for the answer.
const READ_PATIENCE: Duration = Duration::from_millis(200);

/// How long [`Store::delete`] waits for the writer's answer.
const DELETE_PATIENCE: Duration = Duration::from_secs(3);

const CREATE: &str = "
    CREATE TABLE IF NOT EXISTS history (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        command     TEXT    NOT NULL,
        cwd         TEXT    NOT NULL,
        exit_status INTEGER,
        started_at  INTEGER NOT NULL,
        duration_ms INTEGER,
        session_id  INTEGER NOT NULL,
        host        TEXT    NOT NULL
    );
    CREATE INDEX IF NOT EXISTS history_cwd ON history (cwd);
    CREATE INDEX IF NOT EXISTS history_started_at ON history (started_at);
";

/// A hint looks only at this many of the newest rows, so that its cost has a ceiling whatever
/// the typed text is (blueprint §16: a keystroke is under 5 ms).
pub const HINT_WINDOW: i64 = 20_000;

/// The page cache of each connection, in KiB. SQLite's own default is 2,048 KiB per connection,
/// and with a 100,000-row history two full caches were about 4 MB of the shell's idle memory
/// (S28b). The pages are in the operating system's file cache anyway, so a smaller cache costs a
/// little copying, not disk reads. The writer touches few pages at a time; the reader scans for
/// `history search` and the explorer, so it gets more (the benchmark `query` shows the cost).
const WRITER_CACHE_KIB: i64 = 256;
const READER_CACHE_KIB: i64 = 256;

/// How many visited folders `j` remembers. The least recently visited are removed when a shell
/// starts.
const DIRS_KEPT: i64 = 1_000;

/// The table of visited folders (used by `j`). It is created next to the history table when it
/// is missing and does not change `user_version`: it only adds a table, so builds that do not
/// know it still open the file.
const CREATE_DIRS: &str = "
    CREATE TABLE IF NOT EXISTS dirs (
        path    TEXT    PRIMARY KEY,
        visits  INTEGER NOT NULL,
        last_ms INTEGER NOT NULL
    );
";

const HOUR_MS: i64 = 3_600_000;
const DAY_MS: i64 = 24 * HOUR_MS;
const WEEK_MS: i64 = 7 * DAY_MS;

const COLUMNS: &str = "id, command, cwd, exit_status, started_at, duration_ms, session_id, host";

/// One stored command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub id: i64,
    pub command: String,
    /// The working directory the command was started in.
    pub cwd: String,
    /// Missing while the command runs, and for good if the shell ended before the command did.
    pub exit_status: Option<i32>,
    /// When the command started, in milliseconds since 1970 (UTC).
    pub started_ms: i64,
    pub duration_ms: Option<i64>,
    /// The shell that ran it.
    pub session: i64,
    pub host: String,
}

/// How many of the newest runs the history explorer looks at (a count of ids, like
/// [`HINT_WINDOW`]).
pub const EXPLORER_WINDOW: usize = 10_000;

/// One distinct command with the data of its newest run, for the history explorer (see
/// [`Store::distinct_recent`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Distinct {
    /// The id of the newest run.
    pub id: i64,
    pub command: String,
    /// Where the newest run started.
    pub cwd: String,
    /// How the newest run ended; missing while it runs.
    pub exit_status: Option<i32>,
    /// When the newest run started, in milliseconds since 1970 (UTC).
    pub started_ms: i64,
    /// How many times this exact command is in the history.
    pub runs: u64,
}

/// A hint could not be looked up at this moment (see [`Store::hint`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NotReady;

/// The commands a hint may suggest, best first, for one folder at one moment: what
/// [`Store::hint_ranking`] returns. The rank of a command does not depend on what is typed, so
/// the answer for any text is the first command in the list that starts with it, which is the
/// same answer [`Store::hint`] gives from the database, without asking the database.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ranking {
    commands: Vec<String>,
    /// False if the list was cut: commands beyond it exist. A text that matches nothing in a
    /// cut list is then unknown, not "nothing".
    complete: bool,
}

impl Ranking {
    /// A ranking made from `commands`, best first. `complete` says that nothing was left out.
    /// For sources other than the database, and for tests.
    #[must_use]
    pub const fn from_commands(commands: Vec<String>, complete: bool) -> Self {
        Self { commands, complete }
    }

    /// The hint for `prefix` as [`Store::hint`] would give it: `Some(Some(command))`,
    /// `Some(None)` for "nothing to suggest", or `None` when this list cannot tell (it was cut
    /// and holds no match).
    #[must_use]
    pub fn best(&self, prefix: &str) -> Option<Option<&str>> {
        if prefix.is_empty() {
            return Some(None);
        }
        let found = self
            .commands
            .iter()
            .map(String::as_str)
            .find(|command| command.len() > prefix.len() && command.starts_with(prefix));
        match found {
            Some(command) => Some(Some(command)),
            None if self.complete => Some(None),
            None => None,
        }
    }

    /// How many commands the list holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.commands.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }
}

/// [`Counters`] merge the runs of one command for the first time at this many entries.
const MERGE_RUNS_AT: usize = 4096;

/// What [`Store::hint_ranking`] keeps for a run, and then for a command: a 64-bit hash of the
/// text and three counters, 24 bytes, never the text.
#[derive(Clone, Copy)]
struct Counter {
    /// Hash of the command text (with a key that is new for each call).
    hash: u64,
    /// The id of the newest run.
    newest: i64,
    /// Four times the score of the hint rule (4, 2, 1 and 0.25 per use), to stay in integers.
    quarters: u32,
    /// Run in the folder asked about at least once.
    here: bool,
}

impl Counter {
    /// The order of the ranking, best first: run here, then the score, then the newest run.
    /// `newest` is the id of a row, so no two commands are equal.
    fn best_first(a: &Self, b: &Self) -> std::cmp::Ordering {
        (b.here, b.quarters, b.newest).cmp(&(a.here, a.quarters, a.newest))
    }
}

/// A folder the shell has been in, for `j`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dir {
    pub path: String,
    /// How many times the shell changed into it.
    pub visits: i64,
    /// The last time, in milliseconds since 1970 (UTC).
    pub last_ms: i64,
}

/// What to look for. The default finds everything, oldest first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    /// Only commands started in this directory.
    pub cwd: Option<String>,
    /// Only commands that ended with a status other than 0.
    pub failed: bool,
    /// Only commands started at or after this time (milliseconds since 1970).
    pub since_ms: Option<i64>,
    /// Only commands that contain this text.
    pub contains: Option<String>,
    /// Only commands that start with this text.
    pub prefix: Option<String>,
    /// Only commands that are exactly this text.
    pub exact: Option<String>,
    /// Only commands that contain **every** one of these words, capitals ignored. SQLite folds
    /// the case of the letters A to Z only; other letters must match as they are.
    pub words_all: Vec<String>,
    /// Only commands that contain **at least one** of these words (same rules). Ignored when
    /// empty.
    pub words_any: Vec<String>,
    /// Only commands that ended with status 0.
    pub succeeded: bool,
    /// Only commands started before this time (milliseconds since 1970).
    pub until_ms: Option<i64>,
    /// Leave out commands with the same text as the entry with this id.
    pub unlike: Option<i64>,
    /// Only what this shell ran, and what any shell ran before this one started.
    pub own_or_older: bool,
    /// Only entries with a smaller id.
    pub below_id: Option<i64>,
    /// Only entries with a larger id.
    pub above_id: Option<i64>,
    /// At most this many.
    pub limit: Option<u32>,
    /// Newest first instead of oldest first.
    pub newest_first: bool,
}

/// Names a command between [`Store::start`] and [`Store::finish`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Token(u64);

enum Message {
    Start {
        token: Token,
        command: String,
        cwd: String,
        started_ms: i64,
    },
    Finish {
        token: Token,
        exit_status: i32,
        duration_ms: i64,
    },
    Cancel(Token),
    /// The shell changed into this folder.
    Visit {
        path: String,
        at_ms: i64,
    },
    Delete {
        ids: Vec<i64>,
        /// For each id, whether an entry was removed.
        reply: Sender<Vec<bool>>,
    },
    /// Answered when everything queued before it has been dealt with.
    Settle(Sender<()>),
    /// The shell ends: close the database, then answer.
    Close(Sender<()>),
}

/// What the writer thread and the readers share.
struct Shared {
    /// The connection for reading. In memory, also the one for writing. Missing until the
    /// writer thread has opened the database.
    reader: Mutex<Option<Connection>>,
    /// Why history is not kept in the file, until someone takes it to show it.
    problem: Mutex<Option<String>>,
}

struct Inner {
    shared: Arc<Shared>,
    /// The channel to the writer thread. The thread ends when the last store is dropped.
    sender: Sender<Message>,
    session: i64,
    session_start_ms: i64,
    next_token: AtomicU64,
}

/// A handle on the history. Cheap to copy; every copy talks to the same writer thread.
#[derive(Clone)]
pub struct Store {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("session", &self.inner.session)
            .finish_non_exhaustive()
    }
}

/// Milliseconds since 1970.
#[must_use]
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
        })
}

/// A number for this shell: the time it started in nanoseconds, mixed with its process id so
/// that two shells started in the same instant differ.
fn new_session() -> i64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    let mixed = nanos ^ (u128::from(std::process::id()) << 64);
    // Folded to 63 bits: SQLite's integers are signed.
    let folded = (mixed ^ (mixed >> 64)) & u128::from(u64::MAX >> 1);
    i64::try_from(folded).unwrap_or(0)
}

/// The name of this machine. Linux keeps it in a file (the shell is Linux only).
fn host_name() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|name| name.trim().to_owned())
        .unwrap_or_default()
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Store {
    /// History in the file `path`, which is created if missing (with its directory), keeping
    /// at most `size` commands. Nothing is opened here: the writer thread does it, so the
    /// caller does not wait. If the file cannot be used, the history is kept in memory and
    /// [`Store::take_problem`] says why.
    #[must_use]
    pub fn open(path: &Path, size: u32) -> Self {
        Self::spawn(Some(path.to_owned()), size)
    }

    /// History that is kept in memory and gone when the shell ends.
    #[must_use]
    pub fn in_memory() -> Self {
        Self::spawn(None, u32::MAX)
    }

    fn spawn(path: Option<PathBuf>, size: u32) -> Self {
        let shared = Arc::new(Shared {
            reader: Mutex::new(None),
            problem: Mutex::new(None),
        });
        let (sender, receiver) = mpsc::channel();
        let session = new_session();
        let writer = Writer {
            shared: Arc::clone(&shared),
            own: None,
            rows: HashMap::new(),
            session,
            host: host_name(),
        };
        let spawned = thread::Builder::new()
            .name("history-writer".to_owned())
            .spawn(move || writer.run(path.as_deref(), size, &receiver));
        if let Err(error) = spawned {
            // Without the thread nothing is stored and every query is empty.
            *lock(&shared.problem) = Some(format!("cannot start the history writer: {error}"));
        }
        Self {
            inner: Arc::new(Inner {
                shared,
                sender,
                session,
                session_start_ms: now_ms(),
                next_token: AtomicU64::new(1),
            }),
        }
    }

    /// The number that marks this shell's entries.
    #[must_use]
    pub fn session(&self) -> i64 {
        self.inner.session
    }

    /// Why the history is not kept in its file, if that is so. Given out once.
    #[must_use]
    pub fn take_problem(&self) -> Option<String> {
        lock(&self.inner.shared.problem).take()
    }

    /// Notes that `command` starts now, in the directory `cwd`. The entry has no status until
    /// [`Store::finish`].
    pub fn start(&self, command: &str, cwd: &str) -> Token {
        let token = Token(self.inner.next_token.fetch_add(1, Ordering::Relaxed));
        self.send(Message::Start {
            token,
            command: command.to_owned(),
            cwd: cwd.to_owned(),
            started_ms: now_ms(),
        });
        token
    }

    /// Notes how the command ended and how long it took.
    pub fn finish(&self, token: Token, exit_status: i32, duration: Duration) {
        self.send(Message::Finish {
            token,
            exit_status,
            duration_ms: i64::try_from(duration.as_millis()).unwrap_or(i64::MAX),
        });
    }

    /// Takes the entry back: the command did not run after all.
    pub fn cancel(&self, token: Token) {
        self.send(Message::Cancel(token));
    }

    /// Removes the entries with these ids. The answer says for each id whether there was such
    /// an entry; `None` if the writer did not answer in time.
    #[must_use]
    pub fn delete(&self, ids: &[i64]) -> Option<Vec<bool>> {
        let (reply, answer) = mpsc::channel();
        self.send(Message::Delete {
            ids: ids.to_vec(),
            reply,
        });
        answer.recv_timeout(DELETE_PATIENCE).ok()
    }

    /// Waits until everything queued so far is written, but no longer than `limit`. True if it
    /// is.
    pub fn settle(&self, limit: Duration) -> bool {
        let (reply, answer) = mpsc::channel();
        self.send(Message::Settle(reply));
        answer.recv_timeout(limit).is_ok()
    }

    /// The shell ends. Writes what is queued and closes the database, waiting no longer than
    /// `limit`. When the last shell closes it, SQLite folds its write-ahead log into the file,
    /// so that a single file is left. After this the store answers with nothing.
    pub fn close(&self, limit: Duration) -> bool {
        let (reply, answer) = mpsc::channel();
        self.send(Message::Close(reply));
        answer.recv_timeout(limit).is_ok()
    }

    fn send(&self, message: Message) {
        // The thread is gone only if it could not be started or it panicked; history is then
        // simply not kept.
        let _ = self.inner.sender.send(message);
    }

    /// Notes that the shell is now in the folder `path` (for `j`).
    pub fn visit(&self, path: &str) {
        self.send(Message::Visit {
            path: path.to_owned(),
            at_ms: now_ms(),
        });
    }

    /// Every remembered folder, in no particular order. Empty if the database cannot be read.
    #[must_use]
    pub fn directories(&self) -> Vec<Dir> {
        let guard = lock(&self.inner.shared.reader);
        let Some(connection) = guard.as_ref() else {
            return Vec::new();
        };
        let read = || -> rusqlite::Result<Vec<Dir>> {
            let mut statement = connection.prepare("SELECT path, visits, last_ms FROM dirs")?;
            let rows = statement.query_map([], |row| {
                Ok(Dir {
                    path: row.get(0)?,
                    visits: row.get(1)?,
                    last_ms: row.get(2)?,
                })
            })?;
            rows.collect()
        };
        read().unwrap_or_default()
    }

    /// The command to suggest for the typed text `prefix` (fish-style hint): `Ok(Some(..))`, or
    /// `Ok(None)` when the history has nothing for it, or `Err(NotReady)` when it cannot be
    /// looked up at this moment (the database is not open yet, or could not be read). The
    /// difference matters to the caller: "nothing" can be remembered, "not ready" cannot.
    ///
    /// Looks at the newest [`HINT_WINDOW`] rows of every shell. A command that starts with
    /// `prefix` and is longer than it is a candidate, unless it has several lines or ended
    /// with status 127 (a mistyped name). The best candidate is one that was run in `cwd`;
    /// among those, the one with the best score for how often and how recently it ran (4 for
    /// each use in the last hour, 2 in the last day, 1 in the last week, 0.25 before), then the
    /// newest. `now_ms` is a parameter so that tests can fix the time. This does not wait for
    /// the writer: a command that was just run may still be on its way ([`Store::settle`]).
    ///
    /// # Errors
    ///
    /// [`NotReady`] as described above.
    pub fn hint(&self, prefix: &str, cwd: &str, now_ms: i64) -> Result<Option<String>, NotReady> {
        if prefix.is_empty() {
            return Ok(None);
        }
        let guard = lock(&self.inner.shared.reader);
        let connection = guard.as_ref().ok_or(NotReady)?;
        let values = [
            Value::Integer(HINT_WINDOW),
            Value::Text(prefix.to_owned()),
            Value::Text(prefix.to_owned()),
            Value::Text(cwd.to_owned()),
            Value::Integer(now_ms.saturating_sub(HOUR_MS)),
            Value::Integer(now_ms.saturating_sub(DAY_MS)),
            Value::Integer(now_ms.saturating_sub(WEEK_MS)),
        ];
        let found = connection.query_row(
            "SELECT command FROM history \
             WHERE id > (SELECT max(id) FROM history) - ? \
               AND instr(command, ?) = 1 AND command <> ? \
               AND instr(command, char(10)) = 0 \
               AND (exit_status IS NULL OR exit_status <> 127) \
             GROUP BY command \
             ORDER BY max(cwd = ?) DESC, \
               sum(CASE WHEN started_at >= ? THEN 4.0 WHEN started_at >= ? THEN 2.0 \
                        WHEN started_at >= ? THEN 1.0 ELSE 0.25 END) DESC, \
               max(id) DESC \
             LIMIT 1",
            params_from_iter(values.iter()),
            |row| row.get::<_, String>(0),
        );
        match found {
            Ok(command) => Ok(Some(command)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            // Busy, locked, or a read that failed: no answer is not the same as "nothing".
            Err(_) => Err(NotReady),
        }
    }

    /// The best `limit` commands for hints in `cwd`, in the order of [`Store::hint`]: the same
    /// rows (the newest [`HINT_WINDOW`], no several-line commands, no status 127), the same
    /// groups and the same order, with no text to match. One question that costs as much as the
    /// slowest `hint` (it reads the whole window), asked once for a prompt, so that the first
    /// keys of that prompt are answered from memory.
    ///
    /// The grouping is done here, in one pass over the rows, and not by SQL's `GROUP BY`: SQLite
    /// sorts every row of a `GROUP BY` first, and with a window of 20,000 rows that sorter (and the
    /// memory the allocator then kept) was most of the idle memory of a shell with a long history
    /// (S28b: 18 MB before, 11.5 MB after). Only one entry per distinct command is held. The
    /// scores are kept as quarters (16, 8, 4 and 1 for the 4, 2, 1 and 0.25 of [`Store::hint`]),
    /// which are exact, so the order is the same as the database question gives.
    ///
    /// # Errors
    ///
    /// [`NotReady`] when the history cannot be read at this moment.
    pub fn hint_ranking(&self, cwd: &str, now_ms: i64, limit: usize) -> Result<Ranking, NotReady> {
        let guard = lock(&self.inner.shared.reader);
        let connection = guard.as_ref().ok_or(NotReady)?;
        // Memory is the point of this function (S28b). SQL's `GROUP BY` sorts all the rows of the
        // window (20,000 rows of text); a map from text to counters holds every distinct text.
        // Both made the idle shell big with a long history, and the allocator kept what they
        // used. So: (1) one pass that keeps, for each run, only a hash of its text and three
        // counters; (2) sort by hash and add the runs of one command together; (3) cut to the
        // best `limit + 1`; (4) read the text of those few by the id of their newest run. Both
        // passes see the same data (one read transaction). The order is the one the database
        // question of [`Store::hint`] gives; two different texts with the same 64-bit hash would
        // be taken for one command (a chance of about 1 in 10^10 for 20,000 distinct commands,
        // and the key of the hash differs from call to call).
        let read = || -> rusqlite::Result<Vec<String>> {
            let snapshot = begin_read(connection)?;
            let (hour, day, week) = (
                now_ms.saturating_sub(HOUR_MS),
                now_ms.saturating_sub(DAY_MS),
                now_ms.saturating_sub(WEEK_MS),
            );
            let key = RandomState::new();
            let mut counters = Counters::new(
                MERGE_RUNS_AT,
                usize::try_from(HINT_WINDOW).unwrap_or(MERGE_RUNS_AT),
            );
            {
                let mut statement = snapshot.prepare(
                    "SELECT command, cwd = ?, started_at, id FROM history \
                     WHERE id > (SELECT max(id) FROM history) - ? \
                       AND instr(command, char(10)) = 0 \
                       AND (exit_status IS NULL OR exit_status <> 127)",
                )?;
                let mut rows = statement.query(params![cwd, HINT_WINDOW])?;
                while let Some(row) = rows.next()? {
                    let started: i64 = row.get(2)?;
                    counters.push(Counter {
                        hash: key.hash_one(row.get_ref(0)?.as_str()?),
                        newest: row.get(3)?,
                        quarters: if started >= hour {
                            16
                        } else if started >= day {
                            8
                        } else if started >= week {
                            4
                        } else {
                            1
                        },
                        here: row.get(1)?,
                    });
                }
            }
            let counters = counters.best(limit.saturating_add(1));
            let mut text = snapshot.prepare("SELECT command FROM history WHERE id = ?")?;
            let mut commands = Vec::with_capacity(counters.len());
            for counter in &counters {
                commands.push(text.query_row([counter.newest], |row| row.get::<_, String>(0))?);
            }
            Ok(commands)
        };
        let mut commands = read().map_err(|_| NotReady)?;
        let complete = commands.len() <= limit;
        commands.truncate(limit);
        Ok(Ranking { commands, complete })
    }

    /// The distinct commands among the newest `window` runs (of every shell), newest first, each
    /// with the folder, status and time of its newest run and the number of runs *inside the
    /// window*. One row per command text. The window bounds the cost: grouping the whole table
    /// of 100,000 rows took 94 ms, [`EXPLORER_WINDOW`] runs take a few. This does not wait for
    /// the writer ([`Store::settle`]).
    ///
    /// # Errors
    ///
    /// [`NotReady`] when the history cannot be read at this moment.
    pub fn distinct_recent(&self, window: usize) -> Result<Vec<Distinct>, NotReady> {
        let guard = lock(&self.inner.shared.reader);
        let connection = guard.as_ref().ok_or(NotReady)?;
        let asked = i64::try_from(window).unwrap_or(i64::MAX);
        let read = || -> rusqlite::Result<Vec<Distinct>> {
            // With `max(id)` as the one min/max aggregate, SQLite takes the bare columns from
            // the row that holds the maximum: the newest run.
            let mut statement = connection.prepare(
                "SELECT max(id), command, cwd, exit_status, started_at, count(*) FROM history \
                 WHERE id > (SELECT max(id) FROM history) - ? \
                 GROUP BY command ORDER BY max(id) DESC",
            )?;
            let rows = statement.query_map([asked], |row| {
                Ok(Distinct {
                    id: row.get(0)?,
                    command: row.get(1)?,
                    cwd: row.get(2)?,
                    exit_status: row.get(3)?,
                    started_ms: row.get(4)?,
                    runs: u64::try_from(row.get::<_, i64>(5)?).unwrap_or(0),
                })
            })?;
            rows.collect()
        };
        read().map_err(|_| NotReady)
    }

    /// The entries that fit `query`. Empty if the database cannot be read right now: history
    /// must never stop the shell.
    #[must_use]
    pub fn query(&self, query: &Query) -> Vec<Entry> {
        let (conditions, mut values) = self.conditions(query);
        let order = if query.newest_first { "DESC" } else { "ASC" };
        let mut sql =
            format!("SELECT {COLUMNS} FROM history WHERE {conditions} ORDER BY id {order}");
        if let Some(limit) = query.limit {
            sql.push_str(" LIMIT ?");
            values.push(Value::Integer(i64::from(limit)));
        }
        let guard = lock(&self.inner.shared.reader);
        let Some(connection) = guard.as_ref() else {
            return Vec::new();
        };
        read_entries(connection, &sql, &values).unwrap_or_default()
    }

    /// Hands the entries that fit `query` to `each`, one after the other, in the order and
    /// within the limit of the query, and stops when `each` returns `false`. Unlike [`Store::query`]
    /// the whole answer is never in memory: the rows are read in pages of 2,000 by their ids, and
    /// the reader is free between two pages, so a slow consumer (a pager) does not hold the
    /// database. `below_id` and `above_id` of the query are kept; the paging moves the one on the
    /// side the walk comes from.
    pub fn query_each(&self, query: &Query, mut each: impl FnMut(Entry) -> bool) {
        const PAGE: u32 = 2_000;
        let mut page_query = query.clone();
        let mut left = query.limit;
        loop {
            let size = left.map_or(PAGE, |left| left.min(PAGE));
            if size == 0 {
                return;
            }
            page_query.limit = Some(size);
            let rows = self.query(&page_query);
            let count = u32::try_from(rows.len()).unwrap_or(u32::MAX);
            let Some(last) = rows.last().map(|entry| entry.id) else {
                return;
            };
            for entry in rows {
                if !each(entry) {
                    return;
                }
            }
            if count < size {
                return;
            }
            if query.newest_first {
                page_query.below_id = Some(last);
            } else {
                page_query.above_id = Some(last);
            }
            left = left.map(|left| left - count);
        }
    }

    /// How many entries fit `query` (its limit and order do not count).
    #[must_use]
    pub fn count(&self, query: &Query) -> u64 {
        let (conditions, values) = self.conditions(query);
        let sql = format!("SELECT count(*) FROM history WHERE {conditions}");
        let guard = lock(&self.inner.shared.reader);
        let Some(connection) = guard.as_ref() else {
            return 0;
        };
        connection
            .query_row(&sql, params_from_iter(values.iter()), |row| {
                row.get::<_, i64>(0)
            })
            .map_or(0, |count| u64::try_from(count).unwrap_or(0))
    }

    /// The entry with this id.
    #[must_use]
    pub fn get(&self, id: i64) -> Option<Entry> {
        let guard = lock(&self.inner.shared.reader);
        let connection = guard.as_ref()?;
        let sql = format!("SELECT {COLUMNS} FROM history WHERE id = ?");
        read_entries(connection, &sql, &[Value::Integer(id)])
            .ok()?
            .pop()
    }

    /// The `WHERE` part for `query` and the values of its `?` marks, in order.
    fn conditions(&self, query: &Query) -> (String, Vec<Value>) {
        let mut parts: Vec<String> = vec!["1".to_owned()];
        let mut values = Vec::new();
        let mut text = |parts: &mut Vec<String>, condition: &str, value: &Option<String>| {
            if let Some(value) = value {
                parts.push(condition.to_owned());
                values.push(Value::Text(value.clone()));
            }
        };
        text(&mut parts, "cwd = ?", &query.cwd);
        text(&mut parts, "instr(command, ?) > 0", &query.contains);
        text(&mut parts, "instr(command, ?) = 1", &query.prefix);
        text(&mut parts, "command = ?", &query.exact);
        if query.failed {
            parts.push("exit_status IS NOT NULL AND exit_status <> 0".to_owned());
        }
        if query.succeeded {
            parts.push("exit_status = 0".to_owned());
        }
        for word in &query.words_all {
            parts.push("instr(lower(command), lower(?)) > 0".to_owned());
            values.push(Value::Text(word.clone()));
        }
        if !query.words_any.is_empty() {
            let one = "instr(lower(command), lower(?)) > 0";
            parts.push(format!(
                "({})",
                vec![one; query.words_any.len()].join(" OR ")
            ));
            values.extend(query.words_any.iter().cloned().map(Value::Text));
        }
        let mut number = |parts: &mut Vec<String>, condition: &str, value: Option<i64>| {
            if let Some(value) = value {
                parts.push(condition.to_owned());
                values.push(Value::Integer(value));
            }
        };
        number(&mut parts, "started_at >= ?", query.since_ms);
        number(&mut parts, "started_at < ?", query.until_ms);
        number(
            &mut parts,
            "command <> ifnull((SELECT command FROM history WHERE id = ?), '')",
            query.unlike,
        );
        number(&mut parts, "id < ?", query.below_id);
        number(&mut parts, "id > ?", query.above_id);
        if query.own_or_older {
            parts.push("(session_id = ? OR started_at < ?)".to_owned());
            values.push(Value::Integer(self.inner.session));
            values.push(Value::Integer(self.inner.session_start_ms));
        }
        (parts.join(" AND "), values)
    }
}

fn read_entries(
    connection: &Connection,
    sql: &str,
    values: &[Value],
) -> rusqlite::Result<Vec<Entry>> {
    let mut statement = connection.prepare(sql)?;
    let rows = statement.query_map(params_from_iter(values.iter()), |row| {
        Ok(Entry {
            id: row.get(0)?,
            command: row.get(1)?,
            cwd: row.get(2)?,
            exit_status: row.get(3)?,
            started_ms: row.get(4)?,
            duration_ms: row.get(5)?,
            session: row.get(6)?,
            host: row.get(7)?,
        })
    })?;
    rows.collect()
}

/// The state of the writer thread.
struct Writer {
    shared: Arc<Shared>,
    /// The connection for writing to a file. Without it (in memory), writes go through the
    /// shared connection.
    own: Option<Connection>,
    /// The row of each command that has started and not finished.
    rows: HashMap<Token, i64>,
    session: i64,
    host: String,
}

impl Writer {
    fn run(mut self, path: Option<&Path>, size: u32, receiver: &Receiver<Message>) {
        self.open(path, size);
        // Ends when every `Store` is gone.
        while let Ok(first) = receiver.recv() {
            let mut batch = vec![first];
            batch.extend(receiver.try_iter());
            self.write(batch);
        }
    }

    fn open(&mut self, path: Option<&Path>, size: u32) {
        if let Some(path) = path {
            let started = Instant::now();
            let reason = loop {
                match open_file(path, size) {
                    Ok((writer, reader)) => {
                        self.own = Some(writer);
                        *lock(&self.shared.reader) = Some(reader);
                        return;
                    }
                    // Another shell is opening or writing the database right now. SQLite
                    // does not always wait by itself (it answers "locked" at once where
                    // waiting could mean two processes waiting for each other), so wait here.
                    Err(Failure::Busy(_)) if started.elapsed() < WRITE_PATIENCE => {
                        thread::sleep(OPEN_RETRY);
                    }
                    Err(Failure::Busy(reason) | Failure::Unusable(reason)) => break reason,
                }
            };
            *lock(&self.shared.problem) = Some(format!(
                "history is kept in memory only: {}: {reason}",
                path.display()
            ));
        }
        match open_memory() {
            Ok(connection) => *lock(&self.shared.reader) = Some(connection),
            Err(error) => {
                *lock(&self.shared.problem) = Some(format!("history is off: {error}"));
            }
        }
    }

    /// Writes one batch in one transaction, then answers those who wait.
    fn write(&mut self, batch: Vec<Message>) {
        // Those who wait for the batch get their answer whether or not the write works: they
        // wait for the queue to be dealt with, and it is.
        let mut settled = Vec::new();
        let mut closing = false;
        let mut work = Vec::with_capacity(batch.len());
        for message in batch {
            match message {
                Message::Settle(reply) => settled.push(reply),
                Message::Close(reply) => {
                    closing = true;
                    settled.push(reply);
                }
                other => work.push(other),
            }
        }
        if !work.is_empty() {
            self.apply(work);
        }
        if closing {
            // Dropping a connection closes it. The one that writes goes last: only it can
            // fold the log into the file, which SQLite does when the last connection closes.
            *lock(&self.shared.reader) = None;
            self.own = None;
        }
        for reply in settled {
            let _ = reply.send(());
        }
    }

    fn apply(&mut self, batch: Vec<Message>) {
        let mut deleted = Vec::new();
        let shared = Arc::clone(&self.shared);
        let guard;
        let connection = match &self.own {
            Some(connection) => Some(connection),
            None => {
                guard = lock(&shared.reader);
                guard.as_ref()
            }
        };
        let result = connection.map(|connection| -> rusqlite::Result<()> {
            let transaction = begin(connection)?;
            for message in batch {
                match message {
                    Message::Start {
                        token,
                        command,
                        cwd,
                        started_ms,
                    } => {
                        transaction.execute(
                            "INSERT INTO history (command, cwd, started_at, session_id, host) \
                             VALUES (?, ?, ?, ?, ?)",
                            params![command, cwd, started_ms, self.session, self.host],
                        )?;
                        self.rows.insert(token, transaction.last_insert_rowid());
                    }
                    Message::Finish {
                        token,
                        exit_status,
                        duration_ms,
                    } => {
                        if let Some(row) = self.rows.remove(&token) {
                            transaction.execute(
                                "UPDATE history SET exit_status = ?, duration_ms = ? WHERE id = ?",
                                params![exit_status, duration_ms, row],
                            )?;
                        }
                    }
                    Message::Cancel(token) => {
                        if let Some(row) = self.rows.remove(&token) {
                            transaction.execute("DELETE FROM history WHERE id = ?", [row])?;
                        }
                    }
                    Message::Visit { path, at_ms } => {
                        transaction.execute(
                            "INSERT INTO dirs (path, visits, last_ms) VALUES (?, 1, ?) \
                             ON CONFLICT(path) DO UPDATE SET visits = visits + 1, \
                             last_ms = excluded.last_ms",
                            params![path, at_ms],
                        )?;
                    }
                    Message::Delete { ids, reply } => {
                        let mut removed = Vec::with_capacity(ids.len());
                        for id in ids {
                            let count =
                                transaction.execute("DELETE FROM history WHERE id = ?", [id])?;
                            removed.push(count > 0);
                        }
                        deleted.push((reply, removed));
                    }
                    Message::Settle(_) | Message::Close(_) => {}
                }
            }
            transaction.commit()
        });
        if let Some(Err(error)) = result {
            // The batch is lost (the transaction was rolled back). Say so once.
            let mut problem = lock(&self.shared.problem);
            if problem.is_none() {
                *problem = Some(format!(
                    "some commands were not written to history: {error}"
                ));
            }
            // Nobody is told that an entry was removed when it was not.
            deleted.clear();
        }
        for (reply, removed) in deleted {
            let _ = reply.send(removed);
        }
    }
}

/// Starts a transaction that takes the right to write at once, waiting for another writer if
/// there is one. (A transaction that starts by reading and writes later can fail instead of
/// waiting.)
fn begin(connection: &Connection) -> rusqlite::Result<Transaction<'_>> {
    Transaction::new_unchecked(connection, TransactionBehavior::Immediate)
}

/// Adds up the counters of one command (same hash) in place, so that `counters` holds one per
/// command. The order of `counters` is lost.
fn merge_runs(counters: &mut Vec<Counter>) {
    // Runs of one command are next to each other once sorted by hash.
    counters.sort_unstable_by_key(|counter| counter.hash);
    let mut kept = 0;
    for next in 0..counters.len() {
        let run = counters[next];
        if kept > 0 && counters[kept - 1].hash == run.hash {
            let command = &mut counters[kept - 1];
            command.here |= run.here;
            command.quarters += run.quarters;
            command.newest = command.newest.max(run.newest);
        } else {
            counters[kept] = run;
            kept += 1;
        }
    }
    counters.truncate(kept);
}

/// From one [`Counter`] per run to one per command, the best `wanted` of them, best first: runs
/// with the same hash are added together, then the rest is cut with a selection, not a full sort
/// (the text of each one kept is read afterwards, so the cut is what keeps that read small).
fn best_commands(mut counters: Vec<Counter>, wanted: usize) -> Vec<Counter> {
    merge_runs(&mut counters);
    if counters.len() > wanted {
        counters.select_nth_unstable_by(wanted - 1, Counter::best_first);
        counters.truncate(wanted);
    }
    counters.sort_unstable_by(Counter::best_first);
    counters
}

/// The counters of the runs read so far. Whenever the list is full it is merged to one entry per
/// command, so its size follows the number of different commands and not the number of runs: a
/// history that repeats the same few thousand commands keeps a list of that size.
struct Counters {
    list: Vec<Counter>,
    /// Merge when the list reaches this length.
    merge_at: usize,
    /// How often the list was merged: the tests look at it, so that merging at every run (slow,
    /// and the same answer) does not go unseen.
    #[cfg(test)]
    merges: usize,
}

impl Counters {
    /// `room` is the most the list can hold without growing (it is only address space until the
    /// list really fills it: pages that are never written to are not in memory; growing the list
    /// in steps instead left the freed smaller buffers behind in the allocator).
    fn new(merge_at: usize, room: usize) -> Self {
        Self {
            list: Vec::with_capacity(room.max(merge_at)),
            merge_at,
            #[cfg(test)]
            merges: 0,
        }
    }

    fn push(&mut self, counter: Counter) {
        self.list.push(counter);
        if self.list.len() >= self.merge_at {
            merge_runs(&mut self.list);
            #[cfg(test)]
            {
                self.merges += 1;
            }
            // Mostly different commands: more room, so that this does not merge at every run.
            if self.list.len() * 2 > self.merge_at {
                self.merge_at *= 2;
            }
        }
    }

    fn best(self, wanted: usize) -> Vec<Counter> {
        best_commands(self.list, wanted)
    }
}

/// Starts a transaction that only reads: everything asked inside it sees the database as it was at
/// the first question.
fn begin_read(connection: &Connection) -> rusqlite::Result<Transaction<'_>> {
    Transaction::new_unchecked(connection, TransactionBehavior::Deferred)
}

/// Opens (and creates) the database file: a connection to write with, one to read with.
fn open_file(path: &Path, size: u32) -> Result<(Connection, Connection), Failure> {
    let unusable = |error: std::io::Error| Failure::Unusable(error.to_string());
    if let Some(dir) = path.parent() {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(unusable)?;
    }
    // SQLite would open a FIFO and wait for ever, so look at what is there first.
    //
    // The file is never opened here, not even to create it: closing a descriptor of a file
    // drops every lock this process holds on that file (that is how POSIX locks work), also
    // the ones SQLite took through another connection.
    match std::fs::metadata(path) {
        Ok(kind) if !kind.is_file() => {
            return Err(Failure::Unusable("not a regular file".to_owned()));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(unusable(error)),
    }
    let writer = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    // Private, like the directory. SQLite gives the log files the mode of this file.
    std::fs::set_permissions(path, Permissions::from_mode(0o600)).map_err(unusable)?;
    writer.busy_timeout(WRITE_PATIENCE)?;
    // WAL: readers do not block the writer and the writer does not block readers. It is a
    // property of the file, so it stays on for every shell that opens it.
    let mode: String = writer.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(Failure::Unusable(format!(
            "the database cannot use WAL mode (it uses {mode})"
        )));
    }
    // With WAL, NORMAL cannot corrupt the file; a power cut may lose the last commands.
    writer.pragma_update(None, "synchronous", "NORMAL")?;
    prepare(&writer)?;
    set_cache(&writer, WRITER_CACHE_KIB)?;
    prune(&writer, size)?;
    let reader = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    reader.busy_timeout(READ_PATIENCE)?;
    set_cache(&reader, READER_CACHE_KIB)?;
    Ok((writer, reader))
}

/// Limits the page cache of `connection` to `kib` KiB (see [`WRITER_CACHE_KIB`]). A negative
/// `cache_size` means KiB; a positive one means pages.
fn set_cache(connection: &Connection, kib: i64) -> rusqlite::Result<()> {
    connection.pragma_update(None, "cache_size", -kib)
}

fn open_memory() -> Result<Connection, String> {
    let connection = Connection::open_in_memory().map_err(|error| error.to_string())?;
    match prepare(&connection) {
        Ok(()) => Ok(connection),
        Err(Failure::Busy(reason) | Failure::Unusable(reason)) => Err(reason),
    }
}

/// Why a database could not be opened.
enum Failure {
    /// Someone else holds it at this moment. Trying again can work.
    Busy(String),
    /// It cannot be used as it is.
    Unusable(String),
}

impl From<rusqlite::Error> for Failure {
    fn from(error: rusqlite::Error) -> Self {
        match error.sqlite_error_code() {
            Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => {
                Self::Busy(error.to_string())
            }
            _ => Self::Unusable(error.to_string()),
        }
    }
}

/// Creates the tables in a new database and refuses one written by a later version.
fn prepare(connection: &Connection) -> Result<(), Failure> {
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version > SCHEMA_VERSION {
        return Err(Failure::Unusable(format!(
            "it was written by a newer version of oh-my-shell (format {version})"
        )));
    }
    if version < SCHEMA_VERSION {
        // One transaction: two shells that start together both wait here, and the second
        // finds the tables made.
        let transaction = begin(connection)?;
        transaction.execute_batch(CREATE)?;
        transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        transaction.commit()?;
    }
    // Added after version 1 shipped, without a new version (see `CREATE_DIRS`).
    let has_dirs: bool = connection.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'dirs')",
        [],
        |row| row.get(0),
    )?;
    if !has_dirs {
        let transaction = begin(connection)?;
        transaction.execute_batch(CREATE_DIRS)?;
        transaction.commit()?;
    }
    Ok(())
}

/// Removes the oldest entries beyond the newest `size`.
fn prune(connection: &Connection, size: u32) -> rusqlite::Result<()> {
    connection.execute(
        "DELETE FROM history WHERE id <= \
         (SELECT id FROM history ORDER BY id DESC LIMIT 1 OFFSET ?)",
        [i64::from(size)],
    )?;
    connection.execute(
        "DELETE FROM dirs WHERE path NOT IN \
         (SELECT path FROM dirs ORDER BY last_ms DESC LIMIT ?)",
        [DIRS_KEPT],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
