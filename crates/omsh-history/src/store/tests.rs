use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::AtomicU32;
use std::time::Instant;

use super::*;

/// Long enough for any write of these tests, also on a busy machine.
const PATIENT: Duration = Duration::from_secs(10);

/// A directory of its own for one test, removed when the test ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("omsh-history-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn db(&self) -> PathBuf {
        self.0.join("data").join("history.db")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Runs `command` to its end in `store`.
fn record(store: &Store, command: &str, cwd: &str, status: i32) {
    let token = store.start(command, cwd);
    store.finish(token, status, Duration::from_millis(7));
}

fn commands(entries: &[Entry]) -> Vec<&str> {
    entries.iter().map(|entry| entry.command.as_str()).collect()
}

fn all(store: &Store) -> Vec<Entry> {
    assert!(store.settle(PATIENT));
    store.query(&Query::default())
}

/// A look at the file with a connection of the test's own.
fn pragma(path: &Path, name: &str) -> String {
    let connection = Connection::open(path).unwrap();
    connection
        .query_row(&format!("PRAGMA {name}"), [], |row| row.get::<_, Value>(0))
        .map(|value| match value {
            Value::Text(text) => text,
            Value::Integer(n) => n.to_string(),
            other => format!("{other:?}"),
        })
        .unwrap()
}

#[test]
fn a_command_is_stored_with_everything_about_it() {
    let store = Store::in_memory();
    let before = now_ms();
    let token = store.start("echo hi", "/tmp/x");
    store.finish(token, 3, Duration::from_millis(1234));
    let entries = all(&store);
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert_eq!(entry.command, "echo hi");
    assert_eq!(entry.cwd, "/tmp/x");
    assert_eq!(entry.exit_status, Some(3));
    assert_eq!(entry.duration_ms, Some(1234));
    assert_eq!(entry.session, store.session());
    assert!(entry.started_ms >= before && entry.started_ms <= now_ms());
    assert_eq!(entry.host, host_name());
    assert!(!entry.host.is_empty());
    assert_eq!(store.get(entry.id).as_ref(), Some(entry));
    assert_eq!(store.get(entry.id + 1), None);
    assert_eq!(store.take_problem(), None);
}

#[test]
fn a_command_that_has_not_ended_is_there_without_a_status() {
    let store = Store::in_memory();
    let token = store.start("sleep 100", "/");
    let entries = all(&store);
    assert_eq!(commands(&entries), ["sleep 100"]);
    assert_eq!(entries[0].exit_status, None);
    assert_eq!(entries[0].duration_ms, None);
    store.finish(token, 0, Duration::ZERO);
    assert_eq!(all(&store)[0].exit_status, Some(0));
}

#[test]
fn a_cancelled_command_is_gone() {
    let store = Store::in_memory();
    record(&store, "one", "/", 0);
    let token = store.start("if true; then", "/");
    // Once written, once still queued with its start.
    assert!(store.settle(PATIENT));
    store.cancel(token);
    let token = store.start("while :; do", "/");
    store.cancel(token);
    // Finishing a cancelled command changes nothing.
    store.finish(token, 0, Duration::ZERO);
    record(&store, "two", "/", 0);
    assert_eq!(commands(&all(&store)), ["one", "two"]);
}

#[test]
fn history_in_a_file_survives_a_restart() {
    let scratch = Scratch::new();
    {
        let store = Store::open(&scratch.db(), 100);
        record(&store, "first", "/a", 0);
        record(&store, "second", "/b", 1);
        assert!(store.settle(PATIENT));
        assert_eq!(store.take_problem(), None);
    }
    let store = Store::open(&scratch.db(), 100);
    let entries = all(&store);
    assert_eq!(commands(&entries), ["first", "second"]);
    assert_eq!(entries[1].exit_status, Some(1));
    assert_ne!(entries[0].session, store.session());
    record(&store, "third", "/c", 0);
    let entries = all(&store);
    assert_eq!(commands(&entries), ["first", "second", "third"]);
    assert!(entries[2].id > entries[1].id);
}

#[test]
fn the_file_is_private_and_in_wal_mode() {
    let scratch = Scratch::new();
    let store = Store::open(&scratch.db(), 100);
    record(&store, "x", "/", 0);
    assert!(store.settle(PATIENT));
    let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&scratch.db()), 0o600);
    assert_eq!(mode(scratch.db().parent().unwrap()), 0o700);
    assert_eq!(pragma(&scratch.db(), "journal_mode"), "wal");
    assert_eq!(pragma(&scratch.db(), "user_version"), "1");
    let connection = Connection::open(scratch.db()).unwrap();
    let indexes: Vec<String> = connection
        .prepare(
            "SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = 'history' \
             ORDER BY name",
        )
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(indexes, ["history_cwd", "history_started_at"]);
}

#[test]
fn only_the_newest_commands_are_kept_at_the_next_start() {
    let scratch = Scratch::new();
    {
        let store = Store::open(&scratch.db(), 100);
        for n in 0..10 {
            record(&store, &format!("c{n}"), "/", 0);
        }
        assert_eq!(all(&store).len(), 10);
    }
    let store = Store::open(&scratch.db(), 3);
    assert_eq!(commands(&all(&store)), ["c7", "c8", "c9"]);
    drop(store);
    // A size the history has not reached removes nothing.
    let store = Store::open(&scratch.db(), 3);
    assert_eq!(all(&store).len(), 3);
}

/// Seven commands for the query tests.
fn sample() -> Store {
    let store = Store::in_memory();
    record(&store, "ls", "/home", 0);
    record(&store, "git status", "/repo", 0);
    record(&store, "git push", "/repo", 1);
    record(&store, "ls", "/repo", 0);
    record(&store, "make", "/repo", 2);
    record(&store, "ls -l", "/home", 0);
    let _ = store.start("vim notes", "/home");
    assert!(store.settle(PATIENT));
    store
}

#[test]
fn queries_filter_by_directory_failure_and_text() {
    let store = sample();
    let find = |query: Query| {
        store
            .query(&query)
            .into_iter()
            .map(|entry| entry.command)
            .collect::<Vec<_>>()
    };
    let here = Query {
        cwd: Some("/repo".to_owned()),
        ..Query::default()
    };
    assert_eq!(find(here.clone()), ["git status", "git push", "ls", "make"]);
    // A command that has not ended has not failed.
    let failed = Query {
        failed: true,
        ..Query::default()
    };
    assert_eq!(find(failed), ["git push", "make"]);
    let contains = Query {
        contains: Some("s".to_owned()),
        ..Query::default()
    };
    assert_eq!(
        find(contains),
        ["ls", "git status", "git push", "ls", "ls -l", "vim notes"]
    );
    let prefix = Query {
        prefix: Some("git".to_owned()),
        ..Query::default()
    };
    assert_eq!(find(prefix), ["git status", "git push"]);
    let exact = Query {
        exact: Some("ls".to_owned()),
        ..Query::default()
    };
    assert_eq!(find(exact), ["ls", "ls"]);
    // Filters combine.
    let both = Query {
        failed: true,
        contains: Some("git".to_owned()),
        ..here
    };
    assert_eq!(find(both.clone()), ["git push"]);
    assert_eq!(store.count(&both), 1);
    assert_eq!(store.count(&Query::default()), 7);
    // Text is compared as it is: no wildcards, capitals differ.
    for text in ["%", "_", "LS"] {
        let query = Query {
            contains: Some(text.to_owned()),
            ..Query::default()
        };
        assert_eq!(find(query), Vec::<String>::new(), "{text}");
    }
}

fn found(store: &Store, query: Query) -> Vec<String> {
    store
        .query(&query)
        .into_iter()
        .map(|entry| entry.command)
        .collect()
}

#[test]
fn words_must_all_be_there_whatever_their_case() {
    let store = Store::in_memory();
    record(&store, "Docker compose up", "/a", 0);
    record(&store, "docker ps", "/a", 0);
    record(&store, "echo compose", "/a", 0);
    assert!(store.settle(PATIENT));
    let both = Query {
        words_all: vec!["DOCKER".to_owned(), "Compose".to_owned()],
        ..Query::default()
    };
    assert_eq!(found(&store, both.clone()), ["Docker compose up"]);
    assert_eq!(store.count(&both), 1);
    // No words at all means no condition.
    assert_eq!(found(&store, Query::default()).len(), 3);
    // Wildcard characters are text, not patterns.
    let percent = Query {
        words_all: vec!["%".to_owned()],
        ..Query::default()
    };
    assert_eq!(found(&store, percent), Vec::<String>::new());
}

#[test]
fn any_word_is_enough_and_an_empty_list_is_no_condition() {
    let store = Store::in_memory();
    record(&store, "git push", "/a", 0);
    record(&store, "svn commit", "/a", 0);
    record(&store, "ls", "/a", 0);
    assert!(store.settle(PATIENT));
    let either = Query {
        words_any: vec!["GIT".to_owned(), "svn".to_owned()],
        ..Query::default()
    };
    assert_eq!(found(&store, either), ["git push", "svn commit"]);
    // `all` and `any` combine: both must hold.
    let combined = Query {
        words_all: vec!["push".to_owned()],
        words_any: vec!["git".to_owned(), "svn".to_owned()],
        ..Query::default()
    };
    assert_eq!(found(&store, combined), ["git push"]);
    let empty = Query {
        words_any: Vec::new(),
        ..Query::default()
    };
    assert_eq!(found(&store, empty).len(), 3);
}

#[test]
fn succeeded_means_status_zero_and_not_unfinished() {
    let store = sample();
    let ok = Query {
        succeeded: true,
        ..Query::default()
    };
    // `vim notes` has not ended, so it has not succeeded; `git push` and `make` failed.
    assert_eq!(found(&store, ok), ["ls", "git status", "ls", "ls -l"]);
}

#[test]
fn the_time_window_has_an_inclusive_start_and_an_exclusive_end() {
    let store = Store::in_memory();
    record(&store, "one", "/", 0);
    assert!(store.settle(PATIENT));
    thread::sleep(Duration::from_millis(30));
    record(&store, "two", "/", 0);
    assert!(store.settle(PATIENT));
    // The start time of the second command, exactly: it is the edge of the window.
    let edge = store.query(&Query::default())[1].started_ms;
    assert_eq!(store.query(&Query::default())[1].command, "two");
    let before = Query {
        until_ms: Some(edge),
        ..Query::default()
    };
    assert_eq!(found(&store, before), ["one"], "the end is not included");
    let from = Query {
        since_ms: Some(edge),
        ..Query::default()
    };
    assert_eq!(found(&store, from), ["two"], "the start is included");
    let nothing = Query {
        since_ms: Some(edge),
        until_ms: Some(edge),
        ..Query::default()
    };
    assert_eq!(found(&store, nothing), Vec::<String>::new());
    let one_ms = Query {
        since_ms: Some(edge),
        until_ms: Some(edge + 1),
        ..Query::default()
    };
    assert_eq!(found(&store, one_ms), ["two"]);
}

#[test]
fn a_directory_filter_means_that_folder_and_not_its_subfolders() {
    let store = Store::in_memory();
    record(&store, "in-dir", "/work/proj", 0);
    record(&store, "in-sub", "/work/proj/sub", 0);
    record(&store, "in-sibling", "/work/proj2", 0);
    assert!(store.settle(PATIENT));
    let here = Query {
        cwd: Some("/work/proj".to_owned()),
        ..Query::default()
    };
    assert_eq!(found(&store, here), ["in-dir"]);
}

#[test]
fn queries_page_by_id_order_and_limit() {
    let store = sample();
    let ids: Vec<i64> = store
        .query(&Query::default())
        .iter()
        .map(|e| e.id)
        .collect();
    assert_eq!(ids.len(), 7);
    assert!(ids.windows(2).all(|pair| pair[0] < pair[1]));
    let newest = Query {
        newest_first: true,
        limit: Some(2),
        ..Query::default()
    };
    assert_eq!(commands(&store.query(&newest)), ["vim notes", "ls -l"]);
    let below = Query {
        below_id: Some(ids[2]),
        newest_first: true,
        ..Query::default()
    };
    assert_eq!(commands(&store.query(&below)), ["git status", "ls"]);
    let above = Query {
        above_id: Some(ids[4]),
        ..Query::default()
    };
    assert_eq!(commands(&store.query(&above)), ["ls -l", "vim notes"]);
    // The count ignores the limit.
    assert_eq!(store.count(&newest), 7);
}

#[test]
fn a_query_can_skip_the_text_of_another_entry() {
    let store = sample();
    let ids: Vec<i64> = store
        .query(&Query::default())
        .iter()
        .map(|e| e.id)
        .collect();
    // Walking back from the second `ls` (the fourth entry): the first `ls` is skipped.
    let back = Query {
        below_id: Some(ids[3]),
        unlike: Some(ids[3]),
        newest_first: true,
        ..Query::default()
    };
    assert_eq!(commands(&store.query(&back)), ["git push", "git status"]);
    // An id that names nothing skips nothing.
    let nothing = Query {
        unlike: Some(9999),
        ..Query::default()
    };
    assert_eq!(store.query(&nothing).len(), 7);
}

#[test]
fn queries_filter_by_time() {
    let store = Store::in_memory();
    record(&store, "old", "/", 0);
    assert!(store.settle(PATIENT));
    thread::sleep(Duration::from_millis(30));
    let middle = now_ms();
    thread::sleep(Duration::from_millis(30));
    record(&store, "new", "/", 0);
    assert!(store.settle(PATIENT));
    let since = Query {
        since_ms: Some(middle),
        ..Query::default()
    };
    assert_eq!(commands(&store.query(&since)), ["new"]);
    let later = Query {
        since_ms: Some(now_ms() + 60_000),
        ..Query::default()
    };
    assert_eq!(store.query(&later), Vec::new());
}

#[test]
fn a_shell_sees_its_own_commands_and_older_ones_but_not_another_shells_new_ones() {
    let scratch = Scratch::new();
    let early = Store::open(&scratch.db(), 100);
    record(&early, "before", "/", 0);
    assert!(early.settle(PATIENT));
    thread::sleep(Duration::from_millis(20));
    let mine = Store::open(&scratch.db(), 100);
    thread::sleep(Duration::from_millis(20));
    record(&early, "meanwhile elsewhere", "/", 0);
    record(&mine, "mine", "/", 0);
    assert!(early.settle(PATIENT) && mine.settle(PATIENT));
    let scoped = Query {
        own_or_older: true,
        ..Query::default()
    };
    assert_eq!(commands(&mine.query(&scoped)), ["before", "mine"]);
    // The other shell ran all of its own.
    assert_eq!(
        commands(&early.query(&scoped)),
        ["before", "meanwhile elsewhere"]
    );
    // Without the scope everything is there.
    assert_eq!(mine.query(&Query::default()).len(), 3);
}

#[test]
fn entries_can_be_removed_by_id() {
    let store = sample();
    let ids: Vec<i64> = store
        .query(&Query::default())
        .iter()
        .map(|e| e.id)
        .collect();
    let removed = store.delete(&[ids[1], 9999, ids[2]]);
    assert_eq!(removed, Some(vec![true, false, true]));
    assert_eq!(
        commands(&all(&store)),
        ["ls", "ls", "make", "ls -l", "vim notes"]
    );
    // Ids are not given out again.
    record(&store, "after", "/", 0);
    assert!(all(&store).last().unwrap().id > ids[6]);
}

#[test]
fn text_is_stored_as_it_is() {
    let store = Store::in_memory();
    let command = "echo 'বাংলা' \"a\nb\" \\; -- ; DROP TABLE history; '";
    record(&store, command, "/তথ্য", 0);
    let entries = all(&store);
    assert_eq!(entries[0].command, command);
    assert_eq!(entries[0].cwd, "/তথ্য");
    let found = store.query(&Query {
        contains: Some("'; DROP".to_owned()),
        ..Query::default()
    });
    assert_eq!(found.len(), 0);
    let found = store.query(&Query {
        contains: Some("DROP TABLE".to_owned()),
        ..Query::default()
    });
    assert_eq!(found.len(), 1);
}

/// Four shells (each a store with connections of its own) write to one file at the same time
/// while a fifth reads. Every command arrives, once, and the file is sound.
#[test]
fn shells_that_write_at_the_same_time_do_not_corrupt_the_file() {
    const SHELLS: usize = 4;
    const EACH: usize = 300;
    let scratch = Scratch::new();
    let path = scratch.db();
    // Opened at the same moment, on a file that does not exist yet.
    let writers: Vec<_> = (0..SHELLS)
        .map(|shell| {
            let path = path.clone();
            thread::spawn(move || {
                let store = Store::open(&path, 1_000_000);
                for n in 0..EACH {
                    record(&store, &format!("shell {shell} command {n}"), "/", 0);
                    if n % 50 == 0 {
                        assert!(store.settle(PATIENT), "shell {shell} at {n}");
                    }
                }
                assert!(store.settle(PATIENT));
                assert_eq!(store.take_problem(), None);
                store.session()
            })
        })
        .collect();
    let reader = Store::open(&path, 1_000_000);
    let mut seen = 0;
    while writers.iter().any(|writer| !writer.is_finished()) {
        let now = reader.query(&Query::default()).len();
        assert!(now >= seen, "entries never disappear");
        seen = now;
    }
    let sessions: Vec<i64> = writers.into_iter().map(|w| w.join().unwrap()).collect();
    let entries = all(&reader);
    assert_eq!(entries.len(), SHELLS * EACH);
    for (shell, session) in sessions.iter().enumerate() {
        let own: Vec<&Entry> = entries.iter().filter(|e| e.session == *session).collect();
        assert_eq!(own.len(), EACH);
        // Each shell's commands are in the order it ran them, all finished.
        for (n, entry) in own.iter().enumerate() {
            assert_eq!(entry.command, format!("shell {shell} command {n}"));
            assert_eq!(entry.exit_status, Some(0));
        }
    }
    assert_eq!(reader.take_problem(), None);
    drop(reader);
    assert_eq!(pragma(&path, "integrity_check"), "ok");
}

#[test]
fn a_database_held_by_another_writer_does_not_hold_up_the_shell() {
    let scratch = Scratch::new();
    let store = Store::open(&scratch.db(), 100);
    record(&store, "before", "/", 0);
    assert!(store.settle(PATIENT));
    // Another process in the middle of a long write.
    let other = Connection::open(scratch.db()).unwrap();
    other.execute_batch("BEGIN IMMEDIATE").unwrap();
    let started = Instant::now();
    record(&store, "during", "/", 0);
    assert!(!store.settle(Duration::from_millis(100)));
    let waited = started.elapsed();
    assert!(waited < Duration::from_millis(1500), "{waited:?}");
    // Reading still works, and shows what was there.
    assert_eq!(commands(&store.query(&Query::default())), ["before"]);
    other.execute_batch("COMMIT").unwrap();
    // The write went through once the other was done.
    assert_eq!(commands(&all(&store)), ["before", "during"]);
    assert_eq!(store.take_problem(), None);
}

#[test]
fn a_write_that_never_gets_its_turn_is_reported_once() {
    let scratch = Scratch::new();
    let store = Store::open(&scratch.db(), 100);
    assert!(store.settle(PATIENT));
    let other = Connection::open(scratch.db()).unwrap();
    other.execute_batch("BEGIN IMMEDIATE").unwrap();
    record(&store, "lost", "/", 0);
    // Longer than the writer's patience.
    assert!(store.settle(PATIENT));
    let problem = store.take_problem().unwrap();
    assert!(problem.contains("not written to history"), "{problem}");
    assert_eq!(store.take_problem(), None);
    other.execute_batch("COMMIT").unwrap();
    record(&store, "kept", "/", 0);
    assert_eq!(commands(&all(&store)), ["kept"]);
}

#[test]
fn a_file_that_cannot_be_used_leaves_history_in_memory() {
    let scratch = Scratch::new();
    let dir = scratch.0.join("data");
    std::fs::create_dir_all(&dir).unwrap();

    // A directory where the file should be.
    std::fs::create_dir(dir.join("history.db")).unwrap();
    // A FIFO: opening it for reading would wait for a writer for ever.
    let fifo = dir.join("fifo.db");
    let made = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap();
    assert!(made.success());
    // A file that is not a database.
    let text = dir.join("text.db");
    std::fs::write(
        &text,
        "this is not a database, it is a text file\n".repeat(200),
    )
    .unwrap();
    // A database from the future.
    let future = dir.join("future.db");
    Connection::open(&future)
        .unwrap()
        .pragma_update(None, "user_version", 99)
        .unwrap();
    // A directory that cannot be made.
    let under_file = text.join("sub").join("history.db");

    for path in [scratch.db(), fifo, text.clone(), future, under_file] {
        let store = Store::open(&path, 100);
        record(&store, "still works", "/", 0);
        assert_eq!(commands(&all(&store)), ["still works"], "{path:?}");
        let problem = store.take_problem().unwrap();
        assert!(
            problem.starts_with("history is kept in memory only: "),
            "{problem}"
        );
        assert!(problem.contains(&path.display().to_string()), "{problem}");
    }
    // The text file was not touched.
    assert!(
        std::fs::read_to_string(&text)
            .unwrap()
            .starts_with("this is not")
    );
}

#[test]
fn a_store_without_its_writer_answers_with_nothing() {
    // Before the writer thread has opened the database, a query finds no connection.
    let scratch = Scratch::new();
    let store = Store::open(&scratch.db(), 100);
    let _ = store.query(&Query::default());
    let _ = store.count(&Query::default());
    let _ = store.get(1);
    assert!(store.settle(PATIENT));
    assert_eq!(store.query(&Query::default()), Vec::new());
}

#[test]
fn sessions_differ() {
    let a = Store::in_memory();
    thread::sleep(Duration::from_millis(1));
    let b = Store::in_memory();
    assert_ne!(a.session(), b.session());
    assert!(a.session() >= 0 && b.session() >= 0);
    assert_eq!(a.clone().session(), a.session());
}

/// A database that is slow to open (another process holds it for a while) is waited for by
/// whoever needs to read, and then read. (CI found this: on a slow disk the first Up arrow
/// came back empty, because the reader gave up after 250 ms.)
#[test]
fn readers_wait_for_a_database_that_is_still_opening() {
    let scratch = Scratch::new();
    {
        let store = Store::open(&scratch.db(), 100);
        record(&store, "before", "/", 0);
        assert!(store.close(PATIENT));
    }
    let holder = Connection::open(scratch.db()).unwrap();
    holder.execute_batch("BEGIN EXCLUSIVE").unwrap();
    let release = thread::spawn(move || {
        thread::sleep(Duration::from_millis(600));
        holder.execute_batch("COMMIT").unwrap();
    });
    let store = Store::open(&scratch.db(), 100);
    // Longer than the 250 ms this used to be.
    assert!(store.settle(SETTLE), "the open was still going on");
    assert_eq!(commands(&store.query(&Query::default())), ["before"]);
    assert_eq!(store.take_problem(), None);
    release.join().unwrap();
}

#[test]
fn closing_writes_what_is_queued_and_leaves_one_file() {
    let scratch = Scratch::new();
    // Opened at the same moment, on a file that does not exist yet.
    let first = Store::open(&scratch.db(), 100);
    let second = Store::open(&scratch.db(), 100);
    record(&first, "one", "/", 0);
    assert!(first.settle(PATIENT));
    record(&second, "two", "/", 0);
    assert!(second.settle(PATIENT));
    let log = PathBuf::from(format!("{}-wal", scratch.db().display()));
    // Queued and closed in one go.
    record(&first, "three", "/", 0);
    assert!(first.close(PATIENT));
    assert_eq!(first.take_problem(), None);
    assert_eq!(second.take_problem(), None);
    assert!(log.exists(), "another shell still has the database open");
    assert_eq!(first.query(&Query::default()), Vec::new());
    assert_eq!(commands(&all(&second)), ["one", "two", "three"]);
    assert!(second.close(PATIENT));
    assert!(!log.exists(), "the last shell folded the log into the file");
    // A closed store takes commands without harm, and keeps none.
    record(&second, "late", "/", 0);
    assert!(second.settle(PATIENT));
    drop((first, second));
    let again = Store::open(&scratch.db(), 100);
    assert_eq!(commands(&all(&again)), ["one", "two", "three"]);
}

/// Two shells that start in the same instant on a database that does not exist yet: SQLite
/// tells one of them "locked" at once instead of letting it wait. Both must end up using the
/// file all the same. (Found when a store that had lost this race kept its history in memory.)
#[test]
fn stores_opened_at_the_same_moment_on_a_new_file_all_use_the_file() {
    for round in 0..40 {
        let scratch = Scratch::new();
        let first = Store::open(&scratch.db(), 100);
        let second = Store::open(&scratch.db(), 100);
        record(&first, "one", "/", 0);
        assert!(first.settle(PATIENT));
        record(&second, "two", "/", 0);
        assert!(second.settle(PATIENT));
        assert_eq!(first.take_problem(), None, "round {round}");
        assert_eq!(second.take_problem(), None, "round {round}");
        for store in [&first, &second] {
            assert_eq!(
                commands(&store.query(&Query::default())),
                ["one", "two"],
                "round {round}"
            );
        }
    }
}

/// The hint, for a store that is open.
fn hint_of(store: &Store, prefix: &str, cwd: &str, now: i64) -> Option<String> {
    store.hint(prefix, cwd, now).expect("the store is ready")
}

/// A store with the commands run in order, each with its folder and status.
fn hinted(rows: &[(&str, &str, i32)]) -> Store {
    let store = Store::in_memory();
    for (command, cwd, status) in rows {
        record(&store, command, cwd, *status);
    }
    assert!(store.settle(PATIENT));
    store
}

#[test]
fn a_hint_is_the_rest_of_a_longer_command_that_starts_with_the_text() {
    let store = hinted(&[("git status", "/a", 0), ("ls", "/a", 0)]);
    let now = now_ms();
    assert_eq!(
        hint_of(&store, "git s", "/a", now).as_deref(),
        Some("git status")
    );
    assert_eq!(
        hint_of(&store, "git status", "/a", now),
        None,
        "nothing is left to add"
    );
    assert_eq!(hint_of(&store, "", "/a", now), None);
    assert_eq!(hint_of(&store, "tig", "/a", now), None);
    assert_eq!(
        hint_of(&store, "it s", "/a", now),
        None,
        "a prefix, not a substring"
    );
}

#[test]
fn a_command_from_this_folder_beats_a_newer_one_from_another() {
    let store = hinted(&[
        ("make test", "/here", 0),
        ("make build", "/elsewhere", 0),
        ("make build", "/elsewhere", 0),
    ]);
    let now = now_ms();
    assert_eq!(
        hint_of(&store, "make", "/here", now).as_deref(),
        Some("make test")
    );
    assert_eq!(
        hint_of(&store, "make", "/elsewhere", now).as_deref(),
        Some("make build")
    );
    // A folder where neither ran: the score decides (two uses beat one).
    assert_eq!(
        hint_of(&store, "make", "/new", now).as_deref(),
        Some("make build")
    );
}

#[test]
fn frequency_counts_and_a_tie_goes_to_the_newest() {
    let store = hinted(&[
        ("cargo build", "/p", 0),
        ("cargo build", "/p", 0),
        ("cargo test", "/p", 0),
        ("cargo run", "/p", 0),
        ("cargo test", "/p", 0),
    ]);
    let now = now_ms();
    // build x2, test x2, run x1: test is the newest of the two that tie.
    assert_eq!(
        hint_of(&store, "cargo", "/p", now).as_deref(),
        Some("cargo test")
    );
}

#[test]
fn old_uses_count_less_than_recent_ones() {
    let store = hinted(&[
        ("job old", "/p", 0),
        ("job old", "/p", 0),
        ("job new", "/p", 0),
    ]);
    // Two weeks later every use is "old" (0.25 each): two uses still win.
    let later = now_ms() + 14 * DAY_MS;
    assert_eq!(
        hint_of(&store, "job", "/p", later).as_deref(),
        Some("job old")
    );
    // Just after: 3 uses worth 4 each, same order. Add a fresh one and it must win by recency
    // only when the counts are equal.
    let store = hinted(&[("job old", "/p", 0), ("job new", "/p", 0)]);
    let now = now_ms();
    assert_eq!(
        hint_of(&store, "job", "/p", now).as_deref(),
        Some("job new")
    );
}

#[test]
fn hints_leave_out_several_line_commands_and_mistyped_names() {
    // The mistyped name is the newest, so only the rule can keep it out.
    let store = hinted(&[
        ("echo one\necho two", "/p", 0),
        ("git status", "/p", 0),
        ("gti status", "/p", 127),
    ]);
    let now = now_ms();
    assert_eq!(hint_of(&store, "echo", "/p", now), None);
    assert_eq!(
        hint_of(&store, "g", "/p", now).as_deref(),
        Some("git status")
    );
    // A command that has not ended (no status yet) can be suggested.
    let token = store.start("sleep 100", "/p");
    let _ = token;
    assert!(store.settle(PATIENT));
    assert_eq!(
        hint_of(&store, "sle", "/p", now).as_deref(),
        Some("sleep 100")
    );
}

#[test]
fn hints_see_what_other_shells_ran_and_only_the_newest_rows() {
    let scratch = Scratch::new();
    let other = Store::open(&scratch.db(), 1000);
    record(&other, "deploy prod", "/p", 0);
    assert!(other.settle(PATIENT));
    let mine = Store::open(&scratch.db(), 1000);
    assert!(mine.settle(PATIENT));
    assert_eq!(
        hint_of(&mine, "dep", "/p", now_ms()).as_deref(),
        Some("deploy prod"),
        "another shell's command"
    );
    drop(mine);
    drop(other);
}

#[test]
fn the_hint_window_is_a_count_of_rows_from_the_newest() {
    let store = Store::in_memory();
    record(&store, "ancient command", "/p", 0);
    assert!(store.settle(PATIENT));
    {
        // Fill the window with other rows in one statement.
        let guard = lock(&store.inner.shared.reader);
        let connection = guard.as_ref().unwrap();
        connection
            .execute(
                "INSERT INTO history (command, cwd, started_at, session_id, host) \
                 WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?) \
                 SELECT 'filler', '/p', 0, 1, 'h' FROM n",
                [HINT_WINDOW],
            )
            .unwrap();
    }
    assert_eq!(hint_of(&store, "anc", "/p", now_ms()), None);
    assert_eq!(
        hint_of(&store, "fill", "/p", now_ms()).as_deref(),
        Some("filler")
    );
}

#[test]
fn a_visit_is_counted_and_dated() {
    let store = Store::in_memory();
    let before = now_ms();
    store.visit("/a");
    store.visit("/a");
    store.visit("/b");
    assert!(store.settle(PATIENT));
    let mut dirs = store.directories();
    dirs.sort_by(|x, y| x.path.cmp(&y.path));
    assert_eq!(dirs.len(), 2);
    assert_eq!((dirs[0].path.as_str(), dirs[0].visits), ("/a", 2));
    assert_eq!((dirs[1].path.as_str(), dirs[1].visits), ("/b", 1));
    assert!(dirs[0].last_ms >= before && dirs[0].last_ms <= now_ms());
}

#[test]
fn visited_folders_survive_a_restart_and_only_the_newest_thousand_are_kept() {
    let scratch = Scratch::new();
    {
        let store = Store::open(&scratch.db(), 1000);
        assert!(store.settle(PATIENT));
        store.visit("/first");
        assert!(store.settle(PATIENT));
        // Later by at least a millisecond, so that the order is not a tie.
        std::thread::sleep(Duration::from_millis(5));
        // Enough later visits to push "/first" out of the thousand.
        for n in 0..DIRS_KEPT {
            store.visit(&format!("/d{n}"));
        }
        assert!(store.close(PATIENT));
    }
    let store = Store::open(&scratch.db(), 1000);
    assert!(store.settle(PATIENT));
    let dirs = store.directories();
    assert_eq!(dirs.len(), usize::try_from(DIRS_KEPT).unwrap());
    assert!(dirs.iter().all(|dir| dir.path != "/first"));
    assert!(dirs.iter().any(|dir| dir.path == "/d0"));
}

#[test]
fn a_version_one_file_without_dirs_gets_the_table_and_keeps_its_version() {
    let scratch = Scratch::new();
    std::fs::create_dir_all(scratch.db().parent().unwrap()).unwrap();
    {
        // A file as S13b to S25 made it: the history table, version 1, no `dirs`.
        let connection = Connection::open(scratch.db()).unwrap();
        connection.execute_batch(CREATE).unwrap();
        connection.pragma_update(None, "user_version", 1).unwrap();
    }
    let store = Store::open(&scratch.db(), 1000);
    store.visit("/x");
    assert!(store.settle(PATIENT));
    assert_eq!(store.directories().len(), 1);
    assert!(store.close(PATIENT));
    assert_eq!(pragma(&scratch.db(), "user_version"), "1");
}

/// The premise of the hint-timing fix, with a real writer held up by another process: while
/// the writer cannot write, a hint question finds nothing, and the same question finds the
/// command once the writer has caught up. So "nothing" is not an answer to be kept.
#[test]
fn a_hint_asked_while_the_writer_is_held_up_finds_nothing_until_it_catches_up() {
    let scratch = Scratch::new();
    let store = Store::open(&scratch.db(), 100);
    assert!(store.settle(PATIENT), "the database is open");
    // Another process in the middle of a long write: this store's writer must wait for it.
    let other = Connection::open(scratch.db()).unwrap();
    other.execute_batch("BEGIN IMMEDIATE").unwrap();
    record(&store, "make test", "/p", 0);
    assert!(
        !store.settle(Duration::from_millis(100)),
        "the writer cannot have caught up"
    );
    assert_eq!(hint_of(&store, "make", "/p", now_ms()), None);
    other.execute_batch("COMMIT").unwrap();
    assert!(store.settle(PATIENT), "the writer caught up");
    assert_eq!(
        hint_of(&store, "make", "/p", now_ms()).as_deref(),
        Some("make test")
    );
}

/// Before the writer has opened the database there is nothing to read from: that is "not
/// ready", which a caller must not mistake for "no hint".
#[test]
fn a_hint_asked_before_the_database_is_open_is_not_ready() {
    let scratch = Scratch::new();
    {
        let store = Store::open(&scratch.db(), 100);
        record(&store, "make test", "/p", 0);
        assert!(store.close(PATIENT));
    }
    // A process that holds the whole file: the writer cannot finish opening it.
    let holder = Connection::open(scratch.db()).unwrap();
    holder.execute_batch("BEGIN EXCLUSIVE").unwrap();
    let store = Store::open(&scratch.db(), 100);
    assert_eq!(store.hint("make", "/p", now_ms()), Err(NotReady));
    holder.execute_batch("COMMIT").unwrap();
    assert!(store.settle(PATIENT), "the open finished");
    assert_eq!(
        store.hint("make", "/p", now_ms()),
        Ok(Some("make test".to_owned()))
    );
    // An empty prefix is a plain "nothing", also when not ready.
    assert_eq!(store.hint("", "/p", now_ms()), Ok(None));
    assert_eq!(store.hint("zzz", "/p", now_ms()), Ok(None));
}

/// A read that fails (here: the table is gone) is "not ready", not "no hint".
#[test]
fn a_hint_whose_query_fails_is_not_ready() {
    let scratch = Scratch::new();
    let store = Store::open(&scratch.db(), 100);
    record(&store, "make test", "/p", 0);
    assert!(store.settle(PATIENT));
    assert_eq!(
        store.hint("make", "/p", now_ms()),
        Ok(Some("make test".to_owned()))
    );
    let other = Connection::open(scratch.db()).unwrap();
    other.execute_batch("DROP TABLE history").unwrap();
    assert_eq!(store.hint("make", "/p", now_ms()), Err(NotReady));
}

/// A store with `rows` commands, `one-<n>`, every third one failed.
fn filled(rows: usize) -> Store {
    let store = Store::in_memory();
    for n in 0..rows {
        record(&store, &format!("one-{n}"), "/p", i32::from(n % 3 == 0));
    }
    assert!(store.settle(Duration::from_secs(30)));
    store
}

fn each(store: &Store, query: &Query) -> Vec<Entry> {
    let mut seen = Vec::new();
    store.query_each(query, |entry| {
        seen.push(entry);
        true
    });
    seen
}

#[test]
fn query_each_gives_the_same_rows_as_query_in_both_orders_across_pages() {
    // 4,500 rows are three pages of 2,000.
    let store = filled(4_500);
    for newest_first in [false, true] {
        let query = Query {
            newest_first,
            ..Query::default()
        };
        assert_eq!(each(&store, &query), store.query(&query), "{newest_first}");
    }
}

#[test]
fn query_each_respects_a_limit_that_ends_inside_or_on_a_page() {
    let store = filled(4_500);
    for limit in [0, 1, 1_999, 2_000, 2_001, 4_000, 4_500, 9_000] {
        for newest_first in [false, true] {
            let query = Query {
                newest_first,
                limit: Some(limit),
                ..Query::default()
            };
            let seen = each(&store, &query);
            assert_eq!(seen, store.query(&query), "limit {limit} {newest_first}");
            assert_eq!(seen.len(), (limit as usize).min(4_500));
        }
    }
}

#[test]
fn query_each_keeps_the_filters_of_the_query_on_every_page() {
    let store = filled(4_500);
    let query = Query {
        failed: true,
        ..Query::default()
    };
    let seen = each(&store, &query);
    assert_eq!(seen.len(), 1_500);
    assert!(seen.iter().all(|entry| entry.exit_status == Some(1)));
    assert_eq!(seen, store.query(&query));
}

#[test]
fn query_each_stops_when_asked() {
    let store = filled(4_500);
    let mut calls = 0;
    store.query_each(&Query::default(), |_| {
        calls += 1;
        calls < 3
    });
    assert_eq!(calls, 3);
}

#[test]
fn query_each_on_an_empty_store_calls_nothing() {
    let store = Store::in_memory();
    assert!(each(&store, &Query::default()).is_empty());
}

/// A history with ties, folders, repeats, mistyped names, several-line commands, prefixes of one
/// another and different ages, to compare the ranking with the database question.
fn mixed_history() -> Store {
    let store = Store::in_memory();
    for (command, cwd, status) in [
        ("git status", "/a", 0),
        ("git status", "/b", 0),
        ("git stash", "/b", 0),
        ("git s", "/a", 0),
        ("git sttaus", "/a", 127),
        ("ls", "/a", 0),
        ("ls -la", "/b", 0),
        ("ls -la", "/b", 0),
        ("make test", "/a", 0),
        ("make build", "/b", 0),
        ("make", "/b", 0),
        ("echo 'a\nb'", "/a", 0),
        ("grep -r foo .", "/a", 1),
        ("grep -r foo .", "/a", 1),
        ("cargo build --release", "/a", 0),
        ("cargo b", "/b", 0),
        ("é\u{301}cho", "/a", 0),
        ("ünïcode test", "/a", 0),
    ] {
        record(&store, command, cwd, status);
    }
    assert!(store.settle(PATIENT));
    store
}

/// Every prefix (at character boundaries) of the commands of `mixed_history`, and some that match
/// nothing.
fn prefixes_of_mixed_history() -> Vec<String> {
    let mut prefixes: Vec<String> = ["z", "gitx", "é", "ü"].map(String::from).to_vec();
    for command in [
        "git status",
        "ls -la",
        "make build",
        "cargo build --release",
        "grep -r foo .",
        "echo 'a\nb'",
        "é\u{301}cho",
        "git sttaus",
    ] {
        for end in 0..=command.len() {
            if command.is_char_boundary(end) {
                prefixes.push(command[..end].to_owned());
            }
        }
    }
    prefixes
}

#[test]
fn the_ranking_answers_every_prefix_as_the_database_question_does() {
    let store = mixed_history();
    let now = now_ms();
    for cwd in ["/a", "/b", "/nowhere"] {
        let ranking = store.hint_ranking(cwd, now, 1000).expect("ready");
        for prefix in prefixes_of_mixed_history() {
            let from_ranking = ranking.best(&prefix).expect("a complete ranking can tell");
            let from_database = hint_of(&store, &prefix, cwd, now);
            assert_eq!(
                from_ranking,
                from_database.as_deref(),
                "prefix {prefix:?} in {cwd}"
            );
        }
    }
}

#[test]
fn a_cut_ranking_never_gives_a_wrong_answer_and_says_unknown_when_it_cannot_tell() {
    let store = mixed_history();
    let now = now_ms();
    let full = store.hint_ranking("/a", now, 1000).expect("ready");
    assert!(full.len() > 6);
    let cut = store.hint_ranking("/a", now, 3).expect("ready");
    assert_eq!(cut.len(), 3);
    let (mut told, mut unknown) = (0, 0);
    for prefix in prefixes_of_mixed_history() {
        match cut.best(&prefix) {
            // Whatever the cut list says, the full list says the same.
            Some(answer) => {
                assert_eq!(Some(answer), full.best(&prefix), "prefix {prefix:?}");
                told += 1;
            }
            None => unknown += 1,
        }
    }
    // Both outcomes happen with this history, so the test looks at both.
    assert!(told > 0 && unknown > 0, "told {told}, unknown {unknown}");
    // A complete list can say "nothing"; a cut one cannot.
    assert_eq!(full.best("zzzz"), Some(None));
    assert_eq!(cut.best("zzzz"), None);
}

#[test]
fn the_ranking_is_not_ready_when_the_database_is_not() {
    let scratch = Scratch::new();
    std::fs::create_dir_all(scratch.db()).unwrap(); // a directory where the file should be
    let store = Store::open(&scratch.db(), 1000);
    assert_eq!(store.hint_ranking("/p", now_ms(), 10), Err(NotReady));
}

#[test]
fn distinct_recent_folds_a_command_into_its_newest_run() {
    let store = Store::in_memory();
    record(&store, "ls", "/a", 0);
    record(&store, "make", "/a", 2);
    record(&store, "ls", "/b", 1);
    record(&store, "echo hi", "/c", 0);
    record(&store, "make", "/c", 0);
    assert!(store.settle(PATIENT));
    let rows = store.distinct_recent(100).expect("ready");
    let commands: Vec<_> = rows.iter().map(|row| row.command.as_str()).collect();
    // Newest run first, one row for each text.
    assert_eq!(commands, ["make", "echo hi", "ls"]);
    // The data are those of the newest run, and `runs` counts all of them.
    assert_eq!(
        (rows[0].cwd.as_str(), rows[0].exit_status, rows[0].runs),
        ("/c", Some(0), 2)
    );
    assert_eq!(
        (rows[1].cwd.as_str(), rows[1].exit_status, rows[1].runs),
        ("/c", Some(0), 1)
    );
    assert_eq!(
        (rows[2].cwd.as_str(), rows[2].exit_status, rows[2].runs),
        ("/b", Some(1), 2)
    );
    let newest_ls = all(&store).into_iter().find(|e| e.cwd == "/b").unwrap();
    assert_eq!(rows[2].id, newest_ls.id);
    assert_eq!(rows[2].started_ms, newest_ls.started_ms);
}

#[test]
fn distinct_recent_looks_only_at_the_newest_runs() {
    let store = Store::in_memory();
    for n in 0..10 {
        record(&store, &format!("cmd {n}"), "/a", 0);
    }
    record(&store, "cmd 0", "/a", 0);
    assert!(store.settle(PATIENT));
    // The newest three runs are `cmd 8`, `cmd 9` and the second `cmd 0`.
    let rows = store.distinct_recent(3).expect("ready");
    let commands: Vec<_> = rows.iter().map(|row| row.command.as_str()).collect();
    assert_eq!(commands, ["cmd 0", "cmd 9", "cmd 8"]);
    // `runs` counts inside the window only.
    assert_eq!(rows[0].runs, 1);
    assert!(store.distinct_recent(0).expect("ready").is_empty());
}

#[test]
fn distinct_recent_shows_a_command_that_is_still_running_without_a_status() {
    let store = Store::in_memory();
    let _token = store.start("sleep 100", "/a");
    assert!(store.settle(PATIENT));
    let rows = store.distinct_recent(10).expect("ready");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].exit_status, None);
}

#[test]
fn distinct_recent_is_not_ready_when_the_database_is_not() {
    let scratch = Scratch::new();
    std::fs::create_dir_all(scratch.db()).unwrap();
    let store = Store::open(&scratch.db(), 1000);
    assert_eq!(store.distinct_recent(10), Err(NotReady));
}
