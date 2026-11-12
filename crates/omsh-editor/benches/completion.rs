//! How long it takes for the completion menu to have its choices (blueprint §10 and §16:
//! "completion menu open", under 50 ms at the 99th percentile, on a large `PATH`).
//!
//! The time measured is from the Tab to the choices being there: the call that finds out the
//! answer is not ready, the helper thread that lists `PATH`, the editor noticing that it is
//! done, and the call that answers. The menu is drawn by the terminal library after that, which
//! is not measured here.
//!
//! `PATH` is made for the run, in a scratch folder: 20,000 programs in 12 directories, with
//! names like the real ones (many that start alike, so that a 1-letter prefix has hundreds of
//! matches). Cases:
//!
//! - **first Tab** of a shell: nothing is remembered, the whole `PATH` is listed;
//! - **later Tab**: the list is kept, only the choices are found;
//! - **a folder of 5,000 files**, and **flags from a completion file of 5,000 lines** (first
//!   Tab, which reads the file, and later Tabs).
//!
//! "First" is the first in this process; the file system's own cache is warm (a run first lists
//! everything once), as it is for a person who has used the machine for a while. Run with
//! `cargo bench -p omsh-editor --bench completion`. A plain program with no benchmark framework;
//! CI gating with criterion is S19's job. It exits with an error when a p99 passes the budget.

#![expect(
    clippy::expect_used,
    reason = "a benchmark that cannot set up its folders has nothing to measure"
)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use omsh_editor::{Facts, Scene, ShellCompleter};
use reedline::{Completer, CompletionStatus};

/// The budget of blueprint §16.
const BUDGET: Duration = Duration::from_millis(50);

const WORDS: &[&str] = &[
    "git",
    "gcc",
    "gzip",
    "grep",
    "gawk",
    "python",
    "pip",
    "perl",
    "ping",
    "pnpm",
    "node",
    "npm",
    "nano",
    "nm",
    "make",
    "man",
    "mv",
    "mkdir",
    "ls",
    "ln",
    "less",
    "lsblk",
    "cargo",
    "cat",
    "chmod",
    "chown",
    "curl",
    "cp",
    "cut",
    "docker",
    "dd",
    "df",
    "du",
    "diff",
    "dig",
    "echo",
    "env",
    "ed",
    "find",
    "file",
    "free",
    "fmt",
    "head",
    "host",
    "htop",
    "id",
    "ip",
    "java",
    "jq",
    "kill",
    "less",
    "xargs",
    "xz",
    "yes",
    "zip",
    "zcat",
    "systemctl",
    "ssh",
    "sed",
    "sort",
    "sudo",
    "tar",
    "tee",
    "top",
    "touch",
    "tr",
    "tail",
    "uname",
    "uniq",
    "vim",
    "wc",
    "wget",
    "which",
    "who",
];

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    let index = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[index]
}

fn scratch() -> PathBuf {
    let base = option_env!("CARGO_TARGET_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join(format!("omsh-bench-complete-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("make the scratch folder");
    dir
}

/// 20,000 programs in 12 directories; returns the value of `PATH`.
fn make_path(root: &Path) -> String {
    let mut dirs = Vec::new();
    for d in 0..12 {
        let dir = root.join(format!("bin{d}"));
        fs::create_dir_all(&dir).expect("make a PATH directory");
        dirs.push(dir);
    }
    for n in 0..20_000_usize {
        let name = format!("{}{}", WORDS[n % WORDS.len()], n / WORDS.len());
        let file = dirs[n % dirs.len()].join(&name);
        fs::write(&file, "").expect("make a program");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).expect("make it executable");
    }
    dirs.iter()
        .map(|dir| dir.to_str().expect("utf-8 path"))
        .collect::<Vec<_>>()
        .join(":")
}

fn new_scene(facts: Facts, cwd: &Path) -> Arc<Mutex<Scene>> {
    let scene = Arc::new(Mutex::new(Scene::default()));
    scene
        .lock()
        .expect("lock")
        .reset(facts, cwd.to_path_buf(), None);
    scene
}

/// Asks for the choices at the end of `line` and waits until they are there. Returns how many.
fn tab(completer: &mut ShellCompleter, line: &str) -> usize {
    let mut result = completer.complete(line, line.len());
    let start = Instant::now();
    while result.is_pending() {
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "the helper never finished"
        );
        if completer.poll_completion() == CompletionStatus::Ready {
            result = completer.complete(line, line.len());
        } else {
            // What the editor does: it looks again at its next turn of the loop.
            std::thread::sleep(Duration::from_micros(200));
        }
    }
    result.suggestions().len()
}

struct Case {
    name: String,
    times: Vec<Duration>,
}

fn report(cases: &[Case]) -> Duration {
    println!(
        "{:<44} {:>9} {:>9} {:>9} {:>7}",
        "case", "p50", "p99", "max", "tabs"
    );
    let mut worst = Duration::ZERO;
    for case in cases {
        let mut times = case.times.clone();
        times.sort_unstable();
        let p99 = percentile(&times, 0.99);
        worst = worst.max(p99);
        println!(
            "{:<44} {:>9.1?} {:>9.1?} {:>9.1?} {:>7}",
            case.name,
            percentile(&times, 0.5),
            p99,
            times[times.len() - 1],
            times.len()
        );
    }
    worst
}

fn main() {
    let root = scratch();
    let path = make_path(&root);
    let facts = Facts {
        path: path.clone().into_bytes(),
        functions: vec!["gfn".to_owned(); 20],
        builtins: vec!["cd".to_owned(), "echo".to_owned()],
        ..Facts::default()
    };
    let prefixes = ["g", "gi", "git", "p", "py", "x"];
    let mut cases = Vec::new();

    // A run first, so that the file system's cache is warm for every case.
    {
        let mut completer = ShellCompleter::new(new_scene(facts.clone(), &root));
        tab(&mut completer, "g");
    }

    // First Tab of a shell: a new completer each time, so nothing is remembered.
    for prefix in prefixes {
        let mut times = Vec::new();
        let mut found = 0;
        for _ in 0..30 {
            let mut completer = ShellCompleter::new(new_scene(facts.clone(), &root));
            let start = Instant::now();
            found = tab(&mut completer, prefix);
            times.push(start.elapsed());
        }
        cases.push(Case {
            name: format!("first Tab, {prefix:?} ({found} choices)"),
            times,
        });
    }

    // Later Tabs: the list is kept; a new prompt each time, so no answer is reused.
    {
        let scene = new_scene(facts.clone(), &root);
        let mut completer = ShellCompleter::new(Arc::clone(&scene));
        tab(&mut completer, "g");
        for prefix in prefixes {
            let mut times = Vec::new();
            let mut found = 0;
            for _ in 0..300 {
                scene
                    .lock()
                    .expect("lock")
                    .reset(facts.clone(), root.clone(), None);
                let start = Instant::now();
                found = tab(&mut completer, prefix);
                times.push(start.elapsed());
            }
            cases.push(Case {
                name: format!("later Tab, {prefix:?} ({found} choices)"),
                times,
            });
        }
    }

    // A command line with a command that is not first.
    {
        let scene = new_scene(facts.clone(), &root);
        let mut completer = ShellCompleter::new(Arc::clone(&scene));
        tab(&mut completer, "g");
        let mut times = Vec::new();
        for _ in 0..300 {
            scene
                .lock()
                .expect("lock")
                .reset(facts.clone(), root.clone(), None);
            let start = Instant::now();
            tab(&mut completer, "cat file.txt | grep -v x && gi");
            times.push(start.elapsed());
        }
        cases.push(Case {
            name: "later Tab, after a pipe and a &&".to_owned(),
            times,
        });
    }

    // A folder of 5,000 files.
    let files = root.join("files");
    fs::create_dir_all(&files).expect("make the folder");
    for n in 0..5_000 {
        fs::write(files.join(format!("file-{n:05}.txt")), "").expect("make a file");
    }
    {
        let scene = new_scene(facts.clone(), &files);
        let mut completer = ShellCompleter::new(Arc::clone(&scene));
        let mut times = Vec::new();
        let mut found = 0;
        for _ in 0..100 {
            scene
                .lock()
                .expect("lock")
                .reset(facts.clone(), files.clone(), None);
            let start = Instant::now();
            found = tab(&mut completer, "cat fi");
            times.push(start.elapsed());
        }
        cases.push(Case {
            name: format!("file names, 5,000 in the folder ({found} choices)"),
            times,
        });
    }

    // Flags from a fish completion file of 5,000 lines.
    let fish = root.join("fish");
    fs::create_dir_all(&fish).expect("make the fish folder");
    let mut text = String::new();
    for n in 0..5_000 {
        text.push_str(&format!(
            "complete -c bigcmd -s {} -l option-{n:05} -d 'Option number {n} does something'\n",
            char::from(b'a' + u8::try_from(n % 26).expect("small")),
        ));
    }
    fs::write(fish.join("bigcmd.fish"), text).expect("write the file");
    {
        let facts = Facts {
            fish_dirs: vec![fish.clone()],
            ..facts.clone()
        };
        let mut times = Vec::new();
        let mut found = 0;
        for _ in 0..30 {
            let mut completer = ShellCompleter::new(new_scene(facts.clone(), &root));
            let start = Instant::now();
            found = tab(&mut completer, "bigcmd --option-0");
            times.push(start.elapsed());
        }
        cases.push(Case {
            name: format!("first Tab for flags, 5,000-line file ({found})"),
            times,
        });
        let scene = new_scene(facts.clone(), &root);
        let mut completer = ShellCompleter::new(Arc::clone(&scene));
        tab(&mut completer, "bigcmd --o");
        let mut times = Vec::new();
        for _ in 0..100 {
            scene
                .lock()
                .expect("lock")
                .reset(facts.clone(), root.clone(), None);
            let start = Instant::now();
            found = tab(&mut completer, "bigcmd --option-0");
            times.push(start.elapsed());
        }
        cases.push(Case {
            name: format!("later Tab for flags, 5,000-line file ({found})"),
            times,
        });
    }

    let worst = report(&cases);
    println!(
        "worst p99: {worst:.1?} (budget: {BUDGET:.0?}; 20,000 programs in 12 PATH directories)"
    );
    let _ = fs::remove_dir_all(&root);
    if worst > BUDGET {
        eprintln!("over budget");
        std::process::exit(1);
    }
}
