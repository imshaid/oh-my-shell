//! The desktop notification after a long command (§15.3, S28).
//!
//! Three small parts:
//!
//! - [`decide`] is a pure function: given the typed line, how long it ran and how it ended, it
//!   says whether a notification is due and what it says. Every rule of `[notify]` lives here.
//! - [`plan`] looks at the shell's variables and says whether a notification can be shown at all
//!   (a desktop session, a session bus, the `notify-send` program) and with which environment.
//! - [`Notifier`] runs `notify-send` on a helper thread, so the prompt never waits for it. The
//!   process gets a group of its own, a deadline of [`DEADLINE`], and is killed and collected if
//!   it does not finish, as the prompt's `git` is (S15b). When a notification cannot be shown the
//!   person is told once, as a toast or, with the plain reader, as a line before the next prompt.
//!
//! Nothing here uses the network: `notify-send` talks to the desktop's notification service over
//! the local session bus.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use omsh_config::Notify;
use omsh_editor::ToastHandle;
use omsh_exec::trigger;
use omsh_syntax::{HighlightKind, lex_for_highlight};

/// How long `notify-send` may take before it is killed.
pub const DEADLINE: Duration = Duration::from_secs(2);

/// How long a notifier that is dropped waits for its helper thread.
const EXIT_WAIT: Duration = Duration::from_millis(100);

/// The longest line shown in the body of a notification, in characters.
const BODY_CHARS: usize = 120;

/// Exit status of a command that was ended with Ctrl+C: 128 + `SIGINT`.
const INTERRUPTED: u8 = 130;
/// Exit status of a command that was stopped with Ctrl+Z: 128 + `SIGTSTP`.
const STOPPED: u8 = 148;

/// Programs that run another program given as an argument: the argument counts as a command
/// name too (`sudo vim`, `time less`).
const WRAPPERS: &[&str] = &[
    "sudo", "doas", "env", "command", "time", "nice", "nohup", "exec", "timeout", "xargs",
    "ionice", "setsid", "stdbuf", "builtin",
];

/// The variables of the shell that the notification needs, in the order they are read.
pub const VARIABLES: &[&str] = &[
    "PATH",
    "HOME",
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "DBUS_SESSION_BUS_ADDRESS",
    "XDG_RUNTIME_DIR",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "SSH_CONNECTION",
    "SSH_TTY",
];

/// What a notification says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    pub title: String,
    pub body: String,
}

/// Whether a notification is due for `line`, which ran for `took` and ended with `status`.
///
/// None for: notifications turned off; a command shorter than `min_seconds`; a command ended with
/// Ctrl+C or stopped with Ctrl+Z; a `?` request; an empty line; and a line in which any command
/// name is on the `ignore` list. `aliases` are `(name, text)` pairs: an alias is looked through,
/// so `alias e=vim` makes `e file` an ignored line too.
#[must_use]
pub fn decide(
    line: &str,
    took: Duration,
    status: u8,
    notify: &Notify,
    aliases: &[(String, String)],
    unicode: bool,
) -> Option<Note> {
    if !notify.enabled
        || took < Duration::from_secs(u64::from(notify.min_seconds))
        || status == INTERRUPTED
        || status == STOPPED
        || line.trim().is_empty()
        || trigger(line).is_some()
    {
        return None;
    }
    let names = command_names(line, aliases, 0);
    if names
        .iter()
        .any(|name| notify.ignore.iter().any(|ignored| ignored == name))
    {
        return None;
    }
    let time = took_text(took);
    let title = match (status, unicode) {
        (0, true) => format!("✓ Done in {time}"),
        (0, false) => format!("Done in {time}"),
        (_, true) => format!("✗ Failed (status {status}) after {time}"),
        (_, false) => format!("Failed (status {status}) after {time}"),
    };
    Some(Note {
        title,
        body: body_text(line),
    })
}

/// `12s`, `1m 05s`, `2h 03m`: whole seconds, rounded down.
fn took_text(took: Duration) -> String {
    let total = took.as_secs();
    let (hours, minutes, seconds) = (total / 3600, total % 3600 / 60, total % 60);
    if hours > 0 {
        format!("{hours}h {minutes:02}m")
    } else if minutes > 0 {
        format!("{minutes}m {seconds:02}s")
    } else {
        format!("{seconds}s")
    }
}

/// The typed line as the body of a notification: secrets masked, control characters and runs of
/// blanks made one blank, cut to [`BODY_CHARS`].
fn body_text(line: &str) -> String {
    let masked = omsh_secrets::mask(line);
    let mut out = String::new();
    let mut blank = true;
    for c in masked.chars() {
        if c.is_control() || c.is_whitespace() {
            if !blank {
                out.push(' ');
                blank = true;
            }
        } else {
            out.push(c);
            blank = false;
        }
    }
    let out = out.trim_end();
    if out.chars().count() > BODY_CHARS {
        let mut cut: String = out.chars().take(BODY_CHARS - 3).collect();
        cut.push_str("...");
        cut
    } else {
        out.to_owned()
    }
}

/// The file names of the programs a line runs: its command words, the words after a wrapper such
/// as `sudo`, and what an alias stands for. Words that are not plain text (`$cmd`, a command
/// substitution) have no name.
fn command_names(line: &str, aliases: &[(String, String)], depth: usize) -> Vec<String> {
    let mut names = Vec::new();
    let highlighted = lex_for_highlight(line);
    let mut after_wrapper = false;
    for word in &highlighted.words {
        let is_command = word.kind == HighlightKind::Command;
        if !is_command && !(after_wrapper && word.kind == HighlightKind::Argument) {
            continue;
        }
        let Some(name) = plain_name(word.span.text(line)) else {
            after_wrapper = false;
            continue;
        };
        if is_command {
            after_wrapper = WRAPPERS.contains(&name.as_str());
            // An alias stands for other words; they are looked at too (a few levels deep, so
            // that `alias ls=ls` cannot loop).
            if depth < 4
                && let Some((_, text)) = aliases.iter().find(|(alias, _)| *alias == name)
            {
                names.extend(command_names(text, aliases, depth + 1));
            }
        }
        names.push(name);
    }
    names
}

/// The file name of the program a word names, if the word is plain text: quotes are taken off,
/// a directory part is dropped. A word with an expansion in it names nothing known.
fn plain_name(word: &str) -> Option<String> {
    if word.contains(['$', '`', '*', '?', '[', '{', '~']) {
        return None;
    }
    let bare: String = word
        .chars()
        .filter(|c| !matches!(c, '\'' | '"' | '\\'))
        .collect();
    let name = bare.rsplit('/').next().unwrap_or("");
    (!name.is_empty()).then(|| name.to_owned())
}

/// What the notification needs to be shown: the program and the environment to give it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Delivery {
    pub program: PathBuf,
    pub env: Vec<(OsString, OsString)>,
}

/// Why a notification cannot be shown. The text finishes "Desktop notification not shown: ...".
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unavailable {
    Ssh,
    NoDisplay,
    NoBus,
    NoProgram,
}

impl Unavailable {
    #[must_use]
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Ssh => "this is an ssh session",
            Self::NoDisplay => "no desktop session (DISPLAY is not set)",
            Self::NoBus => "no desktop session bus",
            Self::NoProgram => "notify-send was not found (package libnotify-bin)",
        }
    }
}

fn get<'a>(vars: &'a [(OsString, OsString)], name: &str) -> Option<&'a OsStr> {
    vars.iter()
        .find(|(key, value)| key == name && !value.is_empty())
        .map(|(_, value)| value.as_os_str())
}

/// Works out whether a notification can be shown with the shell's variables `vars`. Looks at
/// the file system for the session bus and the program, so it is not free; it runs when a
/// notification is due, not at every prompt.
///
/// # Errors
///
/// The first thing that is missing.
pub fn plan(vars: &[(OsString, OsString)]) -> Result<Delivery, Unavailable> {
    if get(vars, "SSH_CONNECTION").is_some() || get(vars, "SSH_TTY").is_some() {
        return Err(Unavailable::Ssh);
    }
    if get(vars, "DISPLAY").is_none() && get(vars, "WAYLAND_DISPLAY").is_none() {
        return Err(Unavailable::NoDisplay);
    }
    let bus_in_runtime_dir =
        get(vars, "XDG_RUNTIME_DIR").is_some_and(|dir| Path::new(dir).join("bus").exists());
    if get(vars, "DBUS_SESSION_BUS_ADDRESS").is_none() && !bus_in_runtime_dir {
        return Err(Unavailable::NoBus);
    }
    let path =
        get(vars, "PATH").map_or(b"/usr/local/bin:/usr/bin:/bin".as_slice(), |p| p.as_bytes());
    let program = path
        .split(|&b| b == b':')
        .filter(|dir| dir.first() == Some(&b'/'))
        .map(|dir| Path::new(OsStr::from_bytes(dir)).join("notify-send"))
        .find(|candidate| {
            std::fs::metadata(candidate)
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        })
        .ok_or(Unavailable::NoProgram)?;
    let env = vars
        .iter()
        .filter(|(name, value)| {
            !value.is_empty()
                && VARIABLES.iter().any(|allowed| OsStr::new(allowed) == name)
                // The program does not need to know about ssh.
                && !matches!(name.to_str(), Some("SSH_CONNECTION" | "SSH_TTY"))
        })
        .cloned()
        .collect();
    Ok(Delivery { program, env })
}

impl Delivery {
    /// Runs the program and waits for it, at most `limit`. It is killed with its group when the
    /// time is up or `cancelled` says so, and always collected.
    ///
    /// # Errors
    ///
    /// One short sentence saying what went wrong.
    pub fn run(
        &self,
        note: &Note,
        limit: Duration,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<(), String> {
        let mut command = Command::new(&self.program);
        command
            .env_clear()
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .arg("--app-name=Oh My Shell")
            .arg("--icon=utilities-terminal")
            .arg("--")
            .arg(&note.title)
            .arg(&note.body)
            .current_dir("/")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            // A group of its own: Ctrl+C at the prompt does not reach it, and it is killed with
            // whatever it started.
            .process_group(0);
        let mut child = spawn_patiently(&mut command, cancelled)?;
        let deadline = Instant::now() + limit;
        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => return Ok(()),
                Ok(Some(status)) => {
                    return Err(match status.code() {
                        Some(code) => format!("notify-send ended with status {code}"),
                        None => "notify-send was ended by a signal".to_owned(),
                    });
                }
                Ok(None) if cancelled() => {
                    kill(&mut child);
                    return Err("cancelled".to_owned());
                }
                Ok(None) if Instant::now() >= deadline => {
                    kill(&mut child);
                    return Err("notify-send did not answer in time".to_owned());
                }
                Ok(None) => thread::sleep(Duration::from_millis(5)),
                Err(error) => {
                    kill(&mut child);
                    return Err(format!("notify-send could not be waited for ({error})"));
                }
            }
        }
    }
}

/// How many times a program that is "busy" is tried again, and how long between tries.
const BUSY_TRIES: u32 = 20;
const BUSY_PAUSE: Duration = Duration::from_millis(10);

/// Starts the program. `exec` fails with "text file busy" (`ETXTBSY`) while any process still
/// holds the file open for writing: a package manager replacing `notify-send`, or, in a program
/// with several threads, a `fork` on another thread that copied the open handle and has not
/// reached its own `exec` yet. Both end within milliseconds, so this tries again a few times
/// (at most `BUSY_TRIES` x `BUSY_PAUSE`, 200 ms) before it gives up.
fn spawn_patiently(command: &mut Command, cancelled: &dyn Fn() -> bool) -> Result<Child, String> {
    let mut tries = 0;
    loop {
        match command.spawn() {
            Ok(child) => return Ok(child),
            Err(error)
                if error.kind() == std::io::ErrorKind::ExecutableFileBusy
                    && tries < BUSY_TRIES
                    && !cancelled() =>
            {
                tries += 1;
                thread::sleep(BUSY_PAUSE);
            }
            Err(error) => return Err(format!("notify-send could not be started ({error})")),
        }
    }
}

/// Ends the program and whatever it started, and collects it, so that no zombie is left.
fn kill(child: &mut Child) {
    if let Ok(pid) = i32::try_from(child.id()) {
        let _ = killpg(Pid::from_raw(pid), Signal::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[derive(Default)]
struct Shared {
    /// The notifier is being dropped: end the program now.
    cancelled: AtomicBool,
    /// A helper thread is working: a second notification is skipped, not queued.
    busy: AtomicBool,
    /// The person has been told once that notifications cannot be shown.
    warned: AtomicBool,
    /// The message for a shell with no toast, waiting for the next prompt.
    stash: Mutex<Option<String>>,
}

/// Shows notifications without ever making the prompt wait.
pub struct Notifier {
    shared: Arc<Shared>,
    toast: Option<ToastHandle>,
    worker: Mutex<Option<JoinHandle<()>>>,
    limit: Duration,
}

impl Notifier {
    /// `toast` is where the one-time "not shown" message goes; without one it waits in
    /// [`take_message`](Self::take_message).
    #[must_use]
    pub fn new(toast: Option<ToastHandle>) -> Self {
        Self::with_limit(toast, DEADLINE)
    }

    #[must_use]
    pub fn with_limit(toast: Option<ToastHandle>, limit: Duration) -> Self {
        Self {
            shared: Arc::new(Shared::default()),
            toast,
            worker: Mutex::new(None),
            limit,
        }
    }

    /// Shows `note` with the shell's variables `vars`, on a helper thread. Returns at once. If it
    /// cannot be shown the person hears of it once; a notification that arrives while another is
    /// still being shown is dropped.
    pub fn send(&self, note: Note, vars: &[(OsString, OsString)]) {
        let delivery = match plan(vars) {
            Ok(delivery) => delivery,
            Err(why) => {
                self.fail(why.reason());
                return;
            }
        };
        if self.shared.busy.swap(true, Ordering::AcqRel) {
            return;
        }
        let shared = Arc::clone(&self.shared);
        let toast = self.toast.clone();
        let limit = self.limit;
        let spawned = thread::Builder::new()
            .name("notify".to_owned())
            .spawn(move || {
                let cancelled = || shared.cancelled.load(Ordering::Relaxed);
                let result = delivery.run(&note, limit, &cancelled);
                shared.busy.store(false, Ordering::Release);
                if let Err(reason) = result
                    && !cancelled()
                {
                    warn_once(&shared, toast.as_ref(), &reason);
                }
            });
        match spawned {
            Ok(handle) => {
                let mut worker = self.worker.lock().unwrap_or_else(PoisonError::into_inner);
                // The previous thread has finished its work (`busy` was clear); this reaps it.
                if let Some(old) = worker.replace(handle) {
                    let _ = old.join();
                }
            }
            Err(_) => self.shared.busy.store(false, Ordering::Release),
        }
    }

    fn fail(&self, reason: &str) {
        warn_once(&self.shared, self.toast.as_ref(), reason);
    }

    /// The message waiting for a shell that has no toast, once.
    #[must_use]
    pub fn take_message(&self) -> Option<String> {
        self.shared
            .stash
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }
}

/// Tells the person once, whatever the reason.
fn warn_once(shared: &Shared, toast: Option<&ToastHandle>, reason: &str) {
    if shared.warned.swap(true, Ordering::AcqRel) {
        return;
    }
    let message = format!("Desktop notification not shown: {reason}");
    match toast {
        Some(toast) => toast.post(&message),
        None => {
            *shared.stash.lock().unwrap_or_else(PoisonError::into_inner) = Some(message);
        }
    }
}

impl Drop for Notifier {
    /// Ends a notification that is still being shown, and waits a moment for its thread.
    fn drop(&mut self) {
        self.shared.cancelled.store(true, Ordering::Relaxed);
        let handle = self
            .worker
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(handle) = handle {
            let until = Instant::now() + EXIT_WAIT;
            while !handle.is_finished() && Instant::now() < until {
                thread::sleep(Duration::from_millis(2));
            }
            if handle.is_finished() {
                let _ = handle.join();
            }
        }
    }
}

#[cfg(test)]
mod tests;
