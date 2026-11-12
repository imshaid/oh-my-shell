use std::fs;
use std::os::unix::fs::PermissionsExt;

use super::*;

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

fn defaults() -> Notify {
    Notify::default()
}

fn decide_default(line: &str, took: u64, status: u8) -> Option<Note> {
    decide(line, secs(took), status, &defaults(), &[], true)
}

// ---- decide ----

#[test]
fn a_command_that_ran_ten_seconds_notifies_and_one_second_less_does_not() {
    assert!(decide_default("make", 10, 0).is_some());
    assert!(decide_default("make", 9, 0).is_none());
    let mut n = defaults();
    n.min_seconds = 3;
    assert!(decide("make", secs(3), 0, &n, &[], true).is_some());
    assert!(decide("make", secs(2), 0, &n, &[], true).is_none());
}

#[test]
fn nothing_is_shown_when_notifications_are_off() {
    let mut n = defaults();
    n.enabled = false;
    assert!(decide("make", secs(600), 0, &n, &[], true).is_none());
}

#[test]
fn ctrl_c_and_ctrl_z_never_notify_but_other_failures_do() {
    assert!(decide_default("sleep 99", 20, 130).is_none());
    assert!(decide_default("sleep 99", 20, 148).is_none());
    assert!(decide_default("sleep 99", 20, 129).is_some());
    assert!(decide_default("sleep 99", 20, 137).is_some());
}

#[test]
fn a_request_to_the_ai_and_an_empty_line_never_notify() {
    assert!(decide_default("? what is here", 60, 0).is_none());
    assert!(decide_default("?? ls", 60, 0).is_none());
    assert!(decide_default("ls | ? why", 60, 0).is_none());
    assert!(decide_default("   ", 60, 0).is_none());
}

#[test]
fn the_title_says_done_or_failed_and_how_long() {
    let done = decide_default("make", 72, 0).unwrap();
    assert_eq!(done.title, "✓ Done in 1m 12s");
    let failed = decide_default("make", 3725, 2).unwrap();
    assert_eq!(failed.title, "✗ Failed (status 2) after 1h 02m");
    let plain = decide("make", secs(12), 1, &defaults(), &[], false).unwrap();
    assert_eq!(plain.title, "Failed (status 1) after 12s");
    let plain = decide("make", secs(65), 0, &defaults(), &[], false).unwrap();
    assert_eq!(plain.title, "Done in 1m 05s");
}

#[test]
fn the_body_is_the_line_with_secrets_masked_and_blanks_made_one() {
    let note = decide_default(
        "curl  -H 'Authorization: Bearer abcdef0123456789abcdef'\n  url",
        20,
        0,
    )
    .unwrap();
    assert!(note.body.contains("[MASKED]"), "{}", note.body);
    assert!(!note.body.contains("abcdef0123456789"), "{}", note.body);
    assert!(!note.body.contains('\n'));
    assert!(!note.body.contains("  "));
}

#[test]
fn control_characters_never_reach_the_notification() {
    let note = decide_default("echo a\x1b]0;evil\x07b", 20, 0).unwrap();
    assert!(!note.body.chars().any(char::is_control), "{:?}", note.body);
}

#[test]
fn a_long_line_is_cut_with_dots() {
    let note = decide_default(&format!("echo {}", "x".repeat(500)), 20, 0).unwrap();
    assert_eq!(note.body.chars().count(), BODY_CHARS);
    assert!(note.body.ends_with("..."));
}

#[test]
fn an_interactive_program_never_notifies_by_its_name_or_its_path() {
    for line in [
        "vim notes.txt",
        "/usr/bin/vim x",
        "'vim' x",
        "ssh host",
        "less log",
        "man ls",
        "htop",
        "tmux attach",
    ] {
        assert!(decide_default(line, 60, 0).is_none(), "{line}");
    }
}

#[test]
fn a_program_after_a_wrapper_counts_too() {
    for line in [
        "sudo vim /etc/hosts",
        "sudo -u root less x",
        "env A=1 vim",
        "time less x",
        "command vim",
    ] {
        assert!(decide_default(line, 60, 0).is_none(), "{line}");
    }
    // The same words as arguments of an ordinary program are only arguments.
    assert!(decide_default("git commit -m vim", 60, 0).is_some());
    assert!(decide_default("cat vim", 60, 0).is_some());
}

#[test]
fn any_command_of_a_line_can_skip_it() {
    assert!(decide_default("cat log | less", 60, 0).is_none());
    assert!(decide_default("make && vim x", 60, 0).is_none());
    assert!(decide_default("(cd x; vim y)", 60, 0).is_none());
    assert!(decide_default("make && echo done", 60, 0).is_some());
}

#[test]
fn an_alias_is_looked_through() {
    let aliases = vec![
        ("e".to_owned(), "vim".to_owned()),
        ("ll".to_owned(), "ls -l".to_owned()),
    ];
    assert!(decide("e file", secs(60), 0, &defaults(), &aliases, true).is_none());
    assert!(decide("ll", secs(60), 0, &defaults(), &aliases, true).is_some());
}

#[test]
fn an_alias_that_names_itself_does_not_loop() {
    let aliases = vec![("ls".to_owned(), "ls --color".to_owned())];
    assert!(decide("ls", secs(60), 0, &defaults(), &aliases, true).is_some());
    let aliases = vec![
        ("a".to_owned(), "b".to_owned()),
        ("b".to_owned(), "a".to_owned()),
    ];
    assert!(decide("a", secs(60), 0, &defaults(), &aliases, true).is_some());
}

#[test]
fn a_word_with_an_expansion_names_no_program() {
    assert!(decide_default("$EDITOR x", 60, 0).is_some());
    assert!(decide_default("$(which vim) x", 60, 0).is_some());
}

#[test]
fn the_skip_list_is_the_one_in_the_config() {
    let mut n = defaults();
    n.ignore = vec!["make".to_owned()];
    assert!(decide("make", secs(60), 0, &n, &[], true).is_none());
    assert!(decide("vim x", secs(60), 0, &n, &[], true).is_some());
}

// ---- plan ----

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("omsh-nt-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    /// An executable script called `name` in a folder of its own; returns the folder.
    fn program(&self, name: &str, body: &str) -> PathBuf {
        let dir = self.0.join("bin");
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join(name);
        fs::write(&file, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
        dir
    }

    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn vars(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
    pairs
        .iter()
        .map(|(k, v)| (OsString::from(k), OsString::from(v)))
        .collect()
}

fn desktop(bin: &Path) -> Vec<(OsString, OsString)> {
    vars(&[
        ("PATH", &format!("{}:/usr/bin:/bin", bin.display())),
        ("HOME", "/home/x"),
        ("DISPLAY", ":0"),
        ("DBUS_SESSION_BUS_ADDRESS", "unix:path=/run/user/1000/bus"),
    ])
}

#[test]
fn a_desktop_with_the_program_can_show_a_notification() {
    let s = Scratch::new("plan-ok");
    let bin = s.program("notify-send", "exit 0");
    let delivery = plan(&desktop(&bin)).unwrap();
    assert_eq!(delivery.program, bin.join("notify-send"));
    // Only the variables the program needs.
    let names: Vec<_> = delivery
        .env
        .iter()
        .map(|(k, _)| k.to_str().unwrap())
        .collect();
    assert_eq!(
        names,
        ["PATH", "HOME", "DISPLAY", "DBUS_SESSION_BUS_ADDRESS"]
    );
}

#[test]
fn over_ssh_there_is_no_notification_even_with_a_forwarded_display() {
    let s = Scratch::new("plan-ssh");
    let bin = s.program("notify-send", "exit 0");
    let mut v = desktop(&bin);
    v.extend(vars(&[("SSH_CONNECTION", "1.2.3.4 5 6.7.8.9 22")]));
    assert_eq!(plan(&v), Err(Unavailable::Ssh));
    let mut v = desktop(&bin);
    v.extend(vars(&[("SSH_TTY", "/dev/pts/3")]));
    assert_eq!(plan(&v), Err(Unavailable::Ssh));
}

#[test]
fn without_a_display_a_bus_or_the_program_the_reason_is_named() {
    let s = Scratch::new("plan-missing");
    let bin = s.program("notify-send", "exit 0");
    let path = format!("{}:/usr/bin:/bin", bin.display());
    assert_eq!(
        plan(&vars(&[("PATH", &path), ("DBUS_SESSION_BUS_ADDRESS", "x")])),
        Err(Unavailable::NoDisplay)
    );
    assert_eq!(
        plan(&vars(&[("PATH", &path), ("DISPLAY", ":0")])),
        Err(Unavailable::NoBus)
    );
    // A bus socket in the runtime directory is as good as the variable.
    let runtime = s.0.join("run");
    fs::create_dir_all(&runtime).unwrap();
    fs::write(runtime.join("bus"), "").unwrap();
    assert!(
        plan(&vars(&[
            ("PATH", &path),
            ("WAYLAND_DISPLAY", "wayland-0"),
            ("XDG_RUNTIME_DIR", runtime.to_str().unwrap()),
        ]))
        .is_ok()
    );
    // An empty value is no value.
    assert_eq!(
        plan(&vars(&[
            ("PATH", &path),
            ("DISPLAY", ""),
            ("DBUS_SESSION_BUS_ADDRESS", "x")
        ])),
        Err(Unavailable::NoDisplay)
    );
    let empty = s.0.join("empty");
    fs::create_dir_all(&empty).unwrap();
    assert_eq!(
        plan(&vars(&[
            ("PATH", empty.to_str().unwrap()),
            ("DISPLAY", ":0"),
            ("DBUS_SESSION_BUS_ADDRESS", "x"),
        ])),
        Err(Unavailable::NoProgram)
    );
}

#[test]
fn a_program_that_is_not_executable_or_on_a_relative_path_is_not_used() {
    let s = Scratch::new("plan-mode");
    let bin = s.program("notify-send", "exit 0");
    fs::set_permissions(bin.join("notify-send"), fs::Permissions::from_mode(0o644)).unwrap();
    // A PATH of this folder only: the machine running the test may have a real notify-send.
    let mut v = desktop(&bin);
    v[0].1 = bin.clone().into_os_string();
    assert_eq!(plan(&v), Err(Unavailable::NoProgram));
    let mut v = desktop(&bin);
    v[0].1 = OsString::from("bin:relative");
    assert_eq!(plan(&v), Err(Unavailable::NoProgram));
}

// ---- Delivery::run ----

fn note() -> Note {
    Note {
        title: "✓ Done in 12s".to_owned(),
        body: "make -j8".to_owned(),
    }
}

fn never() -> bool {
    false
}

#[test]
fn the_program_gets_the_title_and_the_body_as_separate_arguments() {
    let s = Scratch::new("run-args");
    let out = s.file("args");
    let bin = s.program(
        "notify-send",
        &format!(
            "for a in \"$@\"; do printf '%s\\n' \"$a\"; done > {}",
            out.display()
        ),
    );
    let delivery = plan(&desktop(&bin)).unwrap();
    delivery
        .run(&note(), Duration::from_secs(5), &never)
        .unwrap();
    let text = fs::read_to_string(out).unwrap();
    assert_eq!(
        text,
        "--app-name=Oh My Shell\n--icon=utilities-terminal\n--\n✓ Done in 12s\nmake -j8\n"
    );
}

#[test]
fn the_program_sees_only_the_variables_it_was_given() {
    let s = Scratch::new("run-env");
    let out = s.file("env");
    let bin = s.program("notify-send", &format!("/usr/bin/env > {}", out.display()));
    let mut v = desktop(&bin);
    v.extend(vars(&[("SECRET_TOKEN", "hunter2"), ("API_KEY", "abc")]));
    plan(&v)
        .unwrap()
        .run(&note(), Duration::from_secs(5), &never)
        .unwrap();
    let text = fs::read_to_string(out).unwrap();
    assert!(text.contains("DISPLAY=:0"), "{text}");
    assert!(
        !text.contains("hunter2") && !text.contains("API_KEY"),
        "{text}"
    );
}

/// Regression test: `exec` of a file that a process still holds open for writing fails with
/// "text file busy". That showed up as a flake when tests wrote fake programs on several threads
/// at once (14 of 60 runs), and it is what a package upgrade of `notify-send` looks like.
#[test]
fn a_program_that_is_still_being_written_is_tried_again_for_a_moment() {
    use std::io::Write;
    let s = Scratch::new("run-busy");
    let bin = s.program("notify-send", "exit 0");
    let file = bin.join("notify-send");
    // Hold the file open for writing, as a writer that is not done does.
    let writer = fs::OpenOptions::new().append(true).open(&file).unwrap();
    let delivery = plan(&desktop(&bin)).unwrap();
    let release = thread::spawn(move || {
        thread::sleep(Duration::from_millis(80));
        let mut writer = writer;
        let _ = writer.flush();
        drop(writer);
    });
    let result = delivery.run(&note(), Duration::from_secs(5), &never);
    release.join().unwrap();
    assert_eq!(result, Ok(()));
}

#[test]
fn a_program_that_stays_busy_is_given_up_on_with_the_reason() {
    let s = Scratch::new("run-busy-forever");
    let bin = s.program("notify-send", "exit 0");
    let _writer = fs::OpenOptions::new()
        .append(true)
        .open(bin.join("notify-send"))
        .unwrap();
    let started = Instant::now();
    let error = plan(&desktop(&bin))
        .unwrap()
        .run(&note(), Duration::from_secs(5), &never)
        .unwrap_err();
    assert!(error.contains("could not be started"), "{error}");
    assert!(error.contains("busy"), "{error}");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn a_program_that_fails_is_reported_with_its_status() {
    let s = Scratch::new("run-fail");
    let bin = s.program("notify-send", "exit 3");
    let error = plan(&desktop(&bin))
        .unwrap()
        .run(&note(), Duration::from_secs(5), &never)
        .unwrap_err();
    assert_eq!(error, "notify-send ended with status 3");
}

/// True while a process whose command line holds `marker` exists (read from /proc).
fn alive(marker: &str) -> bool {
    fs::read_dir("/proc").unwrap().flatten().any(|entry| {
        fs::read(entry.path().join("cmdline"))
            .is_ok_and(|cmd| String::from_utf8_lossy(&cmd).contains(marker))
    })
}

#[test]
fn a_program_that_hangs_is_killed_with_its_group_at_the_deadline() {
    let s = Scratch::new("run-hang");
    let marker = format!("omsh-nt-hang-{}", std::process::id());
    // The script starts a child that would outlive it.
    let bin = s.program(
        "notify-send",
        &format!("sh -c 'sleep 30; true' {marker} & wait"),
    );
    let started = Instant::now();
    let error = plan(&desktop(&bin))
        .unwrap()
        .run(&note(), Duration::from_millis(150), &never)
        .unwrap_err();
    assert_eq!(error, "notify-send did not answer in time");
    assert!(started.elapsed() < Duration::from_secs(5));
    // The child is gone too (the group was killed).
    let until = Instant::now() + Duration::from_secs(3);
    while alive(&marker) && Instant::now() < until {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(!alive(&marker), "the grandchild is still running");
}

#[test]
fn a_cancel_ends_the_program_at_once() {
    let s = Scratch::new("run-cancel");
    // The program says it has started; only then is the cancel true.
    let started_file = s.file("started");
    let bin = s.program(
        "notify-send",
        &format!(": > {}; sleep 30", started_file.display()),
    );
    let started = Instant::now();
    let error = plan(&desktop(&bin))
        .unwrap()
        .run(&note(), Duration::from_secs(30), &|| started_file.exists())
        .unwrap_err();
    assert_eq!(error, "cancelled");
    assert!(started.elapsed() < Duration::from_secs(5));
}

// ---- Notifier ----

/// Waits until `check` holds, at most five seconds.
fn wait_until(what: &str, check: impl Fn() -> bool) {
    let until = Instant::now() + Duration::from_secs(5);
    while !check() {
        assert!(Instant::now() < until, "timed out: {what}");
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_notification_is_shown_on_a_helper_thread() {
    let s = Scratch::new("n-ok");
    let out = s.file("count");
    let bin = s.program("notify-send", &format!("echo x >> {}", out.display()));
    let notifier = Notifier::new(None);
    notifier.send(note(), &desktop(&bin));
    wait_until("the program ran", || out.exists());
    assert_eq!(notifier.take_message(), None);
}

#[test]
fn the_notifier_returns_before_a_slow_program_is_done() {
    let s = Scratch::new("n-slow");
    let bin = s.program("notify-send", "sleep 1");
    let notifier = Notifier::new(None);
    let started = Instant::now();
    notifier.send(note(), &desktop(&bin));
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn a_second_notification_during_the_first_is_dropped_not_queued() {
    let s = Scratch::new("n-busy");
    let out = s.file("count");
    let bin = s.program(
        "notify-send",
        &format!("echo x >> {}; sleep 0.4", out.display()),
    );
    let notifier = Notifier::new(None);
    notifier.send(note(), &desktop(&bin));
    wait_until("the first started", || out.exists());
    notifier.send(note(), &desktop(&bin));
    thread::sleep(Duration::from_millis(700));
    assert_eq!(fs::read_to_string(&out).unwrap(), "x\n");
    // After it finished, the next one is shown.
    notifier.send(note(), &desktop(&bin));
    wait_until("the third ran", || {
        fs::read_to_string(&out).unwrap().len() == 4
    });
}

#[test]
fn what_cannot_be_shown_is_said_once_through_the_stash_without_a_toast() {
    let notifier = Notifier::new(None);
    notifier.send(note(), &vars(&[("PATH", "/usr/bin")]));
    assert_eq!(
        notifier.take_message().as_deref(),
        Some("Desktop notification not shown: no desktop session (DISPLAY is not set)")
    );
    assert_eq!(notifier.take_message(), None, "the message is taken once");
    notifier.send(note(), &vars(&[("PATH", "/usr/bin")]));
    assert_eq!(
        notifier.take_message(),
        None,
        "and said only once per shell"
    );
}

#[test]
fn what_cannot_be_shown_goes_to_the_toast_when_there_is_one() {
    let toast = ToastHandle::with_lifetime(Arc::new(|| {}), Duration::from_secs(60));
    let notifier = Notifier::new(Some(toast.clone()));
    notifier.send(
        note(),
        &vars(&[("PATH", "/usr/bin"), ("SSH_TTY", "/dev/pts/1")]),
    );
    assert_eq!(
        toast.current().as_deref(),
        Some("Desktop notification not shown: this is an ssh session")
    );
    assert_eq!(notifier.take_message(), None);
}

#[test]
fn a_program_that_fails_later_is_said_once_too() {
    let s = Scratch::new("n-fail");
    let bin = s.program("notify-send", "exit 1");
    let notifier = Notifier::new(None);
    notifier.send(note(), &desktop(&bin));
    wait_until("the failure was noted", || {
        notifier.shared.warned.load(Ordering::SeqCst)
    });
    assert_eq!(
        notifier.take_message().as_deref(),
        Some("Desktop notification not shown: notify-send ended with status 1")
    );
}

#[test]
fn a_hung_program_is_killed_when_the_notifier_goes_away() {
    let s = Scratch::new("n-drop");
    let marker = format!("omsh-nt-drop-{}", std::process::id());
    let bin = s.program(
        "notify-send",
        &format!("sh -c 'sleep 30; true' {marker} & wait"),
    );
    let notifier = Notifier::new(None);
    notifier.send(note(), &desktop(&bin));
    wait_until("the program started", || alive(&marker));
    let started = Instant::now();
    drop(notifier);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    let until = Instant::now() + Duration::from_secs(3);
    while alive(&marker) && Instant::now() < until {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(!alive(&marker), "the program outlived the notifier");
}

#[test]
fn a_cancelled_program_is_not_reported_as_a_failure() {
    let s = Scratch::new("n-cancel-quiet");
    let marker = format!("omsh-nt-quiet-{}", std::process::id());
    let bin = s.program(
        "notify-send",
        &format!("sh -c 'sleep 30; true' {marker} & wait"),
    );
    let toast = ToastHandle::with_lifetime(Arc::new(|| {}), Duration::from_secs(60));
    let notifier = Notifier::new(Some(toast.clone()));
    notifier.send(note(), &desktop(&bin));
    wait_until("the program started", || alive(&marker));
    drop(notifier);
    assert_eq!(toast.current(), None);
}
