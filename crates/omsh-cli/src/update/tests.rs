//! Tests of `omsh update` and the daily check with a table of downloads and no network. The
//! tests with a real server and a real terminal are in `omsh-tests/tests/update.rs`.

use std::collections::HashMap;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use flate2::Compression;
use flate2::write::GzEncoder;
use omsh_exec::{ReadError, Terminal};

use super::*;

// -- fakes --------------------------------------------------------------------------------

#[derive(Default)]
struct Screen {
    out: String,
    err: String,
}

impl Terminal for Screen {
    fn input_is_terminal(&self) -> bool {
        true
    }
    fn out(&mut self, text: &str) {
        self.out.push_str(text);
    }
    fn err(&mut self, text: &str) {
        self.err.push_str(text);
    }
    fn read_line(&mut self, _: &str, _: bool, _: &mut Vec<u8>) -> Result<(), ReadError> {
        Err(ReadError::EndOfInput)
    }
    fn interrupted(&self) -> bool {
        false
    }
}

impl Screen {
    fn all(&self) -> String {
        format!("{}{}", self.out, self.err)
    }
}

/// Downloads from a table; counts the asks.
struct Table {
    files: HashMap<String, Vec<u8>>,
    redirects: HashMap<String, String>,
    asked: Mutex<Vec<String>>,
    count: AtomicUsize,
}

impl Table {
    fn new() -> Self {
        Self {
            files: HashMap::new(),
            redirects: HashMap::new(),
            asked: Mutex::new(Vec::new()),
            count: AtomicUsize::new(0),
        }
    }
    fn with(mut self, url: &str, body: &[u8]) -> Self {
        self.files.insert(url.to_owned(), body.to_vec());
        self
    }
    fn with_redirect(mut self, url: &str, target: &str) -> Self {
        self.redirects.insert(url.to_owned(), target.to_owned());
        self
    }
    fn requests(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }
}

impl Fetch for Table {
    fn get(
        &self,
        url: &str,
        _: &str,
        max: usize,
        _: Duration,
        _: &dyn Fn() -> bool,
    ) -> Result<Vec<u8>, ProviderError> {
        self.count.fetch_add(1, Ordering::SeqCst);
        self.asked.lock().unwrap().push(url.to_owned());
        match self.files.get(url) {
            Some(body) if body.len() <= max => Ok(body.clone()),
            Some(_) => Err(ProviderError::new(ErrorKind::BadResponse, "too big")),
            None => Err(ProviderError::new(
                ErrorKind::NotFound,
                "HTTP 404 Not Found",
            )),
        }
    }

    fn location(
        &self,
        url: &str,
        _: Duration,
        _: &dyn Fn() -> bool,
    ) -> Result<String, ProviderError> {
        self.count.fetch_add(1, Ordering::SeqCst);
        self.asked.lock().unwrap().push(url.to_owned());
        self.redirects
            .get(url)
            .cloned()
            .ok_or_else(|| ProviderError::new(ErrorKind::NotFound, "HTTP 404 Not Found"))
    }
}

struct Folder(PathBuf);

impl Folder {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("omsh-update-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

const API: &str = "http://127.0.0.1:1";
const NAME: &str = "oh-my-shell-x86_64-unknown-linux-gnu.tar.gz";

fn origin() -> Origin {
    Origin {
        web: API.to_owned(),
    }
}

fn env(dir: &Folder) -> UpdateEnv {
    UpdateEnv {
        origin: Ok(origin()),
        state_dir: Some(dir.path().join("state")),
        home: Some(dir.path().to_owned()),
        cargo_home: None,
    }
}

fn latest_url() -> String {
    format!("{API}/{REPO}/releases/latest")
}

/// Where `releases/latest` redirects to when `tag` is the newest release.
fn latest_target(tag: &str) -> String {
    format!("{API}/{REPO}/releases/tag/{tag}")
}

/// A tar.gz with the program at the place a release has it.
fn archive_with(program: &[u8]) -> Vec<u8> {
    let name = "oh-my-shell-x86_64-unknown-linux-gnu/oh-my-shell";
    let mut header = [0u8; 512];
    header[..name.len()].copy_from_slice(name.as_bytes());
    header[100..107].copy_from_slice(b"0000755");
    header[124..135].copy_from_slice(format!("{:011o}", program.len()).as_bytes());
    header[156] = b'0';
    header[257..263].copy_from_slice(b"ustar\0");
    let mut tar = header.to_vec();
    tar.extend_from_slice(program);
    tar.resize(tar.len().div_ceil(512) * 512 + 1024, 0);
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(&tar).unwrap();
    encoder.finish().unwrap()
}

/// A program that prints `version` for `--version`.
fn script(version: &str) -> Vec<u8> {
    format!("#!/bin/sh\necho 'oh-my-shell {version}'\n").into_bytes()
}

/// A table with a release `tag` whose program is `program`.
fn release(tag: &str, program: &[u8]) -> Table {
    let archive = archive_with(program);
    let sums = format!("{}  {NAME}\n", archive::sha256_hex(&archive));
    Table::new()
        .with_redirect(&latest_url(), &latest_target(tag))
        .with(
            &format!("{API}/{REPO}/releases/download/{tag}/{NAME}"),
            &archive,
        )
        .with(
            &format!("{API}/{REPO}/releases/download/{tag}/sha256.sum"),
            sums.as_bytes(),
        )
}

fn install_old(dir: &Folder) -> PathBuf {
    let bin = dir.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let path = bin.join("oh-my-shell");
    fs::write(&path, b"#!/bin/sh\necho old\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o750)).unwrap();
    path
}

fn run_update(args: &[&str], env: &UpdateEnv, exe: &Path, table: &Table) -> (u8, Screen) {
    let mut screen = Screen::default();
    let status = run(args, &mut screen, env, Ok(exe.to_owned()), table);
    (status, screen)
}

fn leftovers(path: &Path) -> Vec<String> {
    fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(".oh-my-shell"))
        .collect()
}

// -- omsh update --------------------------------------------------------------------------

#[test]
fn a_newer_release_replaces_the_program_and_keeps_its_mode() {
    let dir = Folder::new("works");
    let exe = install_old(&dir);
    let table = release("v9.9.9", &script("9.9.9"));
    let (status, screen) = run_update(&[], &env(&dir), &exe, &table);
    assert_eq!(status, 0, "{}", screen.all());
    assert_eq!(fs::read(&exe).unwrap(), script("9.9.9"));
    assert_eq!(
        fs::metadata(&exe).unwrap().permissions().mode() & 0o777,
        0o750 | 0o100
    );
    assert!(leftovers(&exe).is_empty(), "{:?}", leftovers(&exe));
    assert!(screen.out.contains("-> 9.9.9"), "{}", screen.all());
    assert!(screen.err.contains("Checksum ok."), "{}", screen.all());
}

#[test]
fn the_stamp_remembers_the_release_after_an_update() {
    let dir = Folder::new("stamp");
    let exe = install_old(&dir);
    let table = release("v9.9.9", &script("9.9.9"));
    let env = env(&dir);
    run_update(&[], &env, &exe, &table);
    let stamp = daily::read(env.state_dir.as_deref().unwrap()).unwrap();
    assert_eq!(stamp.latest, Version::parse("9.9.9"));
}

#[test]
fn a_wrong_checksum_changes_nothing() {
    let dir = Folder::new("badsum");
    let exe = install_old(&dir);
    let archive = archive_with(&script("9.9.9"));
    let table = Table::new()
        .with_redirect(&latest_url(), &latest_target("v9.9.9"))
        .with(
            &format!("{API}/{REPO}/releases/download/v9.9.9/{NAME}"),
            &archive,
        )
        .with(
            &format!("{API}/{REPO}/releases/download/v9.9.9/sha256.sum"),
            format!("{}  {NAME}\n", "0".repeat(64)).as_bytes(),
        );
    let (status, screen) = run_update(&[], &env(&dir), &exe, &table);
    assert_eq!(status, 1);
    assert!(screen.err.contains("checksum mismatch"), "{}", screen.all());
    assert_eq!(fs::read(&exe).unwrap(), b"#!/bin/sh\necho old\n");
    assert!(leftovers(&exe).is_empty());
}

#[test]
fn a_sum_file_that_does_not_list_the_archive_changes_nothing() {
    let dir = Folder::new("nolist");
    let exe = install_old(&dir);
    let archive = archive_with(&script("9.9.9"));
    let table = Table::new()
        .with_redirect(&latest_url(), &latest_target("v9.9.9"))
        .with(
            &format!("{API}/{REPO}/releases/download/v9.9.9/{NAME}"),
            &archive,
        )
        .with(
            &format!("{API}/{REPO}/releases/download/v9.9.9/sha256.sum"),
            b"abc  other.tar.gz\n",
        );
    let (status, screen) = run_update(&[], &env(&dir), &exe, &table);
    assert_eq!(status, 1);
    assert!(screen.err.contains("does not list"), "{}", screen.all());
    assert_eq!(fs::read(&exe).unwrap(), b"#!/bin/sh\necho old\n");
}

#[test]
fn a_program_that_does_not_run_changes_nothing() {
    let dir = Folder::new("broken");
    let exe = install_old(&dir);
    let table = release("v9.9.9", b"#!/bin/sh\nexit 3\n");
    let (status, screen) = run_update(&[], &env(&dir), &exe, &table);
    assert_eq!(status, 1);
    assert!(
        screen.err.contains("nothing was changed"),
        "{}",
        screen.all()
    );
    assert_eq!(fs::read(&exe).unwrap(), b"#!/bin/sh\necho old\n");
    assert!(leftovers(&exe).is_empty());
}

#[test]
fn a_program_that_names_another_version_changes_nothing() {
    let dir = Folder::new("wrongversion");
    let exe = install_old(&dir);
    let table = release("v9.9.9", &script("1.2.3"));
    let (status, screen) = run_update(&[], &env(&dir), &exe, &table);
    assert_eq!(status, 1);
    assert!(
        screen.err.contains("not \"oh-my-shell 9.9.9\""),
        "{}",
        screen.all()
    );
    assert_eq!(fs::read(&exe).unwrap(), b"#!/bin/sh\necho old\n");
}

#[test]
fn an_archive_without_the_program_changes_nothing() {
    let dir = Folder::new("noprogram");
    let exe = install_old(&dir);
    // A valid archive that holds a different file.
    let mut archive = archive_with(&script("9.9.9"));
    archive.truncate(10); // not a gzip any more
    let sums = format!("{}  {NAME}\n", archive::sha256_hex(&archive));
    let table = Table::new()
        .with_redirect(&latest_url(), &latest_target("v9.9.9"))
        .with(
            &format!("{API}/{REPO}/releases/download/v9.9.9/{NAME}"),
            &archive,
        )
        .with(
            &format!("{API}/{REPO}/releases/download/v9.9.9/sha256.sum"),
            sums.as_bytes(),
        );
    let (status, _) = run_update(&[], &env(&dir), &exe, &table);
    assert_eq!(status, 1);
    assert_eq!(fs::read(&exe).unwrap(), b"#!/bin/sh\necho old\n");
}

#[test]
fn the_newest_release_downloads_nothing() {
    let dir = Folder::new("newest");
    let exe = install_old(&dir);
    let current = Version::current().to_string();
    let table = release(&format!("v{current}"), &script(&current));
    let (status, screen) = run_update(&[], &env(&dir), &exe, &table);
    assert_eq!(status, 0);
    assert!(
        screen.out.contains("is the newest release"),
        "{}",
        screen.all()
    );
    assert_eq!(table.requests(), 1, "only the lookup");
    assert_eq!(fs::read(&exe).unwrap(), b"#!/bin/sh\necho old\n");
}

#[test]
fn an_older_release_is_not_installed() {
    let dir = Folder::new("older");
    let exe = install_old(&dir);
    let table = release("v0.0.1", &script("0.0.1"));
    let (status, _) = run_update(&[], &env(&dir), &exe, &table);
    assert_eq!(status, 0);
    assert_eq!(table.requests(), 1);
}

#[test]
fn check_says_what_exists_and_changes_nothing() {
    let dir = Folder::new("check");
    let exe = install_old(&dir);
    let table = release("v9.9.9", &script("9.9.9"));
    let (status, screen) = run_update(&["--check"], &env(&dir), &exe, &table);
    assert_eq!(status, 0);
    assert!(
        screen.out.contains("9.9.9 is available"),
        "{}",
        screen.all()
    );
    assert_eq!(table.requests(), 1);
    assert_eq!(fs::read(&exe).unwrap(), b"#!/bin/sh\necho old\n");
}

#[test]
fn a_refused_place_asks_the_server_nothing() {
    let dir = Folder::new("refused");
    let table = release("v9.9.9", &script("9.9.9"));
    let cases = [
        ("/usr/bin/oh-my-shell", "package"),
        ("/home/u/.cargo/bin/oh-my-shell", "cargo"),
        (
            "/home/u/code/x/target/debug/oh-my-shell",
            "build from source",
        ),
    ];
    for (path, word) in cases {
        let mut env = env(&dir);
        env.home = Some(PathBuf::from("/home/u"));
        let mut screen = Screen::default();
        let status = run(&[], &mut screen, &env, Ok(PathBuf::from(path)), &table);
        assert_eq!(status, 1, "{path}");
        assert!(screen.err.contains(word), "{path}: {}", screen.all());
    }
    assert_eq!(table.requests(), 0, "no request for a refused place");
}

#[test]
fn a_program_removed_while_running_is_reported() {
    let dir = Folder::new("deleted");
    let table = release("v9.9.9", &script("9.9.9"));
    let mut screen = Screen::default();
    let status = run(
        &[],
        &mut screen,
        &env(&dir),
        Err("this program was replaced or removed while it was running".to_owned()),
        &table,
    );
    assert_eq!(status, 1);
    assert_eq!(table.requests(), 0);
}

#[test]
fn a_refused_server_setting_asks_nothing() {
    let dir = Folder::new("hook");
    let exe = install_old(&dir);
    let mut env = env(&dir);
    env.origin = download::release_origin(Some("http://example.com"));
    let table = release("v9.9.9", &script("9.9.9"));
    let (status, screen) = run_update(&[], &env, &exe, &table);
    assert_eq!(status, 1);
    assert!(screen.err.contains("not replaced"), "{}", screen.all());
    assert_eq!(table.requests(), 0);
}

#[test]
fn the_latest_release_is_the_redirect_of_releases_latest_and_not_the_api() {
    let dir = Folder::new("redirect");
    let exe = install_old(&dir);
    let table = table_with_latest("v9.9.9");
    let (status, screen) = run_update(&["--check"], &env(&dir), &exe, &table);
    assert_eq!(status, 0, "{}", screen.all());
    assert!(screen.all().contains("9.9.9"), "{}", screen.all());
    assert_eq!(
        *table.asked.lock().unwrap(),
        vec![format!("{API}/{REPO}/releases/latest")],
        "one lookup, at the web address (the API allows 60 requests an hour)"
    );
}

#[test]
fn a_repository_with_no_release_is_said_in_words() {
    // github.com redirects `releases/latest` to `.../releases` when there is no release.
    let dir = Folder::new("norelease");
    let exe = install_old(&dir);
    let table = Table::new().with_redirect(&latest_url(), &format!("{API}/{REPO}/releases"));
    let (status, screen) = run_update(&["--check"], &env(&dir), &exe, &table);
    assert_eq!(status, 1);
    assert!(
        screen.err.contains("could not be read (is there one yet?)"),
        "{}",
        screen.all()
    );
}

#[test]
fn a_lookup_that_fails_is_said_in_words() {
    let dir = Folder::new("lookup");
    let exe = install_old(&dir);
    let (status, screen) = run_update(&[], &env(&dir), &exe, &Table::new());
    assert_eq!(status, 1);
    assert!(
        screen.err.contains("release was not found"),
        "{}",
        screen.all()
    );
    assert!(
        !screen.err.contains("127.0.0.1"),
        "no address: {}",
        screen.all()
    );
}

#[test]
fn a_tag_that_is_not_a_version_is_refused() {
    let dir = Folder::new("badtag");
    let exe = install_old(&dir);
    let table = Table::new().with_redirect(&latest_url(), &latest_target("nightly"));
    let (status, screen) = run_update(&[], &env(&dir), &exe, &table);
    assert_eq!(status, 1);
    assert!(
        screen.err.contains("not a version number"),
        "{}",
        screen.all()
    );
}

#[test]
fn bad_arguments_are_a_usage_error() {
    let dir = Folder::new("usage");
    let exe = install_old(&dir);
    let table = Table::new();
    let (status, screen) = run_update(&["--force"], &env(&dir), &exe, &table);
    assert_eq!(status, 2);
    assert!(screen.err.contains("usage: omsh update"));
    assert_eq!(table.requests(), 0);
}

#[test]
fn a_folder_that_cannot_be_written_says_what_to_do() {
    if nix::unistd::geteuid().is_root() {
        return; // root can write anywhere
    }
    let dir = Folder::new("readonly");
    let exe = install_old(&dir);
    let bin = exe.parent().unwrap();
    fs::set_permissions(bin, fs::Permissions::from_mode(0o555)).unwrap();
    let table = release("v9.9.9", &script("9.9.9"));
    let (status, screen) = run_update(&[], &env(&dir), &exe, &table);
    fs::set_permissions(bin, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(status, 1);
    assert!(screen.err.contains("cannot write to"), "{}", screen.all());
    assert!(screen.err.contains("sudo"), "{}", screen.all());
}

#[test]
fn the_archive_name_follows_the_cpu() {
    assert_eq!(
        archive_name("x86_64").as_deref(),
        Some("oh-my-shell-x86_64-unknown-linux-gnu.tar.gz")
    );
    assert_eq!(
        archive_name("aarch64").as_deref(),
        Some("oh-my-shell-aarch64-unknown-linux-gnu.tar.gz")
    );
    assert_eq!(archive_name("riscv64"), None);
}

// -- the daily check ----------------------------------------------------------------------

fn table_with_latest(tag: &str) -> Table {
    Table::new().with_redirect(&latest_url(), &latest_target(tag))
}

#[test]
fn the_stamp_round_trips_and_is_private() {
    let dir = Folder::new("stamprt");
    let state = dir.path().join("state");
    let stamp = daily::Stamp {
        checked: 1_700_000_000,
        latest: Version::parse("1.2.3"),
    };
    daily::write(&state, &stamp).unwrap();
    assert_eq!(daily::read(&state), Some(stamp));
    assert_eq!(
        fs::metadata(&state).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let file = state.join("update-check");
    assert_eq!(
        fs::metadata(file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let names: Vec<_> = fs::read_dir(&state).unwrap().collect();
    assert_eq!(names.len(), 1, "no temporary file is left");
}

#[test]
fn a_broken_stamp_counts_as_none() {
    let dir = Folder::new("stampbad");
    let state = dir.path().join("state");
    fs::create_dir_all(&state).unwrap();
    for text in [
        "",
        "garbage",
        "checked=abc\n",
        "latest=1.0.0\n",
        "checked=-5\n",
    ] {
        fs::write(state.join("update-check"), text).unwrap();
        assert_eq!(daily::read(&state), None, "{text:?}");
    }
    fs::write(state.join("update-check"), "checked=5\nlatest=nonsense\n").unwrap();
    assert_eq!(daily::read(&state).unwrap().latest, None);
}

#[test]
fn a_lookup_is_due_once_a_day() {
    let at = |checked| daily::Stamp {
        checked,
        latest: None,
    };
    let now = 1_000_000;
    assert!(daily::is_due(None, now));
    assert!(!daily::is_due(Some(&at(now)), now));
    assert!(!daily::is_due(Some(&at(now - daily::DAY + 1)), now));
    assert!(daily::is_due(Some(&at(now - daily::DAY)), now));
    assert!(daily::is_due(Some(&at(0)), now));
    // The clock was set back: a stamp from the far future is not trusted.
    assert!(!daily::is_due(Some(&at(now + daily::DAY)), now));
    assert!(daily::is_due(Some(&at(now + daily::DAY + 1)), now));
}

#[test]
fn a_newer_release_is_reported_once_and_remembered() {
    let dir = Folder::new("dailynew");
    let env = env(&dir);
    let table = table_with_latest("v9.9.9");
    let current = Version::current();
    let now = 5_000_000;
    let found = daily::check_once(&env, now, &table, &current);
    assert_eq!(found, Version::parse("9.9.9"));
    assert_eq!(table.requests(), 1);
    // The same day: no lookup, nothing to say.
    let again = daily::check_once(&env, now + 60, &table, &current);
    assert_eq!(again, None);
    assert_eq!(table.requests(), 1);
    // A day later it asks again.
    let later = daily::check_once(&env, now + daily::DAY, &table, &current);
    assert_eq!(later, Version::parse("9.9.9"));
    assert_eq!(table.requests(), 2);
}

#[test]
fn the_same_or_an_older_release_says_nothing() {
    let dir = Folder::new("dailysame");
    let env = env(&dir);
    let current = Version::current();
    let table = table_with_latest(&format!("v{current}"));
    assert_eq!(daily::check_once(&env, 1_000_000, &table, &current), None);
    let stamp = daily::read(env.state_dir.as_deref().unwrap()).unwrap();
    assert_eq!(stamp.checked, 1_000_000);
    assert_eq!(stamp.latest, Some(current));
}

#[test]
fn a_failed_lookup_is_not_repeated_at_every_start() {
    let dir = Folder::new("dailyfail");
    let env = env(&dir);
    let table = Table::new(); // answers 404
    let current = Version::current();
    assert_eq!(daily::check_once(&env, 7_000_000, &table, &current), None);
    assert_eq!(table.requests(), 1);
    assert_eq!(daily::check_once(&env, 7_000_100, &table, &current), None);
    assert_eq!(
        table.requests(),
        1,
        "the stamp was written before the lookup"
    );
}

#[test]
fn without_a_state_folder_or_with_a_refused_server_nothing_is_asked() {
    let dir = Folder::new("dailynone");
    let table = table_with_latest("v9.9.9");
    let current = Version::current();
    let mut env = env(&dir);
    env.state_dir = None;
    assert_eq!(daily::check_once(&env, 1, &table, &current), None);
    let mut env = self::env(&dir);
    env.origin = Err("refused".to_owned());
    assert_eq!(daily::check_once(&env, 1, &table, &current), None);
    assert_eq!(table.requests(), 0);
}

#[test]
fn the_toast_names_both_versions_and_the_command() {
    let text = daily::message(
        &Version::parse("0.2.0").unwrap(),
        &Version::parse("0.1.0").unwrap(),
    );
    assert_eq!(
        text,
        "Oh My Shell 0.2.0 is out (you have 0.1.0): run omsh update"
    );
    assert!(text.chars().count() <= 100);
}

#[test]
fn the_helper_thread_leaves_the_news_for_a_shell_without_a_toast() {
    let dir = Folder::new("dailythread");
    let env = env(&dir);
    let table = std::sync::Arc::new(table_with_latest("v9.9.9"));
    let daily = daily::start_with(
        &env,
        None,
        Duration::from_millis(1),
        table.clone() as std::sync::Arc<dyn Fetch>,
    )
    .expect("a check is due");
    let started = Instant::now();
    let mut message = None;
    while started.elapsed() < Duration::from_secs(5) {
        message = daily.take_message();
        if message.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(message.unwrap_or_default().contains("9.9.9"));
    assert_eq!(daily.take_message(), None, "said once");
    // A second shell the same day does not even start a thread.
    assert!(
        daily::start_with(
            &env,
            None,
            Duration::ZERO,
            table as std::sync::Arc<dyn Fetch>
        )
        .is_none()
    );
}
