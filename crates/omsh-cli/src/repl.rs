//! The read-run loop.
//!
//! Lines come from one of two readers. When a person types at a terminal that takes escape
//! codes, the line editor of `omsh-editor` reads them (S13a). Otherwise (a dumb terminal, Emacs'
//! shell mode, `-i` on a pipe, a test that feeds a pipe) the plain reader takes a line from
//! standard input, one byte at a time, with no editing.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, ErrorKind, IsTerminal, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use omsh_config::{Config, Paths};
use omsh_editor::{CommandLookup, Editor, PromptState, Read as Typed, working_directory};
use omsh_exec::{
    DEFAULT_PATH, ExitStatus, Io, LineResult, Outcome, Shell, find_program, hung_up, is_builtin,
    is_root, note_inherited_signals, release_terminal, start_interactive, terminal_gone,
    wait_for_input,
};
use omsh_ui::Theme;

use crate::report::{self, Style};

const PROMPT: &str = "$ ";
const CONTINUATION_PROMPT: &str = "> ";

/// How the shell was asked to run (see `choose` in `lib.rs`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    /// A person is typing the commands: prompts, notices, Ctrl+C survival. Set for a
    /// terminal and by `-i`.
    pub interactive: bool,
    /// A login shell: import the login environment before the first prompt.
    pub login: bool,
    /// `--no-ai`. The AI layer does not exist yet (S20), so nothing reads this.
    pub no_ai: bool,
    /// `--no-config`: read neither `config.toml` nor `init.omsh`, and write nothing.
    pub no_config: bool,
}

/// Runs the shell on standard input until it ends, and returns the exit status. `name` is the
/// name the shell was started under, when `$0` should show it (a login shell: `-oh-my-shell`).
pub fn run(options: Options, name: Option<&OsStr>) -> ExitStatus {
    // Before anything changes a signal setting: what was ignored at the start stays ignored.
    note_inherited_signals();
    let interactive = options.interactive;
    // Job control needs a real terminal; `-i` on a pipe still prompts but has none.
    let job_control = if interactive && io::stdin().is_terminal() {
        match start_interactive() {
            Ok(()) => true,
            Err(reason) => {
                say(&format!(
                    "oh-my-shell: no job control in this shell: {reason}"
                ));
                false
            }
        }
    } else {
        false
    };
    let input = match stdin_without_buffer() {
        Ok(file) => file,
        Err(error) => {
            say(&format!("oh-my-shell: cannot read standard input: {error}"));
            release_terminal();
            return ExitStatus(1);
        }
    };
    // The editor needs a terminal to read from and one to draw on.
    let terminals = io::stdin().is_terminal() && io::stderr().is_terminal();
    let status = run_on(input, options, job_control, terminals, name);
    // Give the terminal back to the program that started the shell.
    release_terminal();
    status
}

/// A second handle on file descriptor 0 that does not buffer.
///
/// `std::io::stdin()` reads ahead in blocks of several kilobytes. If a command such as
/// `head -n 1` then reads from the same input, the lines the shell has already taken are
/// gone. The shell reads one byte at a time instead and takes exactly one line. The new
/// handle shares its position with fd 0 (both name the same open file), so a child that
/// inherits fd 0 continues where the shell stopped.
fn stdin_without_buffer() -> io::Result<File> {
    Ok(File::from(io::stdin().as_fd().try_clone_to_owned()?))
}

/// What the reader brought back.
enum Next {
    /// A line, with its line ending.
    Line(Vec<u8>),
    /// The end of the input (Ctrl+D on an empty line, or the end of a file).
    End,
    /// Ctrl+C in the editor: drop what was typed.
    Interrupted,
    /// The terminal hung up.
    HungUp,
    /// The editor cannot work with this terminal.
    EditorFailed(io::Error),
    /// The plain reader failed.
    ReadFailed(io::Error),
}

fn run_on(
    mut input: impl Read,
    options: Options,
    job_control: bool,
    terminals: bool,
    name: Option<&OsStr>,
) -> ExitStatus {
    let interactive = options.interactive;
    let mut shell = Shell::new();
    shell.set_interactive(interactive);
    shell.set_job_control(job_control);
    if let Some(name) = name {
        shell.set_name(name.as_bytes());
    }
    // The files that commands start with: this process's standard input, output and error.
    let mut files = Io::process();
    if options.login {
        // The profile files are in `sh` syntax, so `/bin/sh` reads them (see `omsh-exec`).
        shell.import_login_environment(&files);
    }
    // After the login environment, so that a profile may set `XDG_CONFIG_HOME`.
    let config = match start_up(&mut shell, &mut files, options) {
        Ok(config) => config,
        // `exit` in the startup file.
        Err(status) => return finish(&mut shell, status, &files),
    };
    let style = Style::detect(config.ui.icons);
    let mut editor = make_editor(&shell, &config, style, interactive && terminals);
    // The lines read so far for the command being typed: more than one when it is unfinished.
    let mut pending = String::new();
    // How long the last command took, for the prompt.
    let mut took: Option<Duration> = None;
    loop {
        if interactive {
            if hung_up() {
                return finish(&mut shell, ExitStatus(HANGUP_STATUS), &files);
            }
            if pending.is_empty() {
                // What happened to the background jobs since the last prompt.
                shell.job_notices(&files);
            }
        }
        let next = match editor.as_mut() {
            Some(editor) => read_edited(editor, &shell, &pending, took),
            None => read_plain(&mut input, interactive, pending.is_empty()),
        };
        let at_end = match next {
            Next::Line(bytes) => {
                if append_utf8(&mut pending, bytes).is_err() {
                    say("oh-my-shell: the input is not valid UTF-8; the line was skipped");
                    pending.clear();
                    shell.set_last_status(ExitStatus::USAGE);
                    continue;
                }
                false
            }
            Next::End => {
                if pending.is_empty() {
                    // End of input with stopped jobs: the first time only warns.
                    if !shell.confirm_exit(&files) {
                        continue;
                    }
                    let status = shell.last_status();
                    return finish(&mut shell, status, &files);
                }
                true
            }
            Next::Interrupted => {
                // `$?` stays as it was, as with the plain reader (the terminal throws the
                // half-typed line away there).
                pending.clear();
                continue;
            }
            Next::HungUp => return finish(&mut shell, ExitStatus(HANGUP_STATUS), &files),
            Next::EditorFailed(error) => {
                say(&format!(
                    "oh-my-shell: the line editor does not work with this terminal ({error}); \
                     using plain input"
                ));
                editor = None;
                continue;
            }
            Next::ReadFailed(error) => {
                say(&format!("oh-my-shell: cannot read standard input: {error}"));
                return ExitStatus(1);
            }
        };

        let started = Instant::now();
        match shell.run_source(&pending, at_end, &mut files) {
            LineResult::Incomplete => {}
            LineResult::Ran(Outcome::Continue(_)) => {
                pending.clear();
                took = Some(started.elapsed());
            }
            LineResult::Ran(Outcome::Exit(status)) => return finish(&mut shell, status, &files),
            LineResult::Rejected(report) => {
                report::show(&report, style);
                pending.clear();
                took = Some(started.elapsed());
            }
        }
        if at_end {
            let status = shell.last_status();
            return finish(&mut shell, status, &files);
        }
    }
}

/// Whether to use the line editor: a terminal to read from and to draw on, a `TERM` that is
/// set and not `dumb`, and not inside Emacs (whose shell mode cannot show escape codes; bash
/// turns readline off there too).
fn should_edit(terminals: bool, term: Option<&[u8]>, inside_emacs: Option<&[u8]>) -> bool {
    terminals
        && term.is_some_and(|term| !term.is_empty() && term != b"dumb")
        && inside_emacs.is_none_or(<[u8]>::is_empty)
}

fn make_editor(shell: &Shell, config: &Config, style: Style, terminals: bool) -> Option<Editor> {
    if !should_edit(terminals, shell.var("TERM"), shell.var("INSIDE_EMACS")) {
        return None;
    }
    let Style::Graphical { color, unicode } = style else {
        return None;
    };
    Some(Editor::new(
        config,
        &Theme::default(),
        unicode,
        color,
        hung_up,
        || terminal_gone(io::stdin()),
    ))
}

/// What the shell can run, as it is when a prompt is drawn. The editor asks it to color command
/// names (red when nothing would run). It is a copy, so the editor's thread needs no access to
/// the shell, and a change made by the next command shows at the next prompt.
struct ShellCommands {
    functions: HashSet<String>,
    aliases: HashSet<String>,
    path: Vec<u8>,
}

impl ShellCommands {
    fn of(shell: &Shell) -> Self {
        Self {
            functions: shell.function_names().into_iter().collect(),
            aliases: shell.alias_names().into_iter().collect(),
            path: shell
                .var("PATH")
                .map_or_else(|| DEFAULT_PATH.as_bytes().to_vec(), <[u8]>::to_vec),
        }
    }
}

impl CommandLookup for ShellCommands {
    fn is_command(&self, name: &str) -> bool {
        self.functions.contains(name)
            || self.aliases.contains(name)
            || is_builtin(name)
            || find_program(name.as_bytes(), &self.path, None)
    }
}

/// Everything the prompt shows, taken from the shell's own variables (not the process
/// environment, which a `cd` or `export` does not change).
fn prompt_state(shell: &Shell, took: Option<Duration>) -> PromptState {
    let path = |name: &str| {
        shell
            .var(name)
            .filter(|value| !value.is_empty())
            .map(|value| PathBuf::from(OsStr::from_bytes(value)))
    };
    PromptState {
        cwd: working_directory(shell.var("PWD")),
        home: path("HOME"),
        virtual_env: path("VIRTUAL_ENV"),
        conda_env: shell
            .var("CONDA_DEFAULT_ENV")
            .and_then(|value| std::str::from_utf8(value).ok())
            .map(str::to_owned),
        last_status: shell.last_status().0,
        last_duration: took,
        root: is_root(),
    }
}

fn read_edited(editor: &mut Editor, shell: &Shell, pending: &str, took: Option<Duration>) -> Next {
    let typed = if pending.is_empty() {
        editor.read(
            &prompt_state(shell, took),
            Box::new(ShellCommands::of(shell)),
        )
    } else {
        editor.read_more()
    };
    match typed {
        Typed::Line(mut text) => {
            text.push('\n');
            Next::Line(text.into_bytes())
        }
        Typed::Interrupted => Next::Interrupted,
        Typed::EndOfInput => Next::End,
        Typed::HungUp => Next::HungUp,
        Typed::Failed(error) => Next::EditorFailed(error),
    }
}

fn read_plain(input: &mut impl Read, interactive: bool, first_line: bool) -> Next {
    if interactive {
        prompt(if first_line {
            PROMPT
        } else {
            CONTINUATION_PROMPT
        });
        // Wait in steps, so that a `SIGHUP` sent to an idle shell is noticed: a plain read
        // would be restarted after the signal and never come back.
        if !wait_for_input(io::stdin().as_fd()) {
            return Next::HungUp;
        }
    }
    match read_line(input) {
        Ok(Some(bytes)) => Next::Line(bytes),
        Ok(None) => Next::End,
        Err(error) => Next::ReadFailed(error),
    }
}

/// How many lines of problems with `config.toml` are shown before "... and N more".
const MAX_CONFIG_LINES: usize = 10;

/// Reads `config.toml` (writing the commented first-run file if there is none) and runs
/// `init.omsh`, for a shell that a person types at. Nothing here stops the shell: whatever is
/// wrong is shown as a line each and the defaults stay. `Err` carries the status of an `exit`
/// in `init.omsh`.
///
/// `-c`, scripts and piped input never get here (they are handed to `/bin/sh` first), and
/// neither does `--interpret-stdin`, which is not interactive.
fn start_up(shell: &mut Shell, files: &mut Io, options: Options) -> Result<Config, ExitStatus> {
    if options.interactive {
        // Built in, reads no file: it also runs under `--no-config`. Before `init.omsh`, so
        // that the file can replace or remove the wrappers.
        shell.define_color_wrappers(files);
    }
    if !options.interactive || options.no_config {
        return Ok(Config::default());
    }
    // The shell's own variables, which include what the login profile exported.
    let paths = Paths::from_lookup(|name| {
        shell
            .var(name)
            .map(|value| OsString::from_vec(value.to_vec()))
    });
    let loaded = omsh_config::load(&paths);
    for line in loaded.report(MAX_CONFIG_LINES) {
        say(&format!("oh-my-shell: {line}"));
    }
    if let Some(init) = paths.init_file()
        && let Outcome::Exit(status) = shell.run_startup_file(&init, files)
    {
        return Err(status);
    }
    Ok(loaded.config)
}

/// The status of a shell that was hung up: 128 + `SIGHUP`.
const HANGUP_STATUS: u8 = 129;

/// The shell ends: its `EXIT` trap runs, then its jobs are dealt with (stopped ones are told to
/// hang up, since no one could continue them any more).
fn finish(shell: &mut Shell, status: ExitStatus, files: &Io) -> ExitStatus {
    let status = shell.run_exit_trap(status, files);
    shell.end_session();
    status
}

/// Adds `bytes` to `text` if they are valid UTF-8. The parser works on `str`.
fn append_utf8(text: &mut String, bytes: Vec<u8>) -> Result<(), std::string::FromUtf8Error> {
    text.push_str(&String::from_utf8(bytes)?);
    Ok(())
}

/// Reads up to and including the next line ending. Returns nothing at the end of the input
/// if no byte was read.
fn read_line(input: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    let mut byte = [0_u8; 1];
    loop {
        match input.read(&mut byte) {
            Ok(0) => return Ok((!line.is_empty()).then_some(line)),
            Ok(_) => {
                line.push(byte[0]);
                if byte[0] == b'\n' {
                    return Ok(Some(line));
                }
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

/// Writes the prompt to stderr, so that it does not mix with a command's output.
fn prompt(text: &str) {
    let mut err = io::stderr();
    let _ = write!(err, "{text}");
    let _ = err.flush();
}

/// Writes one line to stderr. A failed write is ignored: there is nowhere left to say so.
fn say(text: &str) {
    let _ = writeln!(io::stderr(), "{text}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_editor_needs_terminals_and_a_capable_term() {
        let xterm = Some(&b"xterm-256color"[..]);
        assert!(should_edit(true, xterm, None));
        assert!(!should_edit(false, xterm, None), "no terminal");
        assert!(!should_edit(true, None, None), "TERM unset");
        assert!(!should_edit(true, Some(b""), None), "TERM empty");
        assert!(!should_edit(true, Some(b"dumb"), None), "TERM=dumb");
    }

    #[test]
    fn the_editor_is_off_inside_emacs() {
        let xterm = Some(&b"xterm"[..]);
        assert!(!should_edit(true, xterm, Some(b"29.1,comint")));
        assert!(!should_edit(true, xterm, Some(b"t")));
        assert!(should_edit(true, xterm, Some(b"")), "empty means unset");
    }

    #[test]
    fn a_line_ends_after_its_line_ending() {
        let mut input: &[u8] = b"one\ntwo\n";
        assert_eq!(read_line(&mut input).unwrap(), Some(b"one\n".to_vec()));
        assert_eq!(read_line(&mut input).unwrap(), Some(b"two\n".to_vec()));
        assert_eq!(read_line(&mut input).unwrap(), None);
    }

    #[test]
    fn the_last_line_may_have_no_line_ending() {
        let mut input: &[u8] = b"one\ntwo";
        read_line(&mut input).unwrap();
        assert_eq!(read_line(&mut input).unwrap(), Some(b"two".to_vec()));
        assert_eq!(read_line(&mut input).unwrap(), None);
    }

    #[test]
    fn nothing_beyond_the_line_is_consumed() {
        let mut input: &[u8] = b"one\ntwo\n";
        read_line(&mut input).unwrap();
        assert_eq!(input, b"two\n");
    }

    #[test]
    fn bytes_that_are_not_utf8_are_refused() {
        let mut text = String::new();
        assert!(append_utf8(&mut text, vec![0xff, b'\n']).is_err());
        assert!(text.is_empty());
    }
}
