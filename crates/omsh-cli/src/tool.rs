//! The `omsh` program: the shell's own commands (`omsh ai status`, `omsh tour`) run from any
//! shell, one command and then exit (blueprint D1, §18).
//!
//! The installer makes `omsh` a symlink to `oh-my-shell`. The program looks at the name it was
//! started under: `omsh` followed by one of [`TOOLS`] runs that command with the same code as
//! the builtin of the interactive shell, so the two cannot differ. Everything else stays what
//! it was: `omsh` alone is the interactive shell, `omsh script` goes to `/bin/sh` (§8), and
//! `oh-my-shell ai` (no symlink) is still a script name.
//!
//! Nothing here starts the AI layer, the keyring or a runtime by itself; the command does, when
//! it needs them (the S20 rule).

use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::process::ExitCode;

use omsh_config::{Config, Paths};
use omsh_exec::{Io, Shell, catch_interrupt, note_inherited_signals};

use crate::repl::{Options, install_omsh};

/// The first words that make `omsh` a tool and not a shell. `update` and `uninstall` join them
/// in S29b.
const TOOLS: &[&str] = &["ai", "tour", "help"];

/// What `omsh` was asked to run.
#[derive(Debug, PartialEq, Eq)]
pub struct Request {
    /// The command and its arguments (`ai`, `status`).
    pub words: Vec<OsString>,
    /// `--no-ai`.
    pub no_ai: bool,
    /// `--no-config`: read no `config.toml` and write nothing.
    pub no_config: bool,
}

/// Whether `word` names a command of `omsh`.
pub fn is_tool(word: &OsStr) -> bool {
    TOOLS.iter().any(|tool| word.as_bytes() == tool.as_bytes())
}

/// Runs the command and returns its exit status.
pub fn run(request: Request) -> ExitCode {
    // Before anything changes a signal setting, as the shell does. Then Ctrl+C sets a flag that
    // a question reads, so that a hidden key prompt can give the terminal its echo back.
    note_inherited_signals();
    catch_interrupt();
    let mut shell = Shell::new();
    let paths = Paths::from_lookup(|name| {
        shell
            .var(name)
            .map(|value| OsString::from_vec(value.to_vec()))
    });
    let config = if request.no_config {
        Config::default()
    } else {
        let loaded = omsh_config::load(&paths);
        for line in loaded.report(MAX_CONFIG_LINES) {
            say(&format!("omsh: {line}"));
        }
        loaded.config
    };
    let options = Options {
        interactive: false,
        login: false,
        no_ai: request.no_ai,
        no_config: request.no_config,
    };
    let _session = install_omsh(&mut shell, &config, &paths, options);
    let words: Vec<Vec<u8>> = request
        .words
        .into_iter()
        .map(OsStringExt::into_vec)
        .collect();
    let status = shell.run_omsh(&words, &Io::process());
    ExitCode::from(status.0)
}

/// As many config problems as the shell shows at start.
const MAX_CONFIG_LINES: usize = 10;

fn say(text: &str) {
    let _ = writeln!(io::stderr(), "{text}");
}
