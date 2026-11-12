//! Tab completion: command names, file names, variables, git branches and flags.
//!
//! [`ShellCompleter`] is what the line editor asks when Tab is pressed. It looks at the text
//! before the cursor ([`site`]), decides what kind of word is being typed, and asks one source:
//!
//! - a command name: functions, aliases, builtins and the programs in `PATH` ([`index`]);
//! - `$name` or `${name`: the shell's variables;
//! - an argument of `git`: its sub-commands, then branches, tags and remotes read from the
//!   repository's files ([`git`]);
//! - a word that starts with `-`: the flags in fish's completion files ([`fish`]);
//! - anything else: file names ([`paths`]).
//!
//! **Nothing here starts a process or opens a socket.** Two sources can be slow the first time:
//! listing every directory of `PATH` and reading a fish completion file. They run on a helper
//! thread that is started by the first Tab that needs them, never before (the start of the
//! shell and the first prompt do not pay for completion). While the thread works the completer
//! answers [`CompletionResult::Pending`] and the menu fills in when it is done. Asking for
//! something else in the meantime cancels the old work: every request has a number, and an
//! answer that comes back with an old number is dropped.

mod fish;
mod git;
mod index;
mod paths;
mod site;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;

use reedline::{Completer, CompletionResult, CompletionStatus, Span, Suggestion};

use self::fish::Flag;
use self::index::PathIndex;
use self::site::{Site, Slot, render};

pub use self::fish::directories as fish_completion_directories;

/// The most suggestions one Tab returns. A menu of more is of no use to a person.
const MAX_SUGGESTIONS: usize = 1000;

/// What the completer knows about the shell. The shell fills it in before each line is read, so
/// a function defined by the last command or a new variable is offered at once.
#[derive(Clone, Debug, Default)]
pub struct Facts {
    pub functions: Vec<String>,
    pub aliases: Vec<String>,
    /// Builtins a person can type (not the internal ones).
    pub builtins: Vec<String>,
    /// Names of the shell variables that are set.
    pub variables: Vec<String>,
    /// The value of `PATH`.
    pub path: Vec<u8>,
    /// Where to look for fish completion files (see [`fish_completion_directories`]).
    pub fish_dirs: Vec<PathBuf>,
}

/// The facts of the prompt being typed: what [`ShellCompleter`] looks at. The editor renews it
/// before each line is read.
#[derive(Debug, Default)]
pub struct Scene {
    facts: Facts,
    cwd: PathBuf,
    home: Option<PathBuf>,
    /// Raised at every prompt: an answer remembered for an earlier prompt is not used.
    epoch: u64,
}

impl Scene {
    /// Starts a new prompt: new facts, and no answer remembered for an earlier prompt is used.
    pub fn reset(&mut self, facts: Facts, cwd: PathBuf, home: Option<PathBuf>) {
        self.facts = facts;
        self.cwd = cwd;
        self.home = home;
        self.epoch += 1;
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What a helper thread builds.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Key {
    /// The index of the programs in this `PATH`.
    Index(Vec<u8>),
    /// The flags of this command.
    Flags(String),
}

/// What is remembered between Tabs, and shared with the helper thread.
#[derive(Default)]
struct Cache {
    index: Option<Arc<PathIndex>>,
    flags: std::collections::HashMap<String, Arc<Vec<Flag>>>,
    /// What the running helper builds, if one runs.
    loading: Option<Key>,
    /// A helper finished and the editor has not been told yet.
    ready: bool,
    /// The number of the request that counts. A helper whose number is older was replaced.
    generation: u64,
    /// How many helper threads were started (for tests).
    started: usize,
}

/// The slow work, apart from the completer so that a test can slow it down or stop it.
pub(crate) trait Loader: Send + Sync {
    fn path_index(&self, path: &[u8], cancelled: &dyn Fn() -> bool) -> PathIndex;
    fn flags(&self, command: &str, dirs: &[PathBuf], cancelled: &dyn Fn() -> bool) -> Vec<Flag>;
}

/// Reads the real file system.
struct DiskLoader;

impl Loader for DiskLoader {
    fn path_index(&self, path: &[u8], cancelled: &dyn Fn() -> bool) -> PathIndex {
        PathIndex::build(path, cancelled).unwrap_or_else(|| PathIndex::empty(path))
    }

    fn flags(&self, command: &str, dirs: &[PathBuf], cancelled: &dyn Fn() -> bool) -> Vec<Flag> {
        fish::load(command, dirs, cancelled).unwrap_or_default()
    }
}

/// The answer to one request, or "not yet".
enum Answer {
    Done(Vec<Suggestion>),
    Wait,
}

/// An answer worth keeping for the same line and cursor in the same prompt.
struct Memo {
    epoch: u64,
    line: String,
    pos: usize,
    suggestions: Arc<[Suggestion]>,
}

/// The completer reedline calls on Tab.
pub struct ShellCompleter {
    scene: Arc<Mutex<Scene>>,
    cache: Arc<Mutex<Cache>>,
    loader: Arc<dyn Loader>,
    memo: Option<Memo>,
}

impl ShellCompleter {
    /// A completer that looks at `scene`.
    #[must_use]
    pub fn new(scene: Arc<Mutex<Scene>>) -> Self {
        Self::with_loader(scene, Arc::new(DiskLoader))
    }

    pub(crate) fn with_loader(scene: Arc<Mutex<Scene>>, loader: Arc<dyn Loader>) -> Self {
        Self {
            scene,
            cache: Arc::new(Mutex::new(Cache::default())),
            loader,
            memo: None,
        }
    }

    /// How many helper threads were started so far.
    #[cfg(test)]
    pub(crate) fn helpers_started(&self) -> usize {
        lock(&self.cache).started
    }

    /// Asks for `key` to be built. Does nothing if it is being built already; otherwise any
    /// other work in progress is cancelled.
    fn request(&self, key: Key, dirs: &[PathBuf]) {
        let generation = {
            let mut cache = lock(&self.cache);
            if cache.loading.as_ref() == Some(&key) {
                return;
            }
            cache.generation += 1;
            cache.loading = Some(key.clone());
            cache.started += 1;
            cache.generation
        };
        let shared = Arc::clone(&self.cache);
        let loader = Arc::clone(&self.loader);
        let dirs = dirs.to_vec();
        let spawned = thread::Builder::new()
            .name("completion".to_owned())
            .spawn(move || {
                let cancelled = || lock(&shared).generation != generation;
                let built = match &key {
                    Key::Index(path) => Built::Index(loader.path_index(path, &cancelled)),
                    Key::Flags(command) => {
                        Built::Flags(command.clone(), loader.flags(command, &dirs, &cancelled))
                    }
                };
                let mut cache = lock(&shared);
                // Replaced while it worked: the answer is for a question nobody asks any more.
                if cache.generation != generation {
                    return;
                }
                cache.loading = None;
                cache.ready = true;
                match built {
                    Built::Index(index) => cache.index = Some(Arc::new(index)),
                    Built::Flags(command, flags) => {
                        cache.flags.insert(command, Arc::new(flags));
                    }
                }
            });
        if spawned.is_err() {
            // No thread: give up on this one. Nothing is stored, so the next Tab tries again.
            lock(&self.cache).loading = None;
        }
    }

    /// The suggestions for the word at `site`.
    fn answer(&self, site: &Site, scene: &Scene) -> Answer {
        if let Some(variable) = site.variable() {
            return Answer::Done(variables(site, &variable, &scene.facts));
        }
        let Some(text) = site.decoded() else {
            return Answer::Done(Vec::new());
        };
        let command = site.command.as_deref().map(base_name);
        match site.slot {
            Slot::Command => {
                if text.contains('/') || site.raw.starts_with('~') {
                    return Answer::Done(file_names(site, &text, scene, Only::Programs));
                }
                self.command_names(site, &text, scene)
            }
            Slot::Target => Answer::Done(file_names(site, &text, scene, Only::Any)),
            Slot::Argument => {
                if text.starts_with('-')
                    && let Some(command) = command
                {
                    return self.flags(site, &text, command, scene);
                }
                if command == Some("git")
                    && let Some(found) = git_names(site, &text, scene)
                {
                    return Answer::Done(found);
                }
                let only = if matches!(command, Some("cd" | "pushd")) {
                    Only::Folders
                } else {
                    Only::Any
                };
                Answer::Done(file_names(site, &text, scene, only))
            }
        }
    }

    fn command_names(&self, site: &Site, typed: &str, scene: &Scene) -> Answer {
        // Every program in PATH starts with something: an empty word would list thousands.
        if typed.is_empty() {
            return Answer::Done(Vec::new());
        }
        let index = lock(&self.cache)
            .index
            .clone()
            .filter(|index| index.is_fresh(&scene.facts.path));
        let Some(index) = index else {
            self.request(Key::Index(scene.facts.path.clone()), &[]);
            return Answer::Wait;
        };
        // A name can be several things; the first label wins, and a program has none.
        let mut names: BTreeMap<&str, &'static str> = BTreeMap::new();
        for (list, label) in [
            (&scene.facts.builtins, "builtin"),
            (&scene.facts.aliases, "alias"),
            (&scene.facts.functions, "function"),
        ] {
            for name in list.iter().filter(|name| name.starts_with(typed)) {
                names.insert(name, label);
            }
        }
        for name in index.matching(typed, MAX_SUGGESTIONS) {
            names.entry(name).or_insert("");
        }
        let quote = site.quote();
        let suggestions = names
            .into_iter()
            .take(MAX_SUGGESTIONS)
            .filter_map(|(name, label)| {
                let value = render(quote, false, name, true)?;
                Some(Suggestion {
                    display_override: (value != name).then(|| name.to_owned()),
                    value,
                    description: (!label.is_empty()).then(|| label.to_owned()),
                    span: span_of(site),
                    append_whitespace: true,
                    ..Suggestion::default()
                })
            })
            .collect();
        Answer::Done(suggestions)
    }

    fn flags(&self, site: &Site, typed: &str, command: &str, scene: &Scene) -> Answer {
        let known = lock(&self.cache).flags.get(command).cloned();
        let Some(flags) = known else {
            self.request(Key::Flags(command.to_owned()), &scene.facts.fish_dirs);
            return Answer::Wait;
        };
        let quote = site.quote();
        let suggestions = fish::candidates(&flags, typed)
            .into_iter()
            .take(MAX_SUGGESTIONS)
            .filter_map(|(form, description)| {
                Some(Suggestion {
                    value: render(quote, false, &form, true)?,
                    description: (!description.is_empty()).then_some(description),
                    span: span_of(site),
                    append_whitespace: true,
                    ..Suggestion::default()
                })
            })
            .collect();
        Answer::Done(suggestions)
    }
}

/// What a helper thread brought back.
enum Built {
    Index(PathIndex),
    Flags(String, Vec<Flag>),
}

impl Completer for ShellCompleter {
    fn complete(&mut self, line: &str, pos: usize) -> CompletionResult {
        let scene = lock(&self.scene);
        if let Some(memo) = &self.memo
            && memo.epoch == scene.epoch
            && memo.pos == pos
            && memo.line == line
        {
            return CompletionResult::fresh(Arc::clone(&memo.suggestions));
        }
        let Some(site) = site::locate(line, pos) else {
            return CompletionResult::fresh(Vec::new());
        };
        match self.answer(&site, &scene) {
            Answer::Wait => CompletionResult::Pending,
            Answer::Done(list) => {
                let suggestions: Arc<[Suggestion]> = list.into();
                self.memo = Some(Memo {
                    epoch: scene.epoch,
                    line: line.to_owned(),
                    pos,
                    suggestions: Arc::clone(&suggestions),
                });
                CompletionResult::fresh(suggestions)
            }
        }
    }

    fn poll_completion(&mut self) -> CompletionStatus {
        let mut cache = lock(&self.cache);
        if cache.loading.is_some() {
            CompletionStatus::Pending
        } else if std::mem::take(&mut cache.ready) {
            CompletionStatus::Ready
        } else {
            CompletionStatus::Idle
        }
    }
}

/// Where the replacement goes: the word under the cursor.
fn span_of(site: &Site) -> Span {
    Span::new(site.start, site.end)
}

/// The name of a command without the folder it is called by (`/usr/bin/git` is `git`).
fn base_name(command: &str) -> &str {
    Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(command)
}

/// What kind of file names to offer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Only {
    Any,
    Folders,
    /// Folders, and files that can be run.
    Programs,
}

/// File names that fit the word.
fn file_names(site: &Site, typed: &str, scene: &Scene, only: Only) -> Vec<Suggestion> {
    let tilde = site.raw.starts_with('~');
    let query = paths::Query {
        cwd: &scene.cwd,
        home: scene.home.as_deref(),
        tilde,
        text: typed,
        dirs_only: only == Only::Folders,
        programs_only: only == Only::Programs,
    };
    let quote = site.quote();
    paths::list(&query, MAX_SUGGESTIONS)
        .into_iter()
        .filter_map(|entry| {
            // A folder ends with a slash, and its quote stays open so that the next Tab goes on.
            let full = if entry.is_dir {
                format!("{}/", entry.full)
            } else {
                entry.full
            };
            let shown = if entry.is_dir {
                format!("{}/", entry.name)
            } else {
                entry.name
            };
            Some(Suggestion {
                value: render(quote, tilde, &full, !entry.is_dir)?,
                display_override: Some(shown),
                span: span_of(site),
                append_whitespace: !entry.is_dir,
                ..Suggestion::default()
            })
        })
        .collect()
}

/// Names of shell variables that start with what was typed after the `$`.
fn variables(site: &Site, variable: &site::Variable, facts: &Facts) -> Vec<Suggestion> {
    let identifier = |name: &str| {
        let mut chars = name.chars();
        chars
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    };
    facts
        .variables
        .iter()
        .filter(|name| name.starts_with(&variable.typed) && identifier(name))
        .take(MAX_SUGGESTIONS)
        .map(|name| Suggestion {
            value: if variable.braced {
                format!("{name}}}")
            } else {
                name.clone()
            },
            display_override: Some(name.clone()),
            span: Span::new(variable.start, site.end),
            ..Suggestion::default()
        })
        .collect()
}

/// The git sub-commands, or the branches, tags and remotes, for the word at `site`. `None` when
/// the word should be completed as a file name instead.
fn git_names(site: &Site, typed: &str, scene: &Scene) -> Option<Vec<Suggestion>> {
    let make = |names: Vec<(String, &str)>| -> Vec<Suggestion> {
        let quote = site.quote();
        names
            .into_iter()
            .take(MAX_SUGGESTIONS)
            .filter_map(|(name, label)| {
                Some(Suggestion {
                    value: render(quote, false, &name, true)?,
                    display_override: Some(name),
                    description: (!label.is_empty()).then(|| label.to_owned()),
                    span: span_of(site),
                    append_whitespace: true,
                    ..Suggestion::default()
                })
            })
            .collect()
    };
    let with_label = |names: &[String], label: &'static str| -> Vec<(String, &'static str)> {
        names
            .iter()
            .filter(|name| name.starts_with(typed))
            .map(|name| (name.clone(), label))
            .collect()
    };
    match git::place(&site.before) {
        git::Place::Subcommand if !typed.contains('/') => {
            let names = git::SUBCOMMANDS
                .iter()
                .filter(|name| name.starts_with(typed))
                .map(|name| ((*name).to_owned(), ""))
                .collect();
            Some(make(names))
        }
        place @ (git::Place::Ref | git::Place::Remote) => {
            let found = git::refs(&scene.cwd)?;
            let mut names = Vec::new();
            if place == git::Place::Remote {
                names.extend(with_label(&found.remotes, "remote"));
            } else {
                names.extend(with_label(&found.branches, "branch"));
                names.extend(with_label(&found.remote_branches, "remote branch"));
                names.extend(with_label(&found.tags, "tag"));
            }
            // Nothing fits: it may be a file (`git checkout file`).
            (!names.is_empty()).then(|| make(names))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests;
