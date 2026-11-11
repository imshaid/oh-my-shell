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
# Name of a theme: a built-in one, or themes/<name>.toml next to this file. (not used yet)
#theme = "nord"
# "full", "reduced" or "off". When unset: "reduced" over ssh, "full" otherwise. (not used yet)
# motion = "full"
# "unicode", "ascii" or "auto" (unicode when the locale is UTF-8). Used by error reports.
#icons = "auto"

#[editor]
# "emacs" or "vi". (not used yet)
#mode = "emacs"
# Suggest the rest of a command from history as you type. (not used yet)
#hints = true

# Abbreviations: a word followed by a space becomes its text. (not used yet)
#[editor.abbreviations]
# gco = "git checkout"
# gst = "git status"

#[prompt]
# Segments of the prompt, in order. The segments are: cwd, git, venv, status, duration, time.
# (not used yet)
#left = ["cwd", "git", "venv"]
#right = ["status", "duration"]
# Collapse old prompts to a short one. (not used yet)
#transient = false

#[history]
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
# Days a file stays in the trash before it is purged. (not used yet)
#trash_retention_days = 8
# Limits of the walk that counts what a command would touch. (not used yet)
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
# "auto" (the language of the request), "en" or "bn". (not used yet)
#language = "auto"

# What may be sent along with a request. Each item can be turned off. (not used yet)
#[ai.context]
#os = true
#cwd = true
#git = true
#recent_commands = true

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
