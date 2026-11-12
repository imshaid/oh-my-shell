//! Tests of the completer: what each source offers, how the answers are written into the line,
//! and the way the slow work runs on a helper thread.

use std::fs::{self, File};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime};

use super::*;

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("omsh-complete-{}-{n}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.0.join(relative)
    }

    fn write(&self, relative: &str, text: &str) {
        let path = self.path(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn program(&self, relative: &str) {
        self.write(relative, "");
        fs::set_permissions(self.path(relative), fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn dir(&self, relative: &str) {
        fs::create_dir_all(self.path(relative)).unwrap();
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn scene_of(facts: Facts, cwd: &Path, home: Option<&Path>) -> Arc<Mutex<Scene>> {
    let scene = Arc::new(Mutex::new(Scene::default()));
    lock(&scene).reset(facts, cwd.to_path_buf(), home.map(Path::to_path_buf));
    scene
}

fn plain(cwd: &Path) -> (ShellCompleter, Arc<Mutex<Scene>>) {
    let scene = scene_of(Facts::default(), cwd, None);
    (ShellCompleter::new(Arc::clone(&scene)), scene)
}

fn with_facts(facts: Facts, cwd: &Path) -> (ShellCompleter, Arc<Mutex<Scene>>) {
    let scene = scene_of(facts, cwd, None);
    (ShellCompleter::new(Arc::clone(&scene)), scene)
}

/// Asks at the end of `line`; if the answer is not there yet, waits for the helper.
fn settle(completer: &mut ShellCompleter, line: &str) -> Vec<Suggestion> {
    let mut result = completer.complete(line, line.len());
    let deadline = Instant::now() + Duration::from_secs(10);
    while result.is_pending() {
        assert!(Instant::now() < deadline, "the helper never finished");
        if completer.poll_completion() == CompletionStatus::Ready {
            result = completer.complete(line, line.len());
        } else {
            thread::sleep(Duration::from_millis(1));
        }
    }
    result.suggestions().to_vec()
}

fn values(completer: &mut ShellCompleter, line: &str) -> Vec<String> {
    settle(completer, line)
        .into_iter()
        .map(|suggestion| suggestion.value)
        .collect()
}

fn shown(list: &[Suggestion]) -> Vec<&str> {
    list.iter().map(|s| s.display_value()).collect()
}

fn words(list: &[&str]) -> Vec<String> {
    list.iter().map(|word| (*word).to_owned()).collect()
}

// ---- command names ----

fn path_facts(dirs: &[&Path]) -> Facts {
    let path = dirs
        .iter()
        .map(|dir| dir.to_str().unwrap())
        .collect::<Vec<_>>()
        .join(":");
    Facts {
        path: path.into_bytes(),
        ..Facts::default()
    }
}

#[test]
fn a_command_name_comes_from_builtins_aliases_functions_and_path() {
    let bin = Scratch::new();
    for name in ["gitk", "grep", "gist", "other"] {
        bin.program(name);
    }
    let facts = Facts {
        functions: words(&["gfn"]),
        aliases: words(&["gal"]),
        builtins: words(&["getopts", "cd"]),
        ..path_facts(&[&bin.0])
    };
    let (mut c, _) = with_facts(facts, &bin.0);
    let found = settle(&mut c, "g");
    assert_eq!(
        shown(&found),
        ["gal", "getopts", "gfn", "gist", "gitk", "grep"]
    );
    let label = |name: &str| {
        found
            .iter()
            .find(|s| s.display_value() == name)
            .unwrap()
            .description
            .clone()
    };
    assert_eq!(label("gal").as_deref(), Some("alias"));
    assert_eq!(label("getopts").as_deref(), Some("builtin"));
    assert_eq!(label("gfn").as_deref(), Some("function"));
    assert_eq!(label("gitk"), None, "a program has no label");
    // The word being replaced is the one typed.
    assert!(found.iter().all(|s| s.span == Span::new(0, 1)));
    assert!(found.iter().all(|s| s.append_whitespace));
}

#[test]
fn a_function_wins_over_an_alias_a_builtin_and_a_program_of_the_same_name() {
    let bin = Scratch::new();
    bin.program("same");
    let facts = Facts {
        functions: words(&["same"]),
        aliases: words(&["same"]),
        builtins: words(&["same"]),
        ..path_facts(&[&bin.0])
    };
    let (mut c, _) = with_facts(facts, &bin.0);
    let found = settle(&mut c, "sa");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].description.as_deref(), Some("function"));
    let facts = Facts {
        aliases: words(&["same"]),
        builtins: words(&["same"]),
        ..path_facts(&[&bin.0])
    };
    let (mut c, _) = with_facts(facts, &bin.0);
    assert_eq!(
        settle(&mut c, "sa")[0].description.as_deref(),
        Some("alias")
    );
}

#[test]
fn a_name_in_the_middle_of_the_line_is_completed_after_a_separator() {
    let bin = Scratch::new();
    bin.program("grep");
    let (mut c, _) = with_facts(path_facts(&[&bin.0]), &bin.0);
    assert_eq!(values(&mut c, "ls | gr"), ["grep"]);
    assert_eq!(values(&mut c, "true && gr"), ["grep"]);
    assert_eq!(values(&mut c, "echo $(gr"), ["grep"]);
    let found = settle(&mut c, "ls | gr");
    assert_eq!(found[0].span, Span::new(5, 7));
}

#[test]
fn an_empty_command_word_offers_nothing_and_starts_nothing() {
    let bin = Scratch::new();
    bin.program("tool");
    let (mut c, _) = with_facts(path_facts(&[&bin.0]), &bin.0);
    assert!(values(&mut c, "").is_empty());
    assert!(values(&mut c, "ls | ").is_empty());
    assert_eq!(c.helpers_started(), 0);
}

#[test]
fn a_command_name_with_a_slash_or_a_tilde_is_a_file_name_of_a_program() {
    let s = Scratch::new();
    s.program("run.sh");
    s.write("data.txt", "");
    s.dir("sub");
    let (mut c, _) = plain(&s.0);
    assert_eq!(values(&mut c, "./"), ["./run.sh", "./sub/"]);
    assert_eq!(values(&mut c, "./r"), ["./run.sh"]);
    assert_eq!(c.helpers_started(), 0);
}

#[test]
fn a_command_name_is_written_in_the_open_quote() {
    let bin = Scratch::new();
    bin.program("tool");
    let (mut c, _) = with_facts(path_facts(&[&bin.0]), &bin.0);
    assert_eq!(values(&mut c, "'too"), ["'tool'"]);
    assert_eq!(values(&mut c, "\"too"), ["\"tool\""]);
}

// ---- the helper thread ----

/// A gate the loader waits at until a test opens it.
#[derive(Default)]
struct Gate {
    open: Mutex<bool>,
    changed: Condvar,
}

impl Gate {
    fn wait(&self) {
        let mut open = lock(&self.open);
        let deadline = Instant::now() + Duration::from_secs(10);
        while !*open {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return;
            }
            open = self
                .changed
                .wait_timeout(open, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    fn open(&self) {
        *lock(&self.open) = true;
        self.changed.notify_all();
    }
}

/// The real loader, held back at a gate.
struct Slow {
    gate: Arc<Gate>,
    finished: Arc<AtomicUsize>,
}

impl Loader for Slow {
    fn path_index(&self, path: &[u8], cancelled: &dyn Fn() -> bool) -> PathIndex {
        self.gate.wait();
        let index = DiskLoader.path_index(path, cancelled);
        self.finished.fetch_add(1, Ordering::SeqCst);
        index
    }

    fn flags(&self, command: &str, dirs: &[PathBuf], cancelled: &dyn Fn() -> bool) -> Vec<Flag> {
        self.gate.wait();
        let flags = DiskLoader.flags(command, dirs, cancelled);
        self.finished.fetch_add(1, Ordering::SeqCst);
        flags
    }
}

fn slow(scene: Arc<Mutex<Scene>>) -> (ShellCompleter, Arc<Gate>, Arc<AtomicUsize>) {
    let gate = Arc::new(Gate::default());
    let finished = Arc::new(AtomicUsize::new(0));
    let loader = Slow {
        gate: Arc::clone(&gate),
        finished: Arc::clone(&finished),
    };
    (
        ShellCompleter::with_loader(scene, Arc::new(loader)),
        gate,
        finished,
    )
}

fn wait_for(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn no_helper_runs_until_a_tab_needs_one() {
    let bin = Scratch::new();
    bin.program("tool");
    let (c, _) = with_facts(path_facts(&[&bin.0]), &bin.0);
    assert_eq!(c.helpers_started(), 0);
}

#[test]
fn the_first_command_name_is_pending_and_the_answer_comes_when_the_helper_is_done() {
    let bin = Scratch::new();
    bin.program("tool");
    let scene = scene_of(path_facts(&[&bin.0]), &bin.0, None);
    let (mut c, gate, _) = slow(scene);
    let first = c.complete("to", 2);
    assert!(first.is_pending());
    assert_eq!(c.poll_completion(), CompletionStatus::Pending);
    // Asking again while it works does not start a second helper.
    assert!(c.complete("to", 2).is_pending());
    assert!(c.complete("too", 3).is_pending());
    assert_eq!(c.helpers_started(), 1);

    gate.open();
    wait_for("the helper", || {
        c.poll_completion() == CompletionStatus::Ready
    });
    // Ready is told once.
    assert_eq!(c.poll_completion(), CompletionStatus::Idle);
    let second = c.complete("to", 2);
    assert!(!second.is_pending());
    assert_eq!(shown(second.suggestions()), ["tool"]);
    assert_eq!(c.helpers_started(), 1, "the index is kept");
    // Every later Tab is answered at once.
    assert!(!c.complete("tool", 4).is_pending());
}

#[test]
fn asking_for_something_else_cancels_the_old_work_and_its_answer_is_dropped() {
    let (one, two) = (Scratch::new(), Scratch::new());
    one.program("prog-one");
    two.program("prog-two");
    let scene = scene_of(path_facts(&[&one.0]), &one.0, None);
    let (mut c, gate, finished) = slow(Arc::clone(&scene));
    assert!(c.complete("prog", 4).is_pending());
    // The next prompt has another PATH: the first request is out of date.
    lock(&scene).reset(path_facts(&[&two.0]), two.0.clone(), None);
    assert!(c.complete("prog", 4).is_pending());
    assert_eq!(c.helpers_started(), 2);

    gate.open();
    wait_for("both helpers", || finished.load(Ordering::SeqCst) == 2);
    wait_for("the answer", || {
        c.poll_completion() == CompletionStatus::Ready
    });
    assert_eq!(values(&mut c, "prog"), ["prog-two"]);
    // The late answer of the first helper was not stored over the second.
    assert!(
        lock(&c.cache)
            .index
            .as_ref()
            .unwrap()
            .is_fresh(&path_facts(&[&two.0]).path)
    );
}

#[test]
fn a_cancelled_helper_stops_listing() {
    let bin = Scratch::new();
    bin.program("tool");
    let scene = scene_of(path_facts(&[&bin.0]), &bin.0, None);
    let (mut c, gate, finished) = slow(Arc::clone(&scene));
    assert!(c.complete("to", 2).is_pending());
    // A request for flags replaces the request for the index.
    assert!(c.complete("ls --a", 6).is_pending());
    gate.open();
    wait_for("both helpers", || finished.load(Ordering::SeqCst) == 2);
    wait_for("ready", || c.poll_completion() == CompletionStatus::Ready);
    assert!(
        lock(&c.cache).index.is_none(),
        "the replaced request left no index"
    );
    assert!(lock(&c.cache).flags.contains_key("ls"));
}

#[test]
fn a_program_installed_later_is_found_when_the_directory_changes() {
    let bin = Scratch::new();
    bin.program("alpha");
    let (mut c, scene) = with_facts(path_facts(&[&bin.0]), &bin.0);
    assert_eq!(values(&mut c, "a"), ["alpha"]);
    assert_eq!(c.helpers_started(), 1);

    bin.program("also");
    // Some file systems keep a coarse clock; set the directory's time by hand.
    File::open(&bin.0)
        .unwrap()
        .set_modified(SystemTime::now() + Duration::from_secs(5))
        .unwrap();
    // The next prompt: a new epoch, so no remembered answer is used.
    let facts = lock(&scene).facts.clone();
    lock(&scene).reset(facts, bin.0.clone(), None);
    assert!(c.complete("a", 1).is_pending(), "stale: listed again");
    assert_eq!(values(&mut c, "a"), ["alpha", "also"]);
    assert_eq!(c.helpers_started(), 2);
}

#[test]
fn a_changed_path_is_listed_again() {
    let (one, two) = (Scratch::new(), Scratch::new());
    one.program("tool-one");
    two.program("tool-two");
    let (mut c, scene) = with_facts(path_facts(&[&one.0]), &one.0);
    assert_eq!(values(&mut c, "tool"), ["tool-one"]);
    lock(&scene).reset(path_facts(&[&two.0]), two.0.clone(), None);
    assert_eq!(values(&mut c, "tool"), ["tool-two"]);
}

#[test]
fn files_variables_and_git_start_no_helper() {
    let s = Scratch::new();
    s.write("r/.git/refs/heads/main", "");
    s.write("file", "");
    let facts = Facts {
        variables: words(&["HOME"]),
        ..Facts::default()
    };
    let (mut c, scene) = with_facts(facts, &s.0);
    lock(&scene).cwd = s.path("r");
    for line in [
        "ls fi",
        "echo $HO",
        "git checkout ma",
        "git ",
        "cat > fi",
        "cd ",
    ] {
        let _ = values(&mut c, line);
    }
    assert_eq!(c.helpers_started(), 0);
}

// ---- flags from fish files ----

fn fish_dir(files: &[(&str, &str)]) -> Scratch {
    let s = Scratch::new();
    for (name, text) in files {
        s.write(name, text);
    }
    s
}

#[test]
fn flags_come_from_the_fish_file_with_their_descriptions() {
    let dir = fish_dir(&[(
        "ls.fish",
        "complete -c ls -s a -l all -d 'Show hidden'\n\
         complete -c ls -s A -l almost-all -d 'Almost all'\n\
         complete -c ls -s l -d 'Long listing'\n\
         complete -c ls -n '__fish_x' -l conditional\n",
    )]);
    let facts = Facts {
        fish_dirs: vec![dir.0.clone()],
        ..Facts::default()
    };
    let (mut c, _) = with_facts(facts, &dir.0);
    let found = settle(&mut c, "ls --al");
    assert_eq!(shown(&found), ["--all", "--almost-all"]);
    assert_eq!(found[0].description.as_deref(), Some("Show hidden"));
    assert_eq!(found[0].span, Span::new(3, 7));
    assert_eq!(
        values(&mut c, "ls -"),
        ["-A", "-a", "--all", "--almost-all", "-l"]
    );
    assert!(
        values(&mut c, "ls --cond").is_empty(),
        "conditions are not read"
    );
    assert_eq!(c.helpers_started(), 1, "the file is read once");
}

#[test]
fn a_command_without_a_file_has_no_flags_and_is_not_looked_for_again() {
    let dir = fish_dir(&[("other.fish", "complete -c other -s x\n")]);
    let facts = Facts {
        fish_dirs: vec![dir.0.clone()],
        ..Facts::default()
    };
    let (mut c, _) = with_facts(facts, &dir.0);
    assert!(values(&mut c, "nosuch -").is_empty());
    assert!(values(&mut c, "nosuch --x").is_empty());
    assert_eq!(c.helpers_started(), 1);
    assert_eq!(values(&mut c, "other -"), ["-x"]);
    assert_eq!(c.helpers_started(), 2);
}

#[test]
fn flags_are_looked_up_by_the_name_of_the_program_not_its_folder() {
    let dir = fish_dir(&[("ls.fish", "complete -c ls -s a\n")]);
    let facts = Facts {
        fish_dirs: vec![dir.0.clone()],
        ..Facts::default()
    };
    let (mut c, _) = with_facts(facts, &dir.0);
    assert_eq!(values(&mut c, "/bin/ls -"), ["-a"]);
}

#[test]
fn a_dash_in_the_first_word_or_after_a_redirection_is_not_a_flag() {
    let dir = fish_dir(&[("ls.fish", "complete -c ls -s a\n")]);
    let facts = Facts {
        fish_dirs: vec![dir.0.clone()],
        ..Facts::default()
    };
    let (mut c, _) = with_facts(facts, &dir.0);
    // The first word is a command name, whatever it starts with.
    assert!(values(&mut c, "-").is_empty());
    assert!(values(&mut c, "ls > -").is_empty());
}

// ---- file names ----

#[test]
fn a_name_with_spaces_is_written_with_backslashes() {
    let s = Scratch::new();
    s.write("my file.txt", "");
    s.dir("my dir");
    s.write("other", "");
    let (mut c, _) = plain(&s.0);
    let found = settle(&mut c, "cat my");
    assert_eq!(
        found.iter().map(|f| f.value.as_str()).collect::<Vec<_>>(),
        ["my\\ dir/", "my\\ file.txt"]
    );
    assert_eq!(shown(&found), ["my dir/", "my file.txt"]);
    assert!(found.iter().all(|f| f.span == Span::new(4, 6)));
    assert!(!found[0].append_whitespace, "a folder: go on typing");
    assert!(found[1].append_whitespace, "a file: the word is done");
}

#[test]
fn a_name_is_written_inside_the_quote_that_is_open() {
    let s = Scratch::new();
    s.write("my file.txt", "");
    s.dir("my dir");
    let (mut c, _) = plain(&s.0);
    assert_eq!(values(&mut c, "cat 'my"), ["'my dir/", "'my file.txt'"]);
    assert_eq!(values(&mut c, "cat \"my"), ["\"my dir/", "\"my file.txt\""]);
    // A closed quote in the middle: the word is rewritten whole, without the quotes.
    s.write("my dir/inner.txt", "");
    assert_eq!(values(&mut c, "cat \"my dir\"/in"), ["my\\ dir/inner.txt"]);
}

#[test]
fn what_is_written_reads_back_as_the_same_file_name() {
    let s = Scratch::new();
    let names = [
        "it's",
        "say \"hi\"",
        "$HOME",
        "a*b",
        "semi;colon",
        "-dash",
        "naïve",
        "বাংলা",
        "a&b",
        "a|b",
        "a<b>c",
        "hash#tag",
        "~tilde",
        "paren(s)",
        "back\\slash",
    ];
    for name in names {
        s.write(&format!("d/{name}"), "");
    }
    // A name with a control character cannot be written into a line; it is left out.
    s.write("d/tab\there", "");
    let (mut c, _) = plain(&s.0);
    for quote in ["", "'", "\""] {
        let found = settle(&mut c, &format!("cat {quote}d/"));
        assert_eq!(found.len(), names.len(), "{quote:?}");
        for suggestion in &found {
            let read = crate::highlight::literal_value(&suggestion.value).unwrap();
            assert!(
                names.iter().any(|name| read == format!("d/{name}")),
                "{quote:?}: {:?} reads back as {read:?}",
                suggestion.value
            );
        }
    }
}

#[test]
fn a_name_with_a_line_feed_is_not_offered() {
    let s = Scratch::new();
    s.write("bad\nname", "");
    s.write("good", "");
    let (mut c, _) = plain(&s.0);
    assert_eq!(values(&mut c, "cat "), ["good"]);
}

#[test]
fn the_tilde_stays_and_the_home_directory_is_looked_in() {
    let home = Scratch::new();
    home.dir("projects");
    home.write("notes file", "");
    let scene = scene_of(Facts::default(), Path::new("/"), Some(&home.0));
    let mut c = ShellCompleter::new(scene);
    assert_eq!(values(&mut c, "ls ~/"), ["~/notes\\ file", "~/projects/"]);
    assert_eq!(values(&mut c, "ls ~/pro"), ["~/projects/"]);
    assert_eq!(values(&mut c, "ls ~"), ["~/"]);
    assert!(values(&mut c, "ls ~nobody/x").is_empty());
}

#[test]
fn only_folders_follow_cd_and_pushd() {
    let s = Scratch::new();
    s.write("file", "");
    s.dir("folder");
    let (mut c, _) = plain(&s.0);
    assert_eq!(values(&mut c, "cd "), ["folder/"]);
    assert_eq!(values(&mut c, "pushd "), ["folder/"]);
    assert_eq!(values(&mut c, "ls "), ["file", "folder/"]);
    assert_eq!(values(&mut c, "cd -- f"), ["folder/"]);
}

#[test]
fn the_target_of_a_redirection_is_a_file_name() {
    let s = Scratch::new();
    s.write("out.txt", "");
    let (mut c, _) = plain(&s.0);
    assert_eq!(values(&mut c, "ls > ou"), ["out.txt"]);
    assert_eq!(values(&mut c, "ls 2>ou"), ["out.txt"]);
    assert_eq!(values(&mut c, "cat < ou"), ["out.txt"]);
}

#[test]
fn a_huge_folder_gives_at_most_the_limit() {
    let s = Scratch::new();
    for n in 0..(MAX_SUGGESTIONS + 100) {
        File::create(s.path(&format!("f{n:05}"))).unwrap();
    }
    let (mut c, _) = plain(&s.0);
    assert_eq!(settle(&mut c, "ls f").len(), MAX_SUGGESTIONS);
}

#[test]
fn an_answer_is_kept_for_the_same_line_until_the_next_prompt() {
    let s = Scratch::new();
    s.write("a1", "");
    let (mut c, scene) = plain(&s.0);
    assert_eq!(values(&mut c, "ls a"), ["a1"]);
    s.write("a2", "");
    assert_eq!(values(&mut c, "ls a"), ["a1"], "same prompt, same line");
    assert_eq!(
        values(&mut c, "ls a2"),
        ["a2"],
        "another line is asked again"
    );
    let facts = lock(&scene).facts.clone();
    lock(&scene).reset(facts, s.0.clone(), None);
    assert_eq!(values(&mut c, "ls a"), ["a1", "a2"], "a new prompt forgets");
}

// ---- variables ----

#[test]
fn a_variable_name_is_completed_after_a_dollar() {
    let facts = Facts {
        variables: words(&["HOME", "HOSTNAME", "PATH", "bad-name", "1x", "hostile"]),
        ..Facts::default()
    };
    let (mut c, _) = with_facts(facts, Path::new("/"));
    let found = settle(&mut c, "echo $HO");
    assert_eq!(shown(&found), ["HOME", "HOSTNAME"]);
    assert!(
        found.iter().all(|f| f.span == Span::new(6, 8)),
        "after the dollar"
    );
    assert!(found.iter().all(|f| !f.append_whitespace));
    assert_eq!(values(&mut c, "echo ${HO"), ["HOME}", "HOSTNAME}"]);
    assert_eq!(values(&mut c, "echo \"$P"), ["PATH"]);
    assert_eq!(values(&mut c, "echo x$P"), ["PATH"]);
    assert_eq!(
        values(&mut c, "echo $"),
        ["HOME", "HOSTNAME", "PATH", "hostile"]
    );
    assert!(
        values(&mut c, "echo '$HO").is_empty(),
        "single quotes: no expansion"
    );
    // An escaped dollar is not a variable (the word is then looked for as a file name).
    assert!(values(&mut c, "echo \\$HO").is_empty());
}

#[test]
fn a_variable_in_the_command_position_is_completed_too() {
    let facts = Facts {
        variables: words(&["EDITOR"]),
        ..Facts::default()
    };
    let (mut c, _) = with_facts(facts, Path::new("/"));
    assert_eq!(values(&mut c, "$ED"), ["EDITOR"]);
    assert_eq!(c.helpers_started(), 0);
}

// ---- git ----

fn repo() -> Scratch {
    let s = Scratch::new();
    s.write("r/.git/HEAD", "ref: refs/heads/main\n");
    s.write("r/.git/refs/heads/main", "");
    s.write("r/.git/refs/heads/feature/login", "");
    s.write("r/.git/refs/remotes/origin/main", "");
    s.write("r/.git/refs/tags/v1", "");
    s.write("r/tracked.txt", "");
    s
}

fn in_repo(s: &Scratch) -> ShellCompleter {
    plain(&s.path("r")).0
}

#[test]
fn git_arguments_are_branches_remote_branches_and_tags() {
    let s = repo();
    let mut c = in_repo(&s);
    let found = settle(&mut c, "git checkout ");
    assert_eq!(
        shown(&found),
        ["feature/login", "main", "origin/main", "v1"]
    );
    let label = |name: &str| {
        found
            .iter()
            .find(|f| f.display_value() == name)
            .unwrap()
            .description
            .clone()
            .unwrap()
    };
    assert_eq!(label("main"), "branch");
    assert_eq!(label("origin/main"), "remote branch");
    assert_eq!(label("v1"), "tag");
    assert_eq!(values(&mut c, "git checkout ma"), ["main"]);
    assert_eq!(values(&mut c, "git switch fea"), ["feature/login"]);
    assert_eq!(values(&mut c, "git -C . checkout or"), ["origin/main"]);
    assert_eq!(values(&mut c, "git merge feature/"), ["feature/login"]);
    assert_eq!(c.helpers_started(), 0, "no process and no helper");
}

#[test]
fn the_first_argument_of_push_and_pull_is_a_remote() {
    let s = repo();
    let mut c = in_repo(&s);
    let found = settle(&mut c, "git push ");
    assert_eq!(shown(&found), ["origin"]);
    assert_eq!(found[0].description.as_deref(), Some("remote"));
    assert_eq!(values(&mut c, "git pull o"), ["origin"]);
    assert_eq!(values(&mut c, "git push origin ma"), ["main"]);
    assert_eq!(values(&mut c, "git push -u origin fea"), ["feature/login"]);
}

#[test]
fn the_git_command_itself_is_completed() {
    let s = repo();
    let mut c = in_repo(&s);
    assert_eq!(values(&mut c, "git ch"), ["checkout", "cherry-pick"]);
    assert_eq!(values(&mut c, "git -C . che"), ["checkout", "cherry-pick"]);
    assert!(values(&mut c, "git zzz").is_empty());
    let all = values(&mut c, "git ");
    assert!(all.contains(&"status".to_owned()) && all.contains(&"commit".to_owned()));
}

#[test]
fn other_git_commands_complete_file_names() {
    let s = repo();
    let mut c = in_repo(&s);
    assert_eq!(values(&mut c, "git add tr"), ["tracked.txt"]);
    assert_eq!(values(&mut c, "git commit -m tr"), ["tracked.txt"]);
}

#[test]
fn a_ref_command_falls_back_to_files_when_no_name_fits() {
    let s = repo();
    let mut c = in_repo(&s);
    assert_eq!(values(&mut c, "git checkout tr"), ["tracked.txt"]);
    assert_eq!(values(&mut c, "git diff -- tr"), ["tracked.txt"]);
}

#[test]
fn outside_a_repository_git_arguments_are_file_names() {
    let s = Scratch::new();
    s.write("plain.txt", "");
    let mut c = plain(&s.0).0;
    assert_eq!(values(&mut c, "git checkout pl"), ["plain.txt"]);
}

// ---- where nothing is completed ----

#[test]
fn an_ai_request_a_comment_and_an_assignment_get_nothing() {
    let s = Scratch::new();
    s.write("file", "");
    let facts = Facts {
        variables: words(&["HOME"]),
        ..Facts::default()
    };
    let (mut c, _) = with_facts(facts, &s.0);
    for line in [
        "? list fi",
        "?? ls fi",
        "? what is $HO",
        "ls # fi",
        "FOO=fi",
        "cat <<EOF\nfi",
        "ls | ? fi",
    ] {
        assert!(values(&mut c, line).is_empty(), "{line:?}");
    }
    assert_eq!(c.helpers_started(), 0);
}

#[test]
fn a_cursor_in_the_middle_of_the_line_completes_the_word_before_it() {
    let s = Scratch::new();
    s.write("alpha", "");
    let (mut c, _) = plain(&s.0);
    let result = c.complete("ls al && echo", 5);
    assert_eq!(shown(result.suggestions()), ["alpha"]);
    assert_eq!(result.suggestions()[0].span, Span::new(3, 5));
    // A position inside a character does not panic.
    assert!(c.complete("é", 1).suggestions().is_empty());
    assert!(c.complete("ls", 50).suggestions().is_empty());
}

#[test]
fn nothing_is_pending_when_nothing_was_asked() {
    let s = Scratch::new();
    let (mut c, _) = plain(&s.0);
    assert_eq!(c.poll_completion(), CompletionStatus::Idle);
}
