use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::AtomicBool;

use super::*;

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("omsh-gs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    /// A repository with `HEAD` on branch `main`, and the path of its work tree.
    fn repo(&self) -> PathBuf {
        let root = self.0.join("repo");
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        root
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Something a test can hold back and let go.
#[derive(Default)]
struct Gate {
    open: Mutex<bool>,
    changed: Condvar,
}

impl Gate {
    fn open(&self) {
        *lock(&self.open) = true;
        self.changed.notify_all();
    }

    /// True if the gate opened; false if `cancelled` or `end` came first.
    fn wait(&self, end: Option<Instant>, cancelled: Option<&dyn Fn() -> bool>) -> bool {
        let mut open = lock(&self.open);
        loop {
            if *open {
                return true;
            }
            if cancelled.is_some_and(|cancelled| cancelled())
                || end.is_some_and(|end| Instant::now() >= end)
            {
                return false;
            }
            open = self
                .changed
                .wait_timeout(open, Duration::from_millis(5))
                .unwrap()
                .0;
        }
    }
}

struct Fake {
    answer: Dirtiness,
    gate: Option<Arc<Gate>>,
    /// Ignores the deadline and the cancel flag, like a call stuck in the kernel.
    stubborn: bool,
    calls: AtomicUsize,
    gave_up: AtomicBool,
}

impl Fake {
    fn new(answer: Dirtiness, gate: Option<&Arc<Gate>>, stubborn: bool) -> Arc<Self> {
        Arc::new(Self {
            answer,
            gate: gate.map(Arc::clone),
            stubborn,
            calls: AtomicUsize::new(0),
            gave_up: AtomicBool::new(false),
        })
    }
}

impl GitRunner for Fake {
    fn dirt(
        &self,
        _root: &Path,
        _env: &[(OsString, OsString)],
        deadline: Instant,
        cancelled: &dyn Fn() -> bool,
    ) -> Dirtiness {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(gate) = &self.gate {
            let opened = if self.stubborn {
                gate.wait(None, None)
            } else {
                gate.wait(Some(deadline), Some(cancelled))
            };
            if !opened {
                self.gave_up.store(true, Ordering::SeqCst);
                return Dirtiness::Unknown;
            }
        }
        self.answer
    }
}

fn counter() -> (Arc<AtomicUsize>, Arc<dyn Fn() + Send + Sync>) {
    let count = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&count);
    (
        count,
        Arc::new(move || {
            seen.fetch_add(1, Ordering::SeqCst);
        }),
    )
}

/// Waits (at most two seconds) until `ready` is true.
fn until(what: &str, ready: impl Fn() -> bool) {
    let end = Instant::now() + Duration::from_secs(2);
    while !ready() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(2));
    }
}

fn head_known(handle: &Handle) -> bool {
    matches!(handle.snapshot().head, HeadState::Known(_))
}

const LONG: Duration = Duration::from_secs(5);

#[test]
fn a_quick_answer_is_there_before_the_first_paint() {
    let s = Scratch::new("quick");
    let repo = s.repo();
    let fake = Fake::new(Dirtiness::Dirty, None, false);
    let (repaints, repaint) = counter();
    let watch = GitWatch::with(fake, repaint, LONG);
    let handle = watch.start(&repo, &[]);
    watch.wait(&handle, Duration::from_secs(2));
    let snapshot = handle.snapshot();
    assert_eq!(snapshot.text().as_deref(), Some("(main*)"));
    assert_eq!(repaints.load(Ordering::SeqCst), 0, "nothing to repaint");
}

#[test]
fn a_held_answer_does_not_hold_the_prompt_and_a_late_one_repaints_once() {
    let s = Scratch::new("late");
    let repo = s.repo();
    let gate = Arc::new(Gate::default());
    let fake = Fake::new(Dirtiness::Dirty, Some(&gate), false);
    let (repaints, repaint) = counter();
    let watch = GitWatch::with(fake, repaint, LONG);
    let handle = watch.start(&repo, &[]);
    until("the branch", || head_known(&handle));

    let before = Instant::now();
    watch.wait(&handle, Duration::from_millis(20));
    assert!(
        before.elapsed() < Duration::from_millis(500),
        "the wait is bounded"
    );
    // The prompt is drawn now: the branch, no marker.
    assert_eq!(handle.snapshot().text().as_deref(), Some("(main)"));
    assert_eq!(repaints.load(Ordering::SeqCst), 0);

    gate.open();
    until("the repaint", || repaints.load(Ordering::SeqCst) == 1);
    assert_eq!(handle.snapshot().text().as_deref(), Some("(main*)"));
    thread::sleep(Duration::from_millis(30));
    assert_eq!(repaints.load(Ordering::SeqCst), 1, "one repaint, not two");
}

#[test]
fn the_deadline_ends_a_job_that_does_not_answer() {
    let s = Scratch::new("deadline");
    let repo = s.repo();
    let gate = Arc::new(Gate::default());
    let fake = Fake::new(Dirtiness::Dirty, Some(&gate), false);
    let (_, repaint) = counter();
    let watch = GitWatch::with(
        Arc::clone(&fake) as Arc<dyn GitRunner>,
        repaint,
        Duration::from_millis(60),
    );
    let handle = watch.start(&repo, &[]);
    until("the unknown marker", || {
        handle.snapshot().dirt == Dirtiness::Unknown
    });
    assert!(fake.gave_up.load(Ordering::SeqCst));
    // Unknown: the branch alone.
    assert_eq!(handle.snapshot().text().as_deref(), Some("(main)"));
}

#[test]
fn a_new_prompt_hides_the_old_result_and_cancels_the_old_job() {
    let s = Scratch::new("newer");
    let repo = s.repo();
    let gate = Arc::new(Gate::default());
    let fake = Fake::new(Dirtiness::Dirty, Some(&gate), false);
    let (_, repaint) = counter();
    let watch = GitWatch::with(Arc::clone(&fake) as Arc<dyn GitRunner>, repaint, LONG);
    let old = watch.start(&repo, &[]);
    until("the first job to run", || {
        fake.calls.load(Ordering::SeqCst) == 1
    });
    let new = watch.start(&repo, &[]);
    until("the old job to give up", || {
        fake.gave_up.load(Ordering::SeqCst)
    });
    assert_eq!(
        old.snapshot(),
        Snapshot::nothing(),
        "the old prompt has nothing to say"
    );
    gate.open();
    until("the new answer", || new.snapshot().dirt == Dirtiness::Dirty);
    assert_eq!(old.snapshot(), Snapshot::nothing());
}

#[test]
fn a_job_that_is_stuck_blocks_the_next_start_until_it_ends() {
    let s = Scratch::new("stuck");
    let repo = s.repo();
    let gate = Arc::new(Gate::default());
    let fake = Fake::new(Dirtiness::Clean, Some(&gate), true);
    let (_, repaint) = counter();
    let watch = GitWatch::with(
        Arc::clone(&fake) as Arc<dyn GitRunner>,
        repaint,
        Duration::from_millis(30),
    );
    let first = watch.start(&repo, &[]);
    until("the first job to run", || {
        fake.calls.load(Ordering::SeqCst) == 1
    });
    thread::sleep(Duration::from_millis(60));

    let second = watch.start(&repo, &[]);
    assert_eq!(
        second.snapshot(),
        Snapshot::nothing(),
        "no segment while stuck"
    );
    thread::sleep(Duration::from_millis(30));
    assert_eq!(fake.calls.load(Ordering::SeqCst), 1, "no second thread");
    assert_eq!(watch.shared.alive.load(Ordering::SeqCst), 1);

    gate.open();
    until("the stuck job to end", || {
        watch.shared.alive.load(Ordering::SeqCst) == 0
    });
    let third = watch.start(&repo, &[]);
    watch.wait(&third, Duration::from_secs(2));
    assert_eq!(third.snapshot().text().as_deref(), Some("(main)"));
    assert_eq!(fake.calls.load(Ordering::SeqCst), 2);
    drop(first);
}

/// A job for `generation`, which never runs: only its `publish` is called.
fn idle_job(watch: &GitWatch, generation: u64) -> Job {
    Job {
        shared: Arc::clone(&watch.shared),
        runner: Arc::clone(&watch.runner),
        repaint: Arc::clone(&watch.repaint),
        generation,
        cwd: PathBuf::new(),
        env: Vec::new(),
        deadline: Instant::now(),
        uid: 0,
    }
}

#[test]
fn only_news_that_changes_something_repaints_and_only_the_newest_prompt_hears_it() {
    let s = Scratch::new("publish");
    let repo = s.repo();
    let gate = Arc::new(Gate::default());
    let fake = Fake::new(Dirtiness::Clean, Some(&gate), true);
    let (repaints, repaint) = counter();
    let watch = GitWatch::with(fake, repaint, LONG);
    let old = watch.start(&repo, &[]);
    let newest = watch.start(&repo, &[]);
    until("the branch", || head_known(&newest));
    watch.wait(&newest, Duration::from_millis(5));
    let before = newest.snapshot();

    // The job of the older prompt has news: it changes nothing and repaints nothing.
    let stale = idle_job(&watch, 1);
    stale.publish(|snapshot| snapshot.dirt = Dirtiness::Dirty);
    assert_eq!(newest.snapshot(), before);
    assert_eq!(repaints.load(Ordering::SeqCst), 0);
    assert_eq!(old.snapshot(), Snapshot::nothing());

    // The newest prompt's job says what is already known: no repaint.
    let current = idle_job(&watch, 2);
    current.publish(|_| {});
    current.publish(|snapshot| snapshot.head = before.head.clone());
    assert_eq!(repaints.load(Ordering::SeqCst), 0);

    // New news: one repaint.
    current.publish(|snapshot| snapshot.dirt = Dirtiness::Dirty);
    assert_eq!(repaints.load(Ordering::SeqCst), 1);
    assert_eq!(newest.snapshot().text().as_deref(), Some("(main*)"));
    gate.open();
}

#[test]
fn no_more_than_a_few_helper_threads_exist_at_once() {
    let s = Scratch::new("cap");
    let repo = s.repo();
    let gate = Arc::new(Gate::default());
    let fake = Fake::new(Dirtiness::Clean, Some(&gate), true);
    let (_, repaint) = counter();
    // A long deadline: none of the jobs is overdue, so only the count can refuse a start.
    let watch = GitWatch::with(Arc::clone(&fake) as Arc<dyn GitRunner>, repaint, LONG);
    // Each job is let into the runner (where it sticks) before the next prompt is asked for;
    // a job that is not there yet is cancelled by the next start and ends at once.
    for started in 0..(MAX_ALIVE + 3) {
        watch.start(&repo, &[]);
        if started < MAX_ALIVE {
            until("a helper to reach git", || {
                fake.calls.load(Ordering::SeqCst) == started + 1
            });
        }
    }
    thread::sleep(Duration::from_millis(50));
    assert_eq!(fake.calls.load(Ordering::SeqCst), MAX_ALIVE);
    assert_eq!(watch.shared.alive.load(Ordering::SeqCst), MAX_ALIVE);
    gate.open();
    until("the helpers to end", || {
        watch.shared.alive.load(Ordering::SeqCst) == 0
    });
}

#[test]
fn no_repository_means_nothing_and_git_is_not_asked() {
    let s = Scratch::new("none");
    let plain = s.0.join("plain");
    fs::create_dir_all(&plain).unwrap();
    let fake = Fake::new(Dirtiness::Dirty, None, false);
    let (repaints, repaint) = counter();
    let watch = GitWatch::with(Arc::clone(&fake) as Arc<dyn GitRunner>, repaint, LONG);
    let handle = watch.start(&plain, &[]);
    watch.wait(&handle, Duration::from_secs(2));
    assert_eq!(handle.snapshot(), Snapshot::nothing());
    assert_eq!(handle.snapshot().text(), None);
    assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
    assert_eq!(repaints.load(Ordering::SeqCst), 0);
}

#[test]
fn a_repository_of_another_user_is_not_asked() {
    let s = Scratch::new("owner");
    let repo = s.repo();
    let found = find_repo(&repo).unwrap();
    let me = geteuid().as_raw();
    assert!(owned_by(&found, me));
    assert!(!owned_by(&found, me + 1), "a different owner");
    // A work tree that is mine with a git directory that is not (or the reverse) is refused too:
    // a missing directory has no owner.
    let mut gone = found.clone();
    gone.git_dir = s.0.join("nowhere");
    assert!(!owned_by(&gone, me));
    let mut gone = found;
    gone.root = s.0.join("nowhere");
    assert!(!owned_by(&gone, me));
}

#[test]
fn git_is_never_run_in_a_repository_of_another_user_and_the_branch_is_still_shown() {
    let s = Scratch::new("stranger");
    let repo = s.repo();
    let fake = Fake::new(Dirtiness::Dirty, None, false);
    let (repaints, repaint) = counter();
    let watch = GitWatch::with(Arc::clone(&fake) as Arc<dyn GitRunner>, repaint, LONG)
        .running_as(geteuid().as_raw() + 1);
    let handle = watch.start(&repo, &[]);
    watch.wait(&handle, Duration::from_secs(2));
    assert_eq!(handle.snapshot().text().as_deref(), Some("(main)"));
    assert_eq!(handle.snapshot().dirt, Dirtiness::Unknown);
    assert_eq!(fake.calls.load(Ordering::SeqCst), 0, "git was not started");
    assert_eq!(repaints.load(Ordering::SeqCst), 0);
}

/// The numbers of blueprint §10 and the owner's rule: 150 ms for the whole job, and a first paint
/// that waits only a moment.
#[test]
fn the_time_limits_are_the_specified_ones() {
    assert_eq!(DEADLINE, Duration::from_millis(150));
    assert_eq!(FIRST_PAINT_WAIT, Duration::from_millis(20));
}

#[test]
fn the_segment_text_has_a_marker_only_for_a_known_change() {
    let head = || HeadState::Known(Head::Branch("dev".into()));
    let text = |head, dirt| Snapshot { head, dirt }.text();
    assert_eq!(text(head(), Dirtiness::Dirty).as_deref(), Some("(dev*)"));
    for dirt in [Dirtiness::Clean, Dirtiness::Pending, Dirtiness::Unknown] {
        assert_eq!(text(head(), dirt).as_deref(), Some("(dev)"));
    }
    assert_eq!(text(HeadState::Pending, Dirtiness::Dirty), None);
    assert_eq!(text(HeadState::Absent, Dirtiness::Dirty), None);
    let detached = HeadState::Known(Head::Detached("0123456".into()));
    assert_eq!(
        text(detached, Dirtiness::Dirty).as_deref(),
        Some("(0123456*)")
    );
}

#[test]
fn a_prompt_reads_the_git_segment_when_it_is_drawn() {
    use reedline::Prompt;

    use crate::prompt::{Look, PromptState, ShellPrompt};
    use crate::segments::Segment;

    let s = Scratch::new("drawn");
    let repo = s.repo();
    let gate = Arc::new(Gate::default());
    let fake = Fake::new(Dirtiness::Dirty, Some(&gate), false);
    let (_, repaint) = counter();
    let watch = GitWatch::with(fake, repaint, LONG);
    let handle = watch.start(&repo, &[]);
    until("the branch", || head_known(&handle));
    watch.wait(&handle, Duration::from_millis(5));

    let look = Look {
        unicode: true,
        color: false,
        left: vec![Segment::Git],
        right: vec![],
    };
    let state = PromptState {
        cwd: repo,
        home: None,
        virtual_env: None,
        conda_env: None,
        last_status: 0,
        last_duration: None,
        root: false,
        git_env: Vec::new(),
    };
    let prompt = ShellPrompt::new(&look, &state, String::new).with_git(handle);
    // One prompt object, drawn twice: what the helper learned in between shows the second time.
    assert_eq!(prompt.render_prompt_left(), "\x1b[2m(main)\x1b[0m\n");
    gate.open();
    until("the marker", || {
        prompt.render_prompt_left() == "\x1b[2m(main*)\x1b[0m\n"
    });
}

// --- The real runner, with a fake `git` script ---------------------------------------------

/// Programs are written and started one at a time here: a thread that forks while another holds
/// a script open for writing makes `exec` fail with "text file busy".
static SCRIPTS: Mutex<()> = Mutex::new(());

/// A directory with a `git` that runs `body`, and the environment that finds it.
fn fake_git(s: &Scratch, body: &str) -> Env {
    let bin = s.0.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let file = bin.join("git");
    fs::write(&file, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
    vec![
        (
            "PATH".into(),
            format!("{}:/usr/bin:/bin", bin.display()).into(),
        ),
        ("SECRET_TOKEN".into(), "hunter2".into()),
        ("GIT_DIR".into(), "/elsewhere".into()),
        ("HOME".into(), "/home/nobody".into()),
    ]
}

fn run(env: &Env, root: &Path, ms: u64) -> Dirtiness {
    ProcessRunner.dirt(
        root,
        env,
        Instant::now() + Duration::from_millis(ms),
        &|| false,
    )
}

#[test]
fn a_change_in_the_output_is_dirty_and_the_command_line_is_the_safe_one() {
    let _one = lock(&SCRIPTS);
    let s = Scratch::new("run-dirty");
    let root = s.repo();
    let out = s.0.display();
    let env = fake_git(
        &s,
        &format!(
            "printf '%s\\n' \"$@\" > {out}/args\nenv > {out}/env\npwd > {out}/cwd\n\
             echo \" M file\""
        ),
    );
    assert_eq!(run(&env, &root, 2000), Dirtiness::Dirty);

    let args = fs::read_to_string(s.0.join("args")).unwrap();
    let args: Vec<&str> = args.lines().collect();
    assert_eq!(
        args,
        [
            "--no-optional-locks",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "protocol.allow=never",
            "status",
            "--porcelain=v1",
            "--untracked-files=normal",
            "--ignore-submodules=dirty",
            "--no-renames",
        ]
    );
    let seen = fs::read_to_string(s.0.join("env")).unwrap();
    for wanted in [
        "GIT_OPTIONAL_LOCKS=0",
        "GIT_TERMINAL_PROMPT=0",
        "GIT_NO_LAZY_FETCH=1",
        "LC_ALL=C",
        "HOME=/home/nobody",
    ] {
        assert!(
            seen.lines().any(|line| line == wanted),
            "{wanted} missing:\n{seen}"
        );
    }
    assert!(
        !seen.contains("SECRET_TOKEN"),
        "the shell's environment is not passed:\n{seen}"
    );
    assert!(
        !seen.contains("/elsewhere"),
        "GIT_DIR is not passed:\n{seen}"
    );
    // Nothing of this test process's own environment (cargo sets dozens of variables) reaches
    // git: it holds the variables passed in, the ones the runner sets, and what `sh` adds.
    let names: Vec<&str> = seen
        .lines()
        .filter_map(|line| line.split_once('=').map(|(name, _)| name))
        .collect();
    for name in &names {
        assert!(
            [
                "PATH",
                "HOME",
                "GIT_OPTIONAL_LOCKS",
                "GIT_TERMINAL_PROMPT",
                "GIT_NO_LAZY_FETCH",
                "LC_ALL",
                "PWD",
                "OLDPWD",
                "SHLVL",
                "_"
            ]
            .contains(name),
            "{name} reached git:\n{seen}"
        );
    }
    let cwd = fs::read_to_string(s.0.join("cwd")).unwrap();
    assert_eq!(Path::new(cwd.trim()), fs::canonicalize(&root).unwrap());
}

#[test]
fn no_output_and_success_is_clean_and_a_failure_is_unknown() {
    let _one = lock(&SCRIPTS);
    let s = Scratch::new("run-clean");
    let root = s.repo();
    let env = fake_git(&s, "exit 0");
    assert_eq!(run(&env, &root, 2000), Dirtiness::Clean);
    let env = fake_git(&s, "exit 128");
    assert_eq!(run(&env, &root, 2000), Dirtiness::Unknown);
    // Output and then a failure still counts as a change: the first byte decides.
    let env = fake_git(&s, "echo '?? new'; exit 1");
    assert_eq!(run(&env, &root, 2000), Dirtiness::Dirty);
}

/// A `PATH` entry that is not absolute (`.`, the empty entry, `bin`) would make the prompt run a
/// `git` that sits inside the repository it is looking at, which is whatever its owner put there.
#[test]
fn a_git_inside_the_repository_is_never_run_through_a_relative_path_entry() {
    let _one = lock(&SCRIPTS);
    let s = Scratch::new("run-relative");
    let root = s.repo();
    let mark = s.0.join("planted-git-ran");
    for entry in ["planted", ".", ""] {
        let dir = if entry.is_empty() || entry == "." {
            root.clone()
        } else {
            root.join(entry)
        };
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("git");
        fs::write(
            &file,
            format!("#!/bin/sh\ntouch {}\necho ' M x'\n", mark.display()),
        )
        .unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
        let env: Env = vec![("PATH".into(), entry.into())];
        assert_eq!(
            run(&env, &root, 2000),
            Dirtiness::Unknown,
            "entry {entry:?}"
        );
        assert!(
            !mark.exists(),
            "the planted git ran through the entry {entry:?}"
        );
    }
}

/// The same rule at the place it is decided: a `PATH` entry that is relative to this process's
/// folder is not searched even where a `git` really is.
#[test]
fn a_relative_path_entry_is_not_searched_even_where_a_git_is() {
    let _one = lock(&SCRIPTS);
    let s = Scratch::new("find-relative");
    let env = fake_git(&s, "exit 0");
    let absolute = s.0.join("bin");
    assert_eq!(
        find_git(&env),
        Some(absolute.join("git")),
        "the control: found by its absolute entry"
    );

    // The same directory, named relative to the folder the tests run in.
    let here = std::env::current_dir().unwrap();
    let up = "../".repeat(here.components().count() - 1);
    let relative = format!("{up}{}", absolute.strip_prefix("/").unwrap().display());
    assert!(here.join(&relative).join("git").exists(), "{relative}");
    let env: Env = vec![("PATH".into(), relative.into())];
    assert_eq!(find_git(&env), None);
}

#[test]
fn a_missing_git_is_unknown() {
    let s = Scratch::new("run-missing");
    let root = s.repo();
    let env: Env = vec![("PATH".into(), s.0.join("empty").into())];
    assert_eq!(run(&env, &root, 2000), Dirtiness::Unknown);
    // A relative entry of PATH is never searched.
    let env: Env = vec![("PATH".into(), "bin:.".into())];
    assert_eq!(run(&env, &root, 2000), Dirtiness::Unknown);
}

#[test]
fn a_git_that_hangs_is_killed_at_the_deadline_and_collected() {
    let _one = lock(&SCRIPTS);
    let s = Scratch::new("run-hang");
    let root = s.repo();
    let out = s.0.display();
    let env = fake_git(
        &s,
        &format!(
            "echo $$ > {out}/pid\nawk '{{print $5}}' /proc/$$/stat > {out}/pgid\nexec sleep 30"
        ),
    );
    let before = Instant::now();
    assert_eq!(run(&env, &root, 150), Dirtiness::Unknown);
    let took = before.elapsed();
    assert!(took < Duration::from_millis(1000), "took {took:?}");

    let pid = fs::read_to_string(s.0.join("pid")).unwrap();
    let pid = pid.trim();
    assert!(
        !Path::new(&format!("/proc/{pid}")).exists(),
        "the process was collected"
    );
    let pgid = fs::read_to_string(s.0.join("pgid")).unwrap();
    assert_eq!(pgid.trim(), pid, "git leads a process group of its own");
}

#[test]
fn a_cancel_ends_git_at_once() {
    let _one = lock(&SCRIPTS);
    let s = Scratch::new("run-cancel");
    let root = s.repo();
    let env = fake_git(&s, "exec sleep 30");
    let start = Instant::now();
    let cancelled = || start.elapsed() > Duration::from_millis(30);
    let answer = ProcessRunner.dirt(&root, &env, start + LONG, &cancelled);
    assert_eq!(answer, Dirtiness::Unknown);
    assert!(start.elapsed() < Duration::from_millis(1000));
}
