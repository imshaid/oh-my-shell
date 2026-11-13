//! The commented `config.toml` that is written the first time the shell starts.

use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, ErrorKind, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};

/// The first-run file. Every setting is shown with its default, commented out.
///
/// Two kinds of commented lines: `#key = value` (no space after the `#`) is a setting at its
/// default, and a test removes the `#` of all of them to check that the file means exactly
/// `Config::default()`. `# key = value` (with a space) is an example for a setting that is
/// unset by default.
pub const TEMPLATE: &str = r##"# Oh My Shell configuration
#
# This file was created the first time the shell started. Every setting is listed with
# its default value, commented out. To change one, remove the "#" in front of it.
#
# A mistake in this file never stops the shell: the setting with the mistake is reported
# with its line and column, and its default is used. The rest of the file still applies.
#
# Commands to run at startup (aliases, exports, functions) go in "init.omsh", next to this
# file. It is written in the shell's own language and is not created for you. Start the
# shell with --no-config to skip both files.
#
# Some settings are checked now, but the part of the shell that uses them comes in a later
# version. They are marked "not used yet".

#[ui]
# Name of a theme: "nord" (built in), or themes/<name>.toml next to this file. A theme file
# lists the colors you want to change and nothing else; the rest stays as in "nord":
#   [tokens]                      # accent, path, ai, success, warning, danger, muted, dim
#   danger = "magenta"            # one of the 16 ANSI color names (black, red, ... bright-white)
#   [roles]                       # the colors of the command line: command, error, quote, ...
#   quote = { color = "green", underline = true }
# Colors are names, never hex values, so they follow your terminal's own colors.
#theme = "nord"
# "full", "reduced" or "off": how much of the waiting line moves. When unset: "reduced" over
# ssh, "full" otherwise. Always off on a "dumb" terminal and in a locale that is not UTF-8.
# motion = "full"
# "unicode", "ascii" or "auto" (unicode when the locale is UTF-8). Used by error reports,
# panels and the waiting line.
#icons = "auto"

#[editor]
# "emacs" or "vi".
#mode = "emacs"
# Suggest the rest of a command from history as you type, in gray. Press Right or End to
# take all of it, Alt+F to take one word. Commands run in the current directory come first.
#hints = true

# Abbreviations: a word that stands where a command name goes, followed by a space (or Enter),
# becomes its text. The history keeps the long form. Not expanded in arguments or in quotes.
#[editor.abbreviations]
# gco = "git checkout"
# gst = "git status"

#[prompt]
# Segments of the prompt, in order. The segments are: cwd, git, venv, status, duration, time.
#left = ["cwd", "git", "venv"]
#right = ["status", "duration"]
# Collapse old prompts to a short one.
#transient = false

#[history]
# The shell also remembers the directories you change into: `j words` jumps to the best match
# (`j proj`, `j me proj src`), `j -` goes back.
# How many commands to keep. Older ones are removed when a shell starts.
#size = 100000
# Lines that match one of these patterns are not stored: `*` stands for any text, `?` for one
# character, and a pattern has to match the whole line (" *" is a line that starts with a
# space). Lines that look like they hold a secret are never stored, whatever this says.
#ignore = [" *"]

#[safety]
# The lowest risk level that asks before a typed line runs: "caution", "dangerous" or
# "critical". Lines below it print a one-line note and run; "critical" lines always ask.
#confirm = "dangerous"
# Days a file stays in the trash before it is purged. A shell empties the older entries when it
# starts, after one line saying so.
#trash_retention_days = 8
# Limits of the walk that counts what a risky command would touch before you confirm it: it
# stops at this many entries or this many milliseconds, whichever comes first, and then says
# "at least".
#blast_radius_max_entries = 50000
#blast_radius_max_ms = 300

#[ai]
# The AI layer is off until you set up a provider. Type `omsh ai setup` at the prompt: it asks
# a few questions, checks them with one small request, and writes provider, model and
# base_url below. API keys are never kept in this file: they go in the system keyring, or in
# a file only you can read when there is no keyring. `--no-ai` turns the layer off for one
# session, whatever this says.
#enabled = true
# "ollama", "anthropic", "openai", "gemini" or "openai-compatible".
# provider = "ollama"
# model = "llama3.2"
# Address of an OpenAI-compatible server, or of an Ollama on another computer. It must not
# contain a user name or password. A key is sent only to https:// addresses and to this
# computer (localhost, 127.0.0.1, ::1).
# base_url = "http://localhost:11434"

# What may be sent along with a request. Each item can be turned off. `os` is the name of the
# system, `cwd` the working directory, `git` whether it is inside a git repository and the
# branch, `recent_commands` the last 3 commands of this shell with their exit status (never
# their output). Secrets are replaced by [MASKED] first. Press `v` in the plan panel to see
# exactly what was sent.
#[ai.context]
#os = true
#cwd = true
#git = true
#recent_commands = true

#[notify]
# A desktop notification when a command that ran a while has ended, so that you can look away
# from the terminal. It is made with the `notify-send` program (package libnotify-bin) and only
# in a desktop session; over ssh, or without that program, nothing is shown and the shell says
# so once. Commands ended with Ctrl+C or stopped with Ctrl+Z, and `?` requests, never notify.
#enabled = true
# A command notifies when it ran at least this many seconds (1 to 86400).
#min_seconds = 10
# Programs that never notify, because a long run is a person working in them. A command line
# is skipped when any of its command names (also after sudo, env, command, time) is listed.
#ignore = ["vi", "vim", "nvim", "nano", "emacs", "micro", "hx", "less", "more", "most", "man", "info", "ssh", "mosh", "top", "htop", "btop", "tmux", "screen", "watch", "sh", "bash", "zsh", "fish", "oh-my-shell"]

#[update]
# Look for a new release when the shell starts. Off, because it needs the network.
# (not used yet)
#check = false
#channel = "stable"
"##;

/// Writes `text` to `file` if there is no such file. Returns whether it was written.
///
/// Two shells that start at the same moment must not overwrite each other or leave half a
/// file for the other to read. So the text goes to a temporary file in the same directory
/// first, and is then given its final name with a hard link, which fails if the name is taken.
/// The directory is made with mode 0700 if it does not exist (the XDG specification).
pub fn create(file: &Path, text: &str) -> io::Result<bool> {
    let dir = file
        .parent()
        .ok_or_else(|| io::Error::new(ErrorKind::InvalidInput, "no directory"))?;
    DirBuilder::new().recursive(true).mode(0o700).create(dir)?;

    static NEXT: AtomicU32 = AtomicU32::new(0);
    let temporary = dir.join(format!(
        ".config.toml.{}.{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let written = (|| {
        let mut out = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(&temporary)?;
        out.write_all(text.as_bytes())?;
        out.sync_all()
    })();
    let result = written.and_then(|()| match fs::hard_link(&temporary, file) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == ErrorKind::AlreadyExists => Ok(false),
        // A file system without hard links: write the file directly. `create_new` still
        // refuses to touch a file that exists.
        Err(_) => match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(file)
        {
            Ok(mut out) => out.write_all(text.as_bytes()).map(|()| true),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => Ok(false),
            Err(error) => Err(error),
        },
    });
    let _ = fs::remove_file(&temporary);
    result
}
