//! Tests for the color wrappers, with fake `ls`, `grep`, `egrep`, `fgrep` and `diff` programs
//! first on `PATH`. A fake logs every call to a file (`OMSH_LOG`) and prints its arguments, so
//! the tests can see both what a command received and how many times it was probed.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use crate::io::capture::Captured;
use crate::shell::{LineResult, Outcome, Shell};
use crate::vars::Variables;

const FAKE: &str = r#"#!/bin/sh
echo "call $*" >> "$OMSH_LOG"
case "$1" in --color=auto) [ "$OMSH_ACCEPT" = no ] && exit 2;; esac
case "$*" in
  *"-d /"*|*"-q x"*|*"/dev/null /dev/null"*)
    if [ -n "$OMSH_FLAKY" ] && [ ! -e "$OMSH_FLAKY" ]; then : > "$OMSH_FLAKY"; exit 130; fi;;
esac
for a; do printf '[%s]' "$a"; done; echo
echo "env:${LS_COLORS-unset}/${GREP_COLOR-unset}/${GREP_COLORS-unset}"
case "$*" in *fail) exit 3;; esac
exit 0
"#;

/// How long `wait_until_runnable` keeps trying: 200 tries, 10 ms apart.
const BUSY_TRIES: u32 = 200;
const BUSY_PAUSE: Duration = Duration::from_millis(10);

/// Waits until `file` can be executed, by running it once (it must be one of the fakes: it logs
/// to `/dev/null` and exits 0).
///
/// `exec` fails with "text file busy" (`ETXTBSY`) while any process holds the file open for
/// writing. The tests run on many threads; a `fork` on another thread that happens between our
/// `fs::write` opening the file and closing it copies the open handle into the child, which
/// keeps it until its own `exec`. So a fake that was written a moment ago can be busy for a few
/// milliseconds, and the wrapper's probe of it ("does it take `--color=auto`?") then fails
/// and the wrapper quietly adds no flag. A holder always lets go: once one run succeeds nobody
/// has the file open for writing any more, and nobody will (nothing writes the fakes again).
/// The same retry as `Delivery::run` makes for `notify-send`.
fn wait_until_runnable(file: &Path) {
    let mut tries = 0;
    loop {
        let started = Command::new(file)
            .arg("ready")
            .env("OMSH_LOG", "/dev/null")
            .env("OMSH_ACCEPT", "yes")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        match started {
            Ok(status) => {
                assert!(status.success(), "{} failed: {status}", file.display());
                return;
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::ExecutableFileBusy && tries < BUSY_TRIES =>
            {
                tries += 1;
                std::thread::sleep(BUSY_PAUSE);
            }
            Err(error) => panic!(
                "{} cannot be run after {tries} retries: {error}",
                file.display()
            ),
        }
    }
}

/// A directory of fake programs, removed at the end of the test.
struct Fakes {
    dir: PathBuf,
}

impl Fakes {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("omsh-color-{}-{n}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        for name in ["ls", "grep", "egrep", "fgrep", "diff"] {
            let file = dir.join(name);
            fs::write(&file, FAKE).unwrap();
            fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
            wait_until_runnable(&file);
        }
        Self { dir }
    }

    fn log(&self) -> PathBuf {
        self.dir.join("log")
    }

    /// The calls so far, one per line.
    fn calls(&self) -> Vec<String> {
        fs::read_to_string(self.log())
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// How many of the calls were probes (the commands in `PROBES`).
    fn probes(&self) -> usize {
        self.calls()
            .iter()
            .filter(|c| {
                c.starts_with("call --color=auto")
                    && (c.ends_with("-d /")
                        || c.ends_with("-q x")
                        || c.ends_with("/dev/null /dev/null"))
            })
            .count()
    }

    fn path(&self) -> &Path {
        &self.dir
    }

    /// An interactive-style shell with the wrappers defined and the fakes first on `PATH`.
    fn shell(&self, accept: bool) -> Shell {
        let path = format!("{}:/usr/bin:/bin", self.dir.display());
        let mut sh = Shell::with_variables(Variables::from_pairs([
            ("PATH", path.as_str()),
            ("HOME", "/tmp"),
            ("OMSH_LOG", self.log().to_str().unwrap()),
            ("OMSH_ACCEPT", if accept { "yes" } else { "no" }),
        ]))
        .subshell();
        let captured = Captured::new();
        sh.define_color_wrappers(&captured.io);
        sh
    }
}

impl Drop for Fakes {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

struct Done {
    out: String,
    err: String,
    status: u8,
}

fn run(sh: &mut Shell, src: &str) -> Done {
    let mut captured = Captured::new();
    let status = match sh.run_source(src, true, &mut captured.io) {
        LineResult::Ran(Outcome::Continue(s) | Outcome::Exit(s)) => s.code(),
        other => panic!("not run: {src}: {other:?}"),
    };
    Done {
        out: captured.stdout(),
        err: captured.stderr(),
        status,
    }
}

/// What to print when the wrapper did not add the flag although the fake accepts it: the wrapper
/// hides its probe's output, so ask the same question again, in the shell and directly, and
/// show the fake's call log (a probe that never started leaves no line in it).
fn probe_report(fakes: &Fakes, sh: &mut Shell, done: &Done) -> String {
    let again = run(
        sh,
        "command ls --color=auto -d / >/dev/null; echo probe-status=$?",
    );
    let direct = Command::new(fakes.path().join("ls"))
        .args(["--color=auto", "-d", "/"])
        .env("OMSH_LOG", "/dev/null")
        .env("OMSH_ACCEPT", "yes")
        .output();
    let direct = match direct {
        Ok(out) => format!(
            "status {:?}, stderr {:?}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        ),
        Err(error) => format!("could not start: {error} ({:?})", error.kind()),
    };
    format!(
        "stderr of the line: {:?} (status {})\n\
         calls the fake logged: {:?}\n\
         the probe asked again in the shell: {:?}, stderr {:?}\n\
         the fake run directly: {direct}",
        done.err,
        done.status,
        fakes.calls(),
        again.out,
        again.err,
    )
}

#[test]
fn the_flag_is_added_when_the_program_takes_it() {
    let fakes = Fakes::new();
    let mut sh = fakes.shell(true);
    for name in ["ls", "grep", "egrep", "fgrep", "diff"] {
        let done = run(&mut sh, &format!("{name} one two"));
        assert_eq!(
            done.out, "[--color=auto][one][two]\nenv:unset/unset/unset\n",
            "{name}"
        );
    }
}

#[test]
fn the_flag_is_left_out_when_the_program_does_not_take_it() {
    let fakes = Fakes::new();
    let mut sh = fakes.shell(false);
    let done = run(&mut sh, "ls one");
    assert_eq!(done.out, "[one]\nenv:unset/unset/unset\n");
    assert_eq!(done.status, 0);
    // A "no" is remembered too.
    run(&mut sh, "ls two");
    assert_eq!(fakes.probes(), 1);
}

#[test]
fn no_color_turns_the_flag_off_at_every_call_and_costs_no_probe() {
    let fakes = Fakes::new();
    let mut sh = fakes.shell(true);
    assert_eq!(
        run(&mut sh, "NO_COLOR=1 ls a").out,
        "[a]\nenv:unset/unset/unset\n"
    );
    run(&mut sh, "export NO_COLOR=yes");
    assert_eq!(run(&mut sh, "ls b").out, "[b]\nenv:unset/unset/unset\n");
    assert_eq!(fakes.probes(), 0, "{:?}", fakes.calls());
    // An empty NO_COLOR counts as not set.
    run(&mut sh, "export NO_COLOR=");
    assert!(run(&mut sh, "ls c").out.starts_with("[--color=auto][c]"));
    run(&mut sh, "unset NO_COLOR");
    assert!(run(&mut sh, "ls d").out.starts_with("[--color=auto][d]"));
}

#[test]
fn a_program_is_probed_once_for_every_copy_of_the_shell() {
    let fakes = Fakes::new();
    let mut sh = fakes.shell(true);
    run(&mut sh, "ls a");
    run(&mut sh, "ls b | cat");
    run(&mut sh, "x=$(ls c)");
    run(&mut sh, "( ls d )");
    run(&mut sh, "ls e >/dev/null & wait");
    assert_eq!(fakes.probes(), 1, "{:?}", fakes.calls());
    // Another program has its own answer.
    run(&mut sh, "grep a");
    run(&mut sh, "grep b | cat");
    assert_eq!(fakes.probes(), 2, "{:?}", fakes.calls());
}

#[test]
fn a_different_file_on_path_is_probed_on_its_own() {
    let first = Fakes::new();
    let second = Fakes::new();
    let mut sh = first.shell(true);
    run(&mut sh, "ls a");
    let path = format!("{}:/usr/bin:/bin", second.path().display());
    run(&mut sh, &format!("PATH={path}; export PATH"));
    run(&mut sh, "ls b");
    // The second fake logs to the first one's file, since `OMSH_LOG` did not change.
    assert_eq!(first.probes(), 2, "{:?}", first.calls());
}

#[test]
fn an_answer_that_cannot_be_trusted_is_not_remembered() {
    let fakes = Fakes::new();
    let mut sh = fakes.shell(true);
    let marker = fakes.path().join("flaky");
    sh.vars
        .set("OMSH_FLAKY", marker.as_os_str().as_encoded_bytes().to_vec())
        .unwrap();
    sh.vars.export("OMSH_FLAKY");
    // The first probe ends with status 130, as if a signal killed it: no color this time, and
    // nothing remembered.
    assert_eq!(run(&mut sh, "ls a").out, "[a]\nenv:unset/unset/unset\n");
    assert!(run(&mut sh, "ls b").out.starts_with("[--color=auto][b]"));
    assert_eq!(fakes.probes(), 2, "{:?}", fakes.calls());
}

#[test]
fn arguments_and_the_status_pass_through_unchanged() {
    let fakes = Fakes::new();
    let mut sh = fakes.shell(true);
    let done = run(&mut sh, "grep 'a b' '' -- '*' fail");
    assert_eq!(
        done.out,
        "[--color=auto][a b][][--][*][fail]\nenv:unset/unset/unset\n"
    );
    assert_eq!(done.status, 3);
}

#[test]
fn the_wrappers_work_under_unusual_options_and_ifs() {
    let fakes = Fakes::new();
    let mut sh = fakes.shell(true);
    let done = run(&mut sh, "set -u -f; IFS=a; ls 'x y' '*'");
    assert_eq!(
        done.out, "[--color=auto][x y][*]\nenv:unset/unset/unset\n",
        "{}",
        done.err
    );
    assert!(done.err.is_empty(), "{}", done.err);
}

#[test]
fn a_probe_that_says_no_cannot_end_the_shell_under_set_e() {
    let fakes = Fakes::new();
    let mut sh = fakes.shell(false);
    let done = run(&mut sh, "set -e; ls a; echo still-here");
    assert_eq!(done.out, "[a]\nenv:unset/unset/unset\nstill-here\n");
}

#[test]
fn the_probe_is_not_traced_and_leaves_the_status_alone() {
    let fakes = Fakes::new();
    let mut sh = fakes.shell(true);
    let done = run(&mut sh, "set -x; (exit 7); ls a");
    assert!(
        done.err.contains("+ ls --color=auto a"),
        "{}",
        probe_report(&fakes, &mut sh, &done)
    );
    assert!(!done.err.contains("command"), "{}", done.err);
    assert!(
        !done.err.contains("-q x") && !done.err.contains("-d /"),
        "{}",
        done.err
    );
}

#[test]
fn the_shell_sets_none_of_the_color_variables() {
    let fakes = Fakes::new();
    let mut sh = fakes.shell(true);
    let done = run(
        &mut sh,
        "echo ${LS_COLORS-unset} ${GREP_COLOR-unset} ${GREP_COLORS-unset}",
    );
    assert_eq!(done.out, "unset unset unset\n");
    let done = run(&mut sh, "ls a");
    assert!(
        done.out.ends_with("env:unset/unset/unset\n"),
        "{}",
        done.out
    );
}

#[test]
fn command_skips_the_wrapper() {
    let fakes = Fakes::new();
    let mut sh = fakes.shell(true);
    assert_eq!(
        run(&mut sh, "command ls a").out,
        "[a]\nenv:unset/unset/unset\n"
    );
    assert_eq!(fakes.probes(), 0);
}

#[test]
fn an_alias_a_function_and_unset_f_all_beat_the_wrapper() {
    let fakes = Fakes::new();
    let mut sh = fakes.shell(true);
    // An alias that replaces the program.
    run(&mut sh, "alias grep=echo");
    assert_eq!(run(&mut sh, "grep hi").out, "hi\n");
    // An alias that adds a flag keeps the wrapper: the user's flags come last.
    run(&mut sh, "alias ls='ls -l'");
    assert_eq!(
        run(&mut sh, "ls a").out,
        "[--color=auto][-l][a]\nenv:unset/unset/unset\n"
    );
    // A function of the same name.
    assert_eq!(run(&mut sh, "diff() { echo mine; }; diff").out, "mine\n");
    // Removing the wrapper leaves the plain program.
    run(&mut sh, "unalias ls");
    assert_eq!(
        run(&mut sh, "unset -f ls; ls a").out,
        "[a]\nenv:unset/unset/unset\n"
    );
}

#[test]
fn a_program_that_is_not_there_gives_the_usual_error() {
    let fakes = Fakes::new();
    let mut sh = fakes.shell(true);
    run(&mut sh, "PATH=/nonexistent-dir");
    let done = run(&mut sh, "ls a");
    assert_eq!(done.status, 127);
    assert!(done.err.contains("ls"), "{}", done.err);
    assert_eq!(fakes.probes(), 0);
}

#[test]
fn the_internal_word_needs_a_name() {
    let fakes = Fakes::new();
    let mut sh = fakes.shell(true);
    let done = run(&mut sh, "__omsh_color");
    assert_eq!(done.status, 2);
    assert!(done.err.contains("needs a command name"), "{}", done.err);
}

#[test]
fn the_wrappers_exist_only_after_they_are_defined() {
    let plain =
        Shell::with_variables(Variables::from_pairs([("PATH", "/usr/bin:/bin")])).subshell();
    assert!(plain.function_names().is_empty());
    let fakes = Fakes::new();
    let mut names = fakes.shell(true).function_names();
    names.sort();
    assert_eq!(names, ["diff", "egrep", "fgrep", "grep", "ls"]);
}

/// The wait for a fake that is "text file busy": a handle open for writing makes `exec` fail
/// until it is dropped, and the wait must go on trying until then. Without the retry the first
/// busy answer would end the test run (the mutant "no retry" fails this test).
#[test]
fn a_fake_that_is_busy_for_a_while_is_waited_for() {
    let dir = std::env::temp_dir().join(format!("omsh-busy-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let file = dir.join("ls");
    fs::write(&file, FAKE).unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();

    // Proof that the file really is busy while the handle is open.
    let holder = fs::OpenOptions::new().append(true).open(&file).unwrap();
    let busy = Command::new(&file).output().unwrap_err();
    assert_eq!(busy.kind(), std::io::ErrorKind::ExecutableFileBusy);

    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        drop(holder);
    });
    let started = Instant::now();
    wait_until_runnable(&file);
    assert!(
        started.elapsed() >= Duration::from_millis(100),
        "it returned while the file was still busy: {:?}",
        started.elapsed()
    );
    release.join().unwrap();
    let _ = fs::remove_dir_all(&dir);
}
