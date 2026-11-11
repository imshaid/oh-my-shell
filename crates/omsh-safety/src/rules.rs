//! The rule table (§12.2). Each rule is data: an id, the highest level it can give, a reason in
//! plain words, whether a trash alternative exists, and a matcher over the syntax tree.

use std::borrow::Cow;
use std::fmt;
use std::path::{Path, PathBuf};

use omsh_syntax::ast::{Command, CommandKind, FunctionDef, List, Pipeline, RedirectOp};

use crate::command::{Invocation, Options, base_name};
use crate::context::Probe;
use crate::level::Level;
use crate::paths::{self, Class};
use crate::word::{Arg, static_text};

/// The facts a matcher may use besides the command itself.
pub struct Env<'e> {
    /// The working directory, as far as the analysis knows it.
    pub cwd: Option<&'e str>,
    /// What `~` means.
    pub home: Option<&'e str>,
    /// Questions about files.
    pub probe: &'e dyn Probe,
}

/// What a matcher found.
#[derive(Clone, Debug)]
pub struct Hit {
    /// How risky this case is. Never above the rule's own level.
    pub level: Level,
    /// The thing it acts on, as the person wrote it. A Critical confirmation asks for it.
    pub target: Option<String>,
    /// A sentence that goes after the rule's reason.
    pub note: Option<Cow<'static, str>>,
}

impl Hit {
    const fn new(level: Level) -> Self {
        Self {
            level,
            target: None,
            note: None,
        }
    }

    fn on(level: Level, arg: &Arg<'_>) -> Self {
        Self {
            level,
            target: Some(arg.src.clone()),
            note: arg.unknown.then_some(Cow::Borrowed(EMPTY_VARIABLE)),
        }
    }
}

const EMPTY_VARIABLE: &str = "A variable in the path may be empty.";

/// How a rule looks at the tree.
#[derive(Clone, Copy)]
pub enum Matcher {
    /// A simple command, with wrappers such as `sudo` peeled off.
    Command(fn(&Invocation<'_>, &Env<'_>) -> Option<Hit>),
    /// The commands of a pipeline with two or more stages (`None` for a stage that is not a
    /// simple command).
    Pipeline(fn(&[Option<Invocation<'_>>]) -> Option<Hit>),
    /// A function definition.
    Function(fn(&FunctionDef) -> Option<Hit>),
    /// A redirection with a target: its operator and the word after it.
    Redirect(fn(RedirectOp, &Arg<'_>, &Env<'_>) -> Option<Hit>),
}

/// One rule of the table.
pub struct Rule {
    /// A stable name, used in tests and by the AI layer.
    pub id: &'static str,
    /// The highest level the rule can give.
    pub level: Level,
    /// Why, in plain words.
    pub reason: &'static str,
    /// True if moving to the trash would be a gentler way to do the same (S18).
    pub trash_alternative: bool,
    /// True if the command acts on files, so the confirmation can count what it would touch
    /// (§12.3).
    pub blast: bool,
    /// How to find it.
    pub matcher: Matcher,
}

impl fmt::Debug for Rule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Rule")
            .field("id", &self.id)
            .field("level", &self.level)
            .finish_non_exhaustive()
    }
}

/// Every rule, in the order they are tried.
pub static RULES: &[Rule] = &[
    Rule {
        id: "rm-critical-path",
        level: Level::Critical,
        reason: "Deletes the system, your home folder, or everything in one of them. \
                 This cannot be undone.",
        trash_alternative: false,
        blast: true,
        matcher: Matcher::Command(rm_critical_path),
    },
    Rule {
        id: "rm-recursive-or-force",
        level: Level::Dangerous,
        reason: "Deletes files without asking, and folders with everything in them. \
                 This cannot be undone.",
        trash_alternative: true,
        blast: true,
        matcher: Matcher::Command(rm_recursive_or_force),
    },
    Rule {
        id: "rm-glob",
        level: Level::Dangerous,
        reason: "Deletes every file the pattern matches. This cannot be undone.",
        trash_alternative: true,
        blast: true,
        matcher: Matcher::Command(rm_glob),
    },
    Rule {
        id: "find-delete",
        level: Level::Dangerous,
        reason: "Deletes every file that the tests of find match, without asking about each \
                 one. This cannot be undone.",
        trash_alternative: false,
        blast: true,
        matcher: Matcher::Command(find_delete),
    },
    Rule {
        id: "destructive-unknown-target",
        level: Level::Dangerous,
        reason: "What this acts on is only known when it runs, so it could be anything.",
        trash_alternative: false,
        blast: true,
        matcher: Matcher::Command(destructive_unknown_target),
    },
    Rule {
        id: "dd-to-device",
        level: Level::Critical,
        reason: "Writes straight onto a disk or device and destroys what is on it.",
        trash_alternative: false,
        blast: false,
        matcher: Matcher::Command(dd_to_device),
    },
    Rule {
        id: "format-or-wipe",
        level: Level::Critical,
        reason: "Formats, wipes or overwrites a disk or a file for good.",
        trash_alternative: false,
        blast: false,
        matcher: Matcher::Command(format_or_wipe),
    },
    Rule {
        id: "write-to-disk-device",
        level: Level::Critical,
        reason: "Writes straight onto a disk and destroys what is on it.",
        trash_alternative: false,
        blast: false,
        matcher: Matcher::Redirect(write_to_disk_device),
    },
    Rule {
        id: "chmod-chown-system-recursive",
        level: Level::Critical,
        reason: "Changes the owner or the permissions of everything in a system folder, \
                 which can break the machine.",
        trash_alternative: false,
        blast: true,
        matcher: Matcher::Command(chmod_chown_system_recursive),
    },
    Rule {
        id: "chmod-777-recursive",
        level: Level::Dangerous,
        reason: "Gives everyone full access to everything under the path.",
        trash_alternative: false,
        blast: true,
        matcher: Matcher::Command(chmod_777_recursive),
    },
    Rule {
        id: "fork-bomb",
        level: Level::Critical,
        reason: "A function that starts copies of itself without end. It freezes the machine.",
        trash_alternative: false,
        blast: false,
        matcher: Matcher::Function(fork_bomb),
    },
    Rule {
        id: "firewall-open",
        level: Level::Dangerous,
        reason: "Opens the firewall or turns it off, which exposes this machine to the network.",
        trash_alternative: false,
        blast: false,
        matcher: Matcher::Command(firewall_open),
    },
    Rule {
        id: "listen-socket",
        level: Level::Dangerous,
        reason: "Opens a port that anyone who can reach this machine can connect to.",
        trash_alternative: false,
        blast: false,
        matcher: Matcher::Command(listen_socket),
    },
    Rule {
        id: "account-create",
        level: Level::Dangerous,
        reason: "Creates an account with no password set up.",
        trash_alternative: false,
        blast: false,
        matcher: Matcher::Command(account_create),
    },
    Rule {
        id: "account-no-password",
        level: Level::Dangerous,
        reason: "Removes a password, so anyone can log in to that account.",
        trash_alternative: false,
        blast: false,
        matcher: Matcher::Command(account_no_password),
    },
    Rule {
        id: "account-admin-group",
        level: Level::Dangerous,
        reason: "Gives an account administrator rights.",
        trash_alternative: false,
        blast: false,
        matcher: Matcher::Command(account_admin_group),
    },
    Rule {
        id: "download-to-shell",
        level: Level::Dangerous,
        reason: "Runs a script straight from the network, with no chance to read it first.",
        trash_alternative: false,
        blast: false,
        matcher: Matcher::Pipeline(download_to_shell),
    },
    Rule {
        id: "download-eval",
        level: Level::Dangerous,
        reason: "Runs a script straight from the network, with no chance to read it first.",
        trash_alternative: false,
        blast: false,
        matcher: Matcher::Command(download_eval),
    },
    Rule {
        id: "git-force-push",
        level: Level::Dangerous,
        reason: "Replaces the history on the remote, which can erase other people's work.",
        trash_alternative: false,
        blast: false,
        matcher: Matcher::Command(git_force_push),
    },
    Rule {
        id: "git-reset-hard",
        level: Level::Dangerous,
        reason: "Throws away every change that is not committed.",
        trash_alternative: false,
        blast: false,
        matcher: Matcher::Command(git_reset_hard),
    },
    Rule {
        id: "git-clean",
        level: Level::Dangerous,
        reason: "Deletes the files that git does not track, for good.",
        trash_alternative: false,
        blast: false,
        matcher: Matcher::Command(git_clean),
    },
    Rule {
        id: "overwrite-file",
        level: Level::Caution,
        reason: "Replaces the content of a file that already has content.",
        trash_alternative: false,
        blast: false,
        matcher: Matcher::Redirect(overwrite_file),
    },
];

// ---------------------------------------------------------------------------------------
// Helpers.

fn class_of(arg: &Arg<'_>, env: &Env<'_>) -> Class {
    paths::classify(&arg.text, arg.glob, env.cwd, env.home)
}

/// `-r`, `-R` or `--recursive`, for the programs that spell it with a capital too.
fn recursive(options: &Options<'_, '_>) -> bool {
    options.has_short('r') || options.has_short('R') || options.has_long("recursive")
}

/// `rm` that does not stop to ask or to refuse.
fn forceful(options: &Options<'_, '_>) -> bool {
    recursive(options)
        || options.has_short('f')
        || options.has_long("force")
        || options.has_long("no-preserve-root")
}

/// The name of the first command of a list item, if it is a simple command with a name that
/// is the same on every run.
fn simple_name(command: &Command) -> Option<String> {
    match &command.kind {
        CommandKind::Simple(simple) => simple
            .words
            .first()
            .and_then(static_text)
            .map(|name| base_name(&name).to_owned()),
        _ => None,
    }
}

/// Every pipeline of a list, at its own level.
fn pipelines(list: &List) -> impl Iterator<Item = &Pipeline> {
    list.items.iter().flat_map(|item| {
        std::iter::once(&item.and_or.first)
            .chain(item.and_or.rest.iter().map(|part| &part.pipeline))
    })
}

/// True if a pipeline of `list` runs one of `names`.
fn list_runs(list: &List, names: &[&str]) -> bool {
    pipelines(list).any(|pipeline| {
        pipeline
            .commands
            .iter()
            .any(|command| simple_name(command).is_some_and(|name| names.contains(&name.as_str())))
    })
}

// ---------------------------------------------------------------------------------------
// rm.

fn rm_critical_path(inv: &Invocation<'_>, env: &Env<'_>) -> Option<Hit> {
    if inv.name != "rm" {
        return None;
    }
    let options = inv.options();
    let target = options
        .operands
        .iter()
        .find(|arg| class_of(arg, env).is_critical())?;
    // Without `-r` or `-f`, `rm` refuses a folder, but `rm /*` still removes the files.
    let level = if forceful(&options) {
        Level::Critical
    } else {
        Level::Dangerous
    };
    Some(Hit::on(level, target))
}

fn rm_recursive_or_force(inv: &Invocation<'_>, env: &Env<'_>) -> Option<Hit> {
    if inv.name != "rm" {
        return None;
    }
    let options = inv.options();
    if !forceful(&options) || options.operands.is_empty() {
        return None;
    }
    // The other rule already speaks for these.
    if options
        .operands
        .iter()
        .any(|arg| class_of(arg, env).is_critical())
    {
        return None;
    }
    Some(Hit::on(Level::Dangerous, options.operands[0]))
}

/// More matches than this, and a plain `rm` with a pattern is Dangerous (§12.2 "globs that
/// expand to many items").
pub const MANY_MATCHES: usize = 20;

/// How many names the patterns among `targets` match, if every one of them can be expanded.
fn matches_of(targets: &[&Arg<'_>], env: &Env<'_>) -> Option<usize> {
    let mut total = 0_usize;
    for arg in targets {
        // With a part only known at run time the pattern is not the text we have.
        if arg.unknown {
            return None;
        }
        let base: PathBuf = if arg.text.starts_with('/') {
            PathBuf::from("/")
        } else {
            PathBuf::from(env.cwd?)
        };
        let found = env.probe.glob(arg.text.as_bytes(), &arg.active, &base)?;
        total = total.saturating_add(found.len());
    }
    Some(total)
}

fn rm_glob(inv: &Invocation<'_>, env: &Env<'_>) -> Option<Hit> {
    if inv.name != "rm" {
        return None;
    }
    let options = inv.options();
    if forceful(&options) {
        return None;
    }
    let patterns: Vec<&Arg<'_>> = options
        .operands
        .iter()
        .copied()
        .filter(|arg| arg.glob && !class_of(arg, env).is_critical())
        .collect();
    let target = *patterns.first()?;
    let mut hit = Hit::on(Level::Caution, target);
    if let Some(count) = matches_of(&patterns, env)
        && count > MANY_MATCHES
    {
        hit.level = Level::Dangerous;
        hit.note = Some(Cow::Owned(format!("The pattern matches {count} entries.")));
    }
    Some(hit)
}

fn find_delete(inv: &Invocation<'_>, _env: &Env<'_>) -> Option<Hit> {
    if inv.name != "find" {
        return None;
    }
    let find = crate::find::parse(inv);
    if !find.deletes {
        return None;
    }
    Some(match find.starts.first() {
        Some(start) => Hit::on(Level::Dangerous, start),
        None => {
            let mut hit = Hit::new(Level::Dangerous);
            hit.target = Some(".".to_owned());
            hit
        }
    })
}

fn destructive_unknown_target(inv: &Invocation<'_>, env: &Env<'_>) -> Option<Hit> {
    let options = inv.options();
    match inv.name.as_str() {
        "rm" => {
            let force = forceful(&options);
            let target = options.operands.iter().find(|arg| {
                arg.unknown && !class_of(arg, env).is_critical() && (arg.cmdsub || force)
            })?;
            Some(Hit::on(Level::Dangerous, target))
        }
        "chmod" | "chown" | "chgrp" if options.has_short('R') || options.has_long("recursive") => {
            let skip = usize::from(!options.has_long("reference"));
            let target = options
                .operands
                .iter()
                .skip(skip)
                .find(|arg| arg.unknown && !class_of(arg, env).is_system())?;
            Some(Hit::on(Level::Dangerous, target))
        }
        "dd" => {
            let target = inv
                .args
                .iter()
                .find(|arg| arg.unknown && arg.src.starts_with("of="))?;
            Some(Hit::on(Level::Dangerous, target))
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------
// Disks.

/// Devices that `dd of=` can name without harm.
fn harmless_device(name: &str) -> bool {
    matches!(
        name,
        "null"
            | "zero"
            | "full"
            | "tty"
            | "stdout"
            | "stderr"
            | "stdin"
            | "random"
            | "urandom"
            | "ptmx"
    ) || name.starts_with("fd/")
        || name.starts_with("pts/")
        || name.starts_with("shm/")
}

fn dd_to_device(inv: &Invocation<'_>, _env: &Env<'_>) -> Option<Hit> {
    if inv.name != "dd" {
        return None;
    }
    let target = inv.args.iter().find(|arg| {
        arg.text
            .strip_prefix("of=/dev/")
            .is_some_and(|device| !harmless_device(device))
    })?;
    Some(Hit::on(Level::Critical, target))
}

fn format_or_wipe(inv: &Invocation<'_>, _env: &Env<'_>) -> Option<Hit> {
    let name = inv.name.as_str();
    if !(matches!(name, "mkfs" | "mke2fs" | "wipefs" | "shred") || name.starts_with("mkfs.")) {
        return None;
    }
    if inv.only_asks_for_help() {
        return None;
    }
    let mut hit = Hit::new(Level::Critical);
    hit.target = inv.options().operands.last().map(|arg| arg.src.clone());
    Some(hit)
}

/// Whole-disk and partition names under `/dev`.
const DISK_PREFIXES: &[&str] = &[
    "sd", "nvme", "mmcblk", "vd", "hd", "xvd", "dm-", "mapper/", "md", "loop",
];

fn write_to_disk_device(op: RedirectOp, target: &Arg<'_>, _env: &Env<'_>) -> Option<Hit> {
    let writes = matches!(
        op,
        RedirectOp::Output
            | RedirectOp::Append
            | RedirectOp::Clobber
            | RedirectOp::ReadWrite
            | RedirectOp::OutputAndError
            | RedirectOp::AppendAndError
    );
    if !writes || target.unknown {
        return None;
    }
    let device = target.text.strip_prefix("/dev/")?;
    DISK_PREFIXES
        .iter()
        .any(|prefix| device.starts_with(prefix))
        .then(|| Hit::on(Level::Critical, target))
}

// ---------------------------------------------------------------------------------------
// Permissions.

fn chmod_chown_system_recursive(inv: &Invocation<'_>, env: &Env<'_>) -> Option<Hit> {
    if !matches!(inv.name.as_str(), "chmod" | "chown" | "chgrp") {
        return None;
    }
    let options = inv.options();
    if !(options.has_short('R') || options.has_long("recursive")) {
        return None;
    }
    // The first operand is the mode or the owner.
    let skip = usize::from(!options.has_long("reference"));
    let target = options
        .operands
        .iter()
        .skip(skip)
        .find(|arg| class_of(arg, env).is_system())?;
    Some(Hit::on(Level::Critical, target))
}

fn chmod_777_recursive(inv: &Invocation<'_>, _env: &Env<'_>) -> Option<Hit> {
    if inv.name != "chmod" {
        return None;
    }
    let options = inv.options();
    if !(options.has_short('R') || options.has_long("recursive")) {
        return None;
    }
    // The mode, and at least one path after it.
    if options.operands.len() < 2 {
        return None;
    }
    let mode = options.operands.first()?;
    let open = matches!(
        mode.text.as_str(),
        "777" | "0777" | "a+rwx" | "ugo+rwx" | "a=rwx" | "ugo=rwx"
    );
    if !open {
        return None;
    }
    let mut hit = Hit::new(Level::Dangerous);
    hit.target = options.operands.last().map(|arg| arg.src.clone());
    Some(hit)
}

// ---------------------------------------------------------------------------------------
// Fork bombs.

fn fork_bomb(def: &FunctionDef) -> Option<Hit> {
    let mut hit = Hit::new(Level::Critical);
    hit.target = Some(def.name.clone());
    bomb_in_command(&def.name, &def.body).then_some(hit)
}

fn bomb_in_command(name: &str, command: &Command) -> bool {
    match &command.kind {
        CommandKind::Simple(_) => false,
        CommandKind::Subshell(list) | CommandKind::Group(list) => bomb_in_list(name, list),
        CommandKind::If(branch) => {
            branch
                .branches
                .iter()
                .any(|b| bomb_in_list(name, &b.condition) || bomb_in_list(name, &b.body))
                || branch
                    .else_body
                    .as_ref()
                    .is_some_and(|body| bomb_in_list(name, body))
        }
        CommandKind::While(lp) | CommandKind::Until(lp) => {
            bomb_in_list(name, &lp.condition) || bomb_in_list(name, &lp.body)
        }
        CommandKind::For(lp) => bomb_in_list(name, &lp.body),
        CommandKind::Case(case) => case.arms.iter().any(|arm| bomb_in_list(name, &arm.body)),
        CommandKind::Function(def) => bomb_in_command(name, &def.body),
    }
}

/// A call of `name` that is piped to another command or sent to the background, in a list.
fn bomb_in_list(name: &str, list: &List) -> bool {
    list.items.iter().any(|item| {
        std::iter::once(&item.and_or.first)
            .chain(item.and_or.rest.iter().map(|part| &part.pipeline))
            .any(|pipeline| {
                let calls = pipeline
                    .commands
                    .iter()
                    .any(|c| simple_name(c).as_deref() == Some(name));
                (calls && (item.background || pipeline.commands.len() >= 2))
                    || pipeline.commands.iter().any(|c| bomb_in_command(name, c))
            })
    })
}

// ---------------------------------------------------------------------------------------
// Network and accounts.

fn firewall_open(inv: &Invocation<'_>, _env: &Env<'_>) -> Option<Hit> {
    let words: Vec<&str> = inv.args.iter().map(|arg| arg.text.as_str()).collect();
    let options = inv.options();
    let name = inv.name.as_str();
    let found = match name {
        "ufw" => {
            let first = options.operands.first().map(|arg| arg.text.as_str());
            let second = options.operands.get(1).map(|arg| arg.text.as_str());
            matches!(first, Some("allow" | "disable" | "reset"))
                || (first == Some("default") && second == Some("allow"))
        }
        _ if name.starts_with("iptables") || name.starts_with("ip6tables") => {
            words
                .windows(2)
                .any(|pair| matches!(pair[0], "-j" | "--jump") && pair[1] == "ACCEPT")
                || words.iter().any(|word| matches!(*word, "-F" | "--flush"))
                || words
                    .windows(3)
                    .any(|triple| triple[0] == "-P" && triple[2] == "ACCEPT")
        }
        "nft" => {
            options.operands.first().map(|arg| arg.text.as_str()) == Some("flush")
                && options.operands.get(1).map(|arg| arg.text.as_str()) == Some("ruleset")
        }
        "firewall-cmd" => words
            .iter()
            .any(|word| word.starts_with("--add-port") || word.starts_with("--add-service")),
        _ => false,
    };
    found.then(|| Hit::new(Level::Dangerous))
}

fn listen_socket(inv: &Invocation<'_>, _env: &Env<'_>) -> Option<Hit> {
    let found = match inv.name.as_str() {
        "nc" | "ncat" | "netcat" => {
            let options = inv.options();
            options.has_short('l') || options.has_long("listen")
        }
        "socat" => inv
            .args
            .iter()
            .any(|arg| arg.text.to_ascii_uppercase().contains("LISTEN")),
        _ => false,
    };
    found.then(|| Hit::new(Level::Dangerous))
}

const ADMIN_GROUPS: &[&str] = &["sudo", "wheel", "admin"];

fn account_create(inv: &Invocation<'_>, _env: &Env<'_>) -> Option<Hit> {
    let options = inv.options();
    let found = match inv.name.as_str() {
        // `useradd` makes a locked account unless it is given a password hash.
        "useradd" => {
            !(options.has_short('p') || options.has_long("password") || options.has_short('D'))
        }
        // `adduser` asks for a password, unless it is told not to.
        "adduser" => options.has_long("disabled-password") || options.has_long("disabled-login"),
        _ => false,
    };
    found.then(|| Hit::new(Level::Dangerous))
}

fn account_no_password(inv: &Invocation<'_>, _env: &Env<'_>) -> Option<Hit> {
    if inv.name != "passwd" {
        return None;
    }
    let options = inv.options();
    (options.has_short('d') || options.has_long("delete")).then(|| Hit::new(Level::Dangerous))
}

fn account_admin_group(inv: &Invocation<'_>, _env: &Env<'_>) -> Option<Hit> {
    let options = inv.options();
    let group = match inv.name.as_str() {
        "usermod" => inv
            .option_value('G', "groups")
            .and_then(|groups| groups.split(',').find(|g| ADMIN_GROUPS.contains(g)))?
            .to_owned(),
        // `gpasswd -a USER GROUP`: the group is the last word.
        "gpasswd" if options.has_short('a') || options.has_short('M') => {
            options.operands.last()?.text.clone()
        }
        // `adduser USER GROUP` adds an existing user to a group.
        "adduser" if options.operands.len() == 2 => options.operands[1].text.clone(),
        _ => return None,
    };
    ADMIN_GROUPS.contains(&group.as_str()).then(|| {
        let mut hit = Hit::new(Level::Dangerous);
        hit.target = Some(group);
        hit
    })
}

const SHELLS: &[&str] = &["sh", "bash", "dash", "zsh", "ksh", "ash"];
const DOWNLOADERS: &[&str] = &["curl", "wget"];

fn download_to_shell(stages: &[Option<Invocation<'_>>]) -> Option<Hit> {
    let first = stages.iter().position(|stage| {
        stage
            .as_ref()
            .is_some_and(|inv| DOWNLOADERS.contains(&inv.name.as_str()))
    })?;
    stages[first + 1..]
        .iter()
        .flatten()
        .any(|inv| SHELLS.contains(&inv.name.as_str()) && !inv.options().has_short('c'))
        .then(|| Hit::new(Level::Dangerous))
}

fn download_eval(inv: &Invocation<'_>, _env: &Env<'_>) -> Option<Hit> {
    let name = inv.name.as_str();
    let runs_text = name == "eval" || (SHELLS.contains(&name) && inv.options().has_short('c'));
    if !runs_text {
        return None;
    }
    inv.args
        .iter()
        .flat_map(|arg| arg.subs.iter())
        .any(|list| list_runs(list, DOWNLOADERS))
        .then(|| Hit::new(Level::Dangerous))
}

// ---------------------------------------------------------------------------------------
// git.

/// The subcommand of a `git` command and the words after it.
fn git_subcommand<'i, 'a>(inv: &'i Invocation<'a>) -> Option<(&'i str, &'i [Arg<'a>])> {
    if inv.name != "git" {
        return None;
    }
    let mut at = 0;
    while let Some(arg) = inv.args.get(at) {
        if arg.unknown {
            return None;
        }
        let text = arg.text.as_str();
        if matches!(
            text,
            "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace"
        ) {
            at += 2;
        } else if text.starts_with('-') {
            at += 1;
        } else {
            return Some((text, &inv.args[at + 1..]));
        }
    }
    None
}

/// True if one of the words is a group of short options with `letter` in it.
fn has_short(words: &[Arg<'_>], letter: char) -> bool {
    words.iter().any(|arg| {
        !arg.unknown
            && arg.text.starts_with('-')
            && !arg.text.starts_with("--")
            && arg.text.contains(letter)
    })
}

fn has_word(words: &[Arg<'_>], text: &str) -> bool {
    words.iter().any(|arg| !arg.unknown && arg.text == text)
}

fn git_force_push(inv: &Invocation<'_>, _env: &Env<'_>) -> Option<Hit> {
    let (sub, rest) = git_subcommand(inv)?;
    if sub != "push" {
        return None;
    }
    let mut level: Option<Level> = None;
    for arg in rest.iter().filter(|arg| !arg.unknown) {
        let text = arg.text.as_str();
        let this = if text == "--force" || (text.starts_with('+') && text.len() > 1) {
            Level::Dangerous
        } else if text.starts_with("--force-with-lease") || text == "--force-if-includes" {
            Level::Caution
        } else {
            continue;
        };
        level = level.max(Some(this));
    }
    if has_short(rest, 'f') {
        level = level.max(Some(Level::Dangerous));
    }
    level.map(Hit::new)
}

fn git_reset_hard(inv: &Invocation<'_>, _env: &Env<'_>) -> Option<Hit> {
    let (sub, rest) = git_subcommand(inv)?;
    (sub == "reset" && has_word(rest, "--hard")).then(|| Hit::new(Level::Dangerous))
}

fn git_clean(inv: &Invocation<'_>, _env: &Env<'_>) -> Option<Hit> {
    let (sub, rest) = git_subcommand(inv)?;
    if sub != "clean" {
        return None;
    }
    if has_short(rest, 'n') || has_word(rest, "--dry-run") {
        return None;
    }
    if !(has_short(rest, 'f') || has_word(rest, "--force")) {
        return None;
    }
    let level = if has_short(rest, 'x') || has_short(rest, 'X') {
        Level::Dangerous
    } else {
        Level::Caution
    };
    Some(Hit::new(level))
}

// ---------------------------------------------------------------------------------------
// Overwriting files.

fn overwrite_file(op: RedirectOp, target: &Arg<'_>, env: &Env<'_>) -> Option<Hit> {
    if !matches!(
        op,
        RedirectOp::Output | RedirectOp::Clobber | RedirectOp::OutputAndError
    ) {
        return None;
    }
    if target.unknown || target.glob || target.text.is_empty() {
        return None;
    }
    let path = if target.text.starts_with('/') {
        target.text.clone()
    } else {
        paths::resolve(env.cwd?, &target.text)
    };
    env.probe
        .is_nonempty_file(Path::new(&path))
        .then(|| Hit::on(Level::Caution, target))
}
