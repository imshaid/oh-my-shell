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
        .prepare("SELECT name FROM sqlite_master WHERE type = 'index' ORDER BY name")
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

/// A measurement, not a check: how long the queries of the editor and of the `history`
/// builtin take on 100,000 commands (the budget of blueprint §16 is 20 ms). Run it with
/// `cargo test -p omsh-history --release -- --ignored --nocapture timing`.
#[test]
#[ignore = "a measurement; S19 turns the budget into a gate"]
fn timing_of_queries_on_a_hundred_thousand_commands() {
    const ROWS: usize = 100_000;
    let scratch = Scratch::new();
    let store = Store::open(&scratch.db(), 1_000_000);
    let filling = Instant::now();
    for n in 0..ROWS {
        let cwd = format!("/home/user/project-{}", n % 50);
        let command = format!(
            "command-{} --flag value-{n} some/path/file-{n}.txt",
            n % 997
        );
        record(&store, &command, &cwd, i32::from(n % 10 == 0));
    }
    assert!(store.settle(Duration::from_secs(120)));
    println!("filled {ROWS} rows in {:?}", filling.elapsed());
    assert_eq!(store.count(&Query::default()), ROWS as u64);
    let newest = store.query(&Query {
        newest_first: true,
        limit: Some(1),
        ..Query::default()
    })[0]
        .id;

    let cases: Vec<(&str, Query)> = vec![
        (
            "Up, first press",
            Query {
                own_or_older: true,
                newest_first: true,
                limit: Some(1),
                ..Query::default()
            },
        ),
        (
            "Up, next press (skips repeats)",
            Query {
                own_or_older: true,
                newest_first: true,
                limit: Some(1),
                below_id: Some(newest),
                unlike: Some(newest),
                ..Query::default()
            },
        ),
        (
            "Up with a prefix that is found",
            Query {
                own_or_older: true,
                newest_first: true,
                limit: Some(1),
                prefix: Some("command-500 ".to_owned()),
                ..Query::default()
            },
        ),
        (
            "Up with a prefix that nothing has (reads every row)",
            Query {
                own_or_older: true,
                newest_first: true,
                limit: Some(1),
                prefix: Some("zzz".to_owned()),
                ..Query::default()
            },
        ),
        (
            "search for a text that nothing has (reads every row)",
            Query {
                newest_first: true,
                limit: Some(100),
                contains: Some("zzz".to_owned()),
                ..Query::default()
            },
        ),
        (
            "history (newest 100)",
            Query {
                newest_first: true,
                limit: Some(100),
                ..Query::default()
            },
        ),
        (
            "history --here (newest 100 of 2,000)",
            Query {
                newest_first: true,
                limit: Some(100),
                cwd: Some("/home/user/project-7".to_owned()),
                ..Query::default()
            },
        ),
        (
            "history --failed (newest 100)",
            Query {
                newest_first: true,
                limit: Some(100),
                failed: true,
                ..Query::default()
            },
        ),
        (
            "history --since (newest 100)",
            Query {
                newest_first: true,
                limit: Some(100),
                since_ms: Some(0),
                ..Query::default()
            },
        ),
        (
            "history --all (all 100,000 rows)",
            Query {
                newest_first: true,
                ..Query::default()
            },
        ),
    ];
    for (name, query) in cases {
        let mut worst = Duration::ZERO;
        let mut found = 0;
        for _ in 0..20 {
            let started = Instant::now();
            found = store.query(&query).len();
            worst = worst.max(started.elapsed());
        }
        println!("{worst:>12.3?}  worst of 20  {name}  ({found} rows)");
    }
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
