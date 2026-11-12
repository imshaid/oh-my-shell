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
use std::sync::Arc;
use std::time::{Duration, Instant};

use omsh_config::{Config, Paths};
use omsh_editor::{
    CommandLookup, Editor, Facts, PromptState, Read as Typed, Recall, fish_completion_directories,
    passed_variables, suggest_command, working_directory,
};
use omsh_exec::{
    CommandSuggest, DEFAULT_PATH, ExitStatus, HistoryCommand, Io, JournalCommand, JumpCommand,
    LineResult, OmshCommand, Outcome, Shell, Trigger, builtin_names, find_program, hung_up,
    is_builtin, is_root, note_inherited_signals, release_terminal, start_interactive,
    terminal_gone, trigger, wait_for_input,
};
use omsh_history::{Filter, Store};
use omsh_ui::Theme;

use crate::ai;
use crate::confirm;
use crate::history::History;
use crate::radius::BlastLimits;
use crate::report::{self, Style};
use crate::undo::{self, Undo};

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
    /// `--no-ai`: the AI layer is off for this session. It wins over `[ai] enabled`.
    pub no_ai: bool,
    /// `--no-config`: read neither `config.toml` nor `init.omsh`, and write nothing (history
    /// is then kept in memory).
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
    /// `Alt+?`: explain the last failure (S24). The typed text stays in the editor.
    ExplainLast,
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
    let Started {
        config,
        history,
        ai,
        undo,
    } = match start_up(&mut shell, &mut files, options) {
        Ok(started) => started,
        // `exit` in the startup file.
        Err(status) => return finish(&mut shell, status, &files, None),
    };
    // After the startup file: only the lines a person types get a "did you mean".
    if interactive {
        shell.set_suggest(Arc::new(Suggestions));
    }
    let history = history.as_ref();
    if let Some(history) = history {
        shell.set_history(Arc::new(history.clone()) as Arc<dyn HistoryCommand>);
        shell.set_jump(Arc::new(history.clone()) as Arc<dyn JumpCommand>);
    }
    let style = Style::detect(config.ui.icons);
    let mut editor = history
        .and_then(|history| make_editor(&shell, &config, style, interactive && terminals, history));
    // The lines read so far for the command being typed: more than one when it is unfinished.
    let mut pending = String::new();
    // How long the last command took, for the prompt.
    let mut took: Option<Duration> = None;
    // The last commands typed here, for the AI context (S22). Lines the history ignores are
    // left out of it too.
    let mut recent = ai::recent::Recent::new(
        history
            .map(|history| history.filter.clone())
            .unwrap_or_default(),
    );
    loop {
        if interactive {
            if hung_up() {
                return finish(&mut shell, ExitStatus(HANGUP_STATUS), &files, history);
            }
            if pending.is_empty() {
                // What happened to the background jobs since the last prompt.
                shell.job_notices(&files);
                // Nothing waits for the writer here: a line typed ahead while the prompt is
                // late can be lost (see `omsh-history`).
                if let Some(problem) = history.and_then(History::problem) {
                    say(&format!("oh-my-shell: {problem}"));
                }
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
                    return finish(&mut shell, status, &files, history);
                }
                true
            }
            Next::Interrupted => {
                // `$?` stays as it was, as with the plain reader (the terminal throws the
                // half-typed line away there).
                pending.clear();
                continue;
            }
            Next::ExplainLast => {
                // Not a line: nothing is stored in history, `$?` stays, and what was typed
                // stays in the editor.
                explain_last_key(ai.as_deref(), &mut shell, &mut files, &mut recent);
                continue;
            }
            Next::HungUp => {
                return finish(&mut shell, ExitStatus(HANGUP_STATUS), &files, history);
            }
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

        // Noted when it starts, so that a command the shell does not come back from (`exit`,
        // a crash) is in the history too; the status and the duration follow when it ends.
        let noted = history.and_then(|history| {
            let token = history.begin(&pending, &directory(&shell))?;
            Some((history, token))
        });
        let started = Instant::now();
        let folder = history.map(|_| directory(&shell));
        // A typed line is looked at by the safety analyzer first (S16). Other input (`-c`
        // scripts, a pipe) is run as written: the person is not there to answer.
        let result = if interactive {
            // A request to the AI is natural language, so it is recognized before aliases and
            // the parser see the line (§4.2 step 2).
            match (trigger(&pending), ai.as_ref()) {
                (Some(trigger), Some(ai)) => ai_line(
                    trigger,
                    ai,
                    &mut shell,
                    &mut files,
                    history,
                    &mut recent,
                    &config,
                    style,
                    &mut editor,
                    undo.as_deref(),
                ),
                _ => confirm::run_line(
                    &mut shell,
                    &pending,
                    at_end,
                    &mut files,
                    config.safety.confirm,
                    BlastLimits::from_config(&config.safety),
                    undo.as_deref(),
                ),
            }
        } else {
            shell.run_source(&pending, at_end, &mut files)
        };
        if let Some((history, token)) = noted {
            match &result {
                // Nothing ran: the rest of the command is still to come.
                LineResult::Incomplete => history.store.cancel(token),
                LineResult::Ran(Outcome::Exit(status)) => {
                    history
                        .store
                        .finish(token, i32::from(status.0), started.elapsed());
                }
                LineResult::Ran(Outcome::Continue(_)) | LineResult::Rejected(_) => {
                    let status = i32::from(shell.last_status().0);
                    history.store.finish(token, status, started.elapsed());
                }
            }
        }
        // A line that changed the folder (`cd`, `j`, a sourced file): `j` remembers the new one.
        if let (Some(history), Some(before)) = (history, folder) {
            let after = directory(&shell);
            if after != before {
                history.store.visit(&after);
            }
        }
        // A typed line that ended (not a `?` request: its steps are noted when they run).
        if interactive
            && trigger(&pending).is_none()
            && matches!(
                result,
                LineResult::Ran(Outcome::Continue(_)) | LineResult::Rejected(_)
            )
        {
            recent.record(&pending, i32::from(shell.last_status().0));
        }
        match result {
            LineResult::Incomplete => {}
            LineResult::Ran(Outcome::Continue(_)) => {
                pending.clear();
                took = Some(started.elapsed());
            }
            LineResult::Ran(Outcome::Exit(status)) => {
                return finish(&mut shell, status, &files, history);
            }
            LineResult::Rejected(report) => {
                report::show(&report, style);
                pending.clear();
                took = Some(started.elapsed());
            }
        }
        if at_end {
            let status = shell.last_status();
            return finish(&mut shell, status, &files, history);
        }
    }
}

/// `Alt+?` in the editor: why did the last command fail? Prints below the prompt line; the
/// editor draws the prompt again afterwards with the text that was being typed.
fn explain_last_key(
    ai: Option<&ai::AiSession>,
    shell: &mut Shell,
    files: &mut Io,
    recent: &mut ai::recent::Recent,
) {
    let Some(ai) = ai else { return };
    // The cursor is on the line being typed: move below it.
    say("");
    let env = ai::explain::ExplainEnv {
        shell,
        files,
        tty: &mut ai::plan::RealTerminal,
        recent,
    };
    let _ = ai::explain::last(|| ai.plan_setup(), false, env);
}

/// Handles a line that starts with `?` (or ends with `| ?`): a plan (`? request`), an
/// explanation (`?? text`, `??`) or a question about output (`cmd | ? question`).
#[expect(
    clippy::too_many_arguments,
    reason = "the pieces of the shell the flow needs"
)]
fn ai_line(
    trigger: Trigger<'_>,
    ai: &ai::AiSession,
    shell: &mut Shell,
    files: &mut Io,
    history: Option<&History>,
    recent: &mut ai::recent::Recent,
    config: &Config,
    style: Style,
    editor: &mut Option<Editor>,
    undo: Option<&Undo>,
) -> LineResult {
    let fail = |shell: &mut Shell, status: ExitStatus| {
        shell.set_last_status(status);
        LineResult::Ran(Outcome::Continue(status))
    };
    let request = match trigger {
        Trigger::Plan(request) => request,
        Trigger::Explain(text) => {
            let mut env = ai::explain::ExplainEnv {
                shell,
                files,
                tty: &mut ai::plan::RealTerminal,
                recent,
            };
            if text.is_empty() {
                return ai::explain::last(|| ai.plan_setup(), true, env);
            }
            return match ai.plan_setup() {
                Ok(setup) => ai::explain::command(setup, text, env),
                Err(why) => ai::explain::unavailable(&mut env, "??", &why),
            };
        }
        Trigger::Pipe {
            command, question, ..
        } => {
            let env = ai::explain::ExplainEnv {
                shell,
                files,
                tty: &mut ai::plan::RealTerminal,
                recent,
            };
            return ai::explain::pipe(
                ai.plan_setup(),
                command,
                question,
                ai::explain::PipeEnv {
                    confirm: config.safety.confirm,
                    blast: BlastLimits::from_config(&config.safety),
                    undo,
                },
                env,
            );
        }
    };
    // `? history ...` searches the local history; the model only writes the filter (S25).
    if let Some(search) = ai::history_search::request_of(request) {
        return ai::history_search::run(
            ai.plan_setup(),
            search,
            chrono::Local::now(),
            ai::history_search::SearchEnv {
                shell,
                files,
                tty: &mut ai::plan::RealTerminal,
                history,
            },
        );
    }
    if request.is_empty() {
        // Before anything about the AI layer: `?` alone only needs to be explained.
        say(ai::plan::USAGE.trim_end());
        return fail(shell, ExitStatus::USAGE);
    }
    let setup = match ai.plan_setup() {
        Ok(setup) => setup,
        Err(why) => {
            say(&format!("oh-my-shell: ?: {why}"));
            return fail(shell, ExitStatus(1));
        }
    };
    let mut edit = |text: &str| match editor {
        Some(editor) => match editor.edit(text) {
            Typed::Line(text) => ai::plan::Edited::Text(text),
            _ => ai::plan::Edited::Cancelled,
        },
        None => ai::plan::Edited::Unavailable,
    };
    ai::plan::run(
        setup,
        request,
        ai::plan::PlanEnv {
            shell,
            files,
            tty: &mut ai::plan::RealTerminal,
            history,
            recent,
            confirm: config.safety.confirm,
            blast: BlastLimits::from_config(&config.safety),
            unicode: matches!(style, Style::Graphical { unicode: true, .. }),
            undo,
            edit: &mut edit,
        },
    )
}

/// Whether to use the line editor: a terminal to read from and to draw on, a `TERM` that is
/// set and not `dumb`, and not inside Emacs (whose shell mode cannot show escape codes; bash
/// turns readline off there too).
fn should_edit(terminals: bool, term: Option<&[u8]>, inside_emacs: Option<&[u8]>) -> bool {
    terminals
        && term.is_some_and(|term| !term.is_empty() && term != b"dumb")
        && inside_emacs.is_none_or(<[u8]>::is_empty)
}

/// The shell's working directory as history keeps it: text. A name that is not UTF-8 is
/// stored with replacement characters.
pub(crate) fn directory(shell: &Shell) -> String {
    working_directory(shell.var("PWD"))
        .to_string_lossy()
        .into_owned()
}

/// How long the terminal has to answer the cursor question. Real terminals answer in well under
/// a millisecond, and a slow link in tens of milliseconds; the limit is long enough that a
/// satellite link is not mistaken for a silent terminal.
const CURSOR_ANSWER_LIMIT: Duration = Duration::from_millis(500);

fn make_editor(
    shell: &Shell,
    config: &Config,
    style: Style,
    terminals: bool,
    history: &History,
) -> Option<Editor> {
    if !should_edit(terminals, shell.var("TERM"), shell.var("INSIDE_EMACS")) {
        return None;
    }
    let Style::Graphical { color, unicode } = style else {
        return None;
    };
    // The editor asks the terminal where the cursor is at every prompt, and waits 2 seconds
    // for each answer it does not get. Ask once, here: a terminal that stays silent gets the
    // plain reader, and the person is told.
    if !omsh_editor::terminal_reports_cursor(CURSOR_ANSWER_LIMIT) {
        eprintln!(
            "oh-my-shell: the terminal does not report the cursor position; line editing is off"
        );
        return None;
    }
    Some(Editor::new(
        config,
        &Theme::default(),
        unicode,
        color,
        Recall {
            store: history.store.clone(),
            filter: history.filter.clone(),
        },
        hung_up,
        || terminal_gone(io::stdin()),
    ))
}

/// The "did you mean" line after `not found`: names close to the one typed, from the editor
/// crate, which already knows the programs in `PATH`.
struct Suggestions;

impl CommandSuggest for Suggestions {
    fn suggest(&self, name: &str, path: &[u8], shell_names: &[String]) -> Vec<String> {
        suggest_command(name, path, shell_names)
    }
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

/// What Tab completion offers, as the shell is when a prompt is drawn: its functions, aliases,
/// variables and builtins, `PATH`, and where fish's completion files would be.
fn completion_facts(shell: &Shell) -> Facts {
    let text = |name: &str| {
        shell
            .var(name)
            .and_then(|value| std::str::from_utf8(value).ok())
    };
    let home = shell
        .var("HOME")
        .filter(|value| !value.is_empty())
        .map(|value| PathBuf::from(OsStr::from_bytes(value)));
    Facts {
        functions: shell.function_names(),
        aliases: shell.alias_names(),
        builtins: builtin_names().into_iter().map(str::to_owned).collect(),
        variables: shell.variable_names(),
        path: shell
            .var("PATH")
            .map_or_else(|| DEFAULT_PATH.as_bytes().to_vec(), <[u8]>::to_vec),
        fish_dirs: fish_completion_directories(
            home.as_deref(),
            text("XDG_CONFIG_HOME"),
            text("XDG_DATA_HOME"),
            text("XDG_DATA_DIRS"),
        ),
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
        // What `git` is given, taken from the shell's variables like everything else here.
        git_env: passed_variables()
            .iter()
            .filter_map(|name| {
                let value = shell.var(name)?;
                Some((OsString::from(name), OsString::from_vec(value.to_vec())))
            })
            .collect(),
    }
}

fn read_edited(editor: &mut Editor, shell: &Shell, pending: &str, took: Option<Duration>) -> Next {
    let typed = if pending.is_empty() {
        editor.read(
            &prompt_state(shell, took),
            Box::new(ShellCommands::of(shell)),
            completion_facts(shell),
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
        Typed::ExplainLast => Next::ExplainLast,
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

/// What an interactive start-up produces.
struct Started {
    config: Config,
    history: Option<History>,
    /// The `omsh` builtin's session, which `? request` also uses.
    ai: Option<Arc<ai::AiSession>>,
    /// The trash and `undo` (S18). None under `--no-config` and without a data directory.
    undo: Option<Arc<Undo>>,
}

/// How many lines of problems with `config.toml` are shown before "... and N more".
const MAX_CONFIG_LINES: usize = 10;

/// Reads `config.toml` (writing the commented first-run file if there is none) and runs
/// `init.omsh`, for a shell that a person types at. Nothing here stops the shell: whatever is
/// wrong is shown as a line each and the defaults stay. `Err` carries the status of an `exit`
/// in `init.omsh`.
///
/// It also opens the history of such a shell: in `history.db` under the data directory, or in
/// memory under `--no-config` (which writes nothing) and when there is no data directory.
///
/// `-c`, scripts and piped input never get here (they are handed to `/bin/sh` first), and
/// neither does `--interpret-stdin`, which is not interactive.
fn start_up(shell: &mut Shell, files: &mut Io, options: Options) -> Result<Started, ExitStatus> {
    if !options.interactive {
        return Ok(Started {
            config: Config::default(),
            history: None,
            ai: None,
            undo: None,
        });
    }
    // Built in, reads no file: it also runs under `--no-config`. Before `init.omsh`, so
    // that the file can replace or remove the wrappers.
    shell.define_color_wrappers(files);
    // The shell's own variables, which include what the login profile exported.
    let paths = Paths::from_lookup(|name| {
        shell
            .var(name)
            .map(|value| OsString::from_vec(value.to_vec()))
    });
    if options.no_config {
        let config = Config::default();
        let ai = install_omsh(shell, &config, &paths, options);
        let history = History {
            store: Store::in_memory(),
            filter: Filter::new(&config.history.ignore),
        };
        return Ok(Started {
            config,
            history: Some(history),
            ai: Some(ai),
            undo: None,
        });
    }
    let loaded = omsh_config::load(&paths);
    for line in loaded.report(MAX_CONFIG_LINES) {
        say(&format!("oh-my-shell: {line}"));
    }
    if let Some(init) = paths.init_file()
        && let Outcome::Exit(status) = shell.run_startup_file(&init, files)
    {
        return Err(status);
    }
    let config = loaded.config;
    let ai = install_omsh(shell, &config, &paths, options);
    // After `init.omsh`: what the startup file runs is not typed by anyone and is not
    // history. The file is opened by the store's own thread, so the prompt does not wait.
    let store = match paths.data_dir() {
        Some(dir) => Store::open(&dir.join(HISTORY_FILE), config.history.size),
        None => Store::in_memory(),
    };
    let history = History {
        store,
        filter: Filter::new(&config.history.ignore),
    };
    // The trash and `undo`. Nothing is opened here: the journal is opened by the first use.
    // Old entries are emptied first, with one line before anything is deleted (§12.4).
    let undo = paths.data_dir().map(|dir| {
        undo::purge_old(dir, config.safety.trash_retention_days, &mut |notice| {
            say(notice.trim_end());
        });
        let undo = Undo::new(dir.to_path_buf());
        shell.set_journal(Arc::clone(&undo) as Arc<dyn JournalCommand>);
        undo
    });
    Ok(Started {
        config,
        history: Some(history),
        ai: Some(ai),
        undo,
    })
}

/// Gives the shell its `omsh` builtin (`omsh ai setup` and friends, S20). This only stores the
/// settings: the keyring, the TLS setup and the runtime are created when a person types an
/// `omsh ai` command, never here.
fn install_omsh(
    shell: &mut Shell,
    config: &Config,
    paths: &Paths,
    options: Options,
) -> Arc<ai::AiSession> {
    let env = ai::Environment::system(paths, options.no_config);
    let session = Arc::new(ai::AiSession::new(config, options.no_ai, env));
    shell.set_omsh(Arc::clone(&session) as Arc<dyn OmshCommand>);
    session
}

/// The name of the history database in the data directory.
const HISTORY_FILE: &str = "history.db";

/// The status of a shell that was hung up: 128 + `SIGHUP`.
const HANGUP_STATUS: u8 = 129;

/// The shell ends: its `EXIT` trap runs, then its jobs are dealt with (stopped ones are told to
/// hang up, since no one could continue them any more). Last, the history gets a moment to
/// write what is still queued.
fn finish(
    shell: &mut Shell,
    status: ExitStatus,
    files: &Io,
    history: Option<&History>,
) -> ExitStatus {
    let status = shell.run_exit_trap(status, files);
    shell.end_session();
    if let Some(history) = history {
        history.close();
    }
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
