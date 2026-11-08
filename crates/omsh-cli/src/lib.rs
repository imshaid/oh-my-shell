//! The `oh-my-shell` program: argument parsing, mode selection and wiring of the
//! other crates.
//!
//! The binary (`main.rs`) is one line that calls [`main`]. The code lives in a library so
//! that the end-to-end tests (`omsh-tests`) can build a copy of the shell that runs
//! exactly the same entry function, which Cargo cannot do across packages for a binary.
//!
//! What the process does depends on how it was started (blueprint §8):
//!
//! | Started as | What happens |
//! |---|---|
//! | `-c command [name args...]` | replaced by `/bin/sh -c ...` before anything else |
//! | `script [args...]` | replaced by `/bin/sh script ...` |
//! | no operands, input is not a terminal | replaced by `/bin/sh` reading the input |
//! | no operands, input is a terminal, or `-i` | this shell's own interactive loop |
//! | `-l`, or `argv[0]` starts with `-` | a login shell: the environment is imported first |
//!
//! The interactive shell reads `~/.config/oh-my-shell/config.toml` (created with comments on
//! the first run) and runs `init.omsh` before the first prompt; `--no-config` skips both.
//!
//! At a terminal that takes escape codes (`TERM` set and not `dumb`, not inside Emacs, stdin and
//! stderr both terminals) lines come from the editor of `omsh-editor`; otherwise from the plain
//! line reader in `repl.rs`.
//!
//! `--interpret-stdin` is a hidden switch for the tests: it runs this shell's interpreter on
//! piped input, which would otherwise go to `/bin/sh`.

mod handoff;
mod repl;
mod report;

use std::ffi::OsString;
use std::io::{self, IsTerminal, Write};
use std::os::unix::ffi::OsStrExt;
use std::process::ExitCode;

use clap::Parser;

/// An interactive Linux shell in early development. It runs the POSIX shell language
/// (pipelines, redirections, `&&` and `||`, `if`, loops, `case`, functions, the usual
/// expansions) with a few builtins. Each command runs in its own process group, so Ctrl+C
/// stops the command and not the shell. A command that ends with `&` runs in the background
/// (`jobs`, `wait` and `kill %n` work with it). Ctrl+Z stops the foreground job; `fg` and `bg`
/// continue it, and `disown` forgets it.
///
/// `-c`, scripts and input from a pipe are run by `/bin/sh` itself, so tools like ssh and scp
/// can use this as a login shell.
///
/// The interactive shell reads ~/.config/oh-my-shell/config.toml (created with comments on the
/// first run) and runs init.omsh from the same directory before the first prompt.
#[derive(Debug, Default, Parser)]
#[command(name = "oh-my-shell", version)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "these are the command line switches"
)]
struct Cli {
    /// Run the first operand as a command string, with `$0` and the rest as its arguments.
    #[arg(short = 'c')]
    command: bool,
    /// Act as a login shell: import the login environment first.
    #[arg(short = 'l', long)]
    login: bool,
    /// Force interactive mode.
    #[arg(short = 'i')]
    interactive: bool,
    /// Turn off the AI layer for this session.
    #[arg(long)]
    no_ai: bool,
    /// Read neither `config.toml` nor `init.omsh`, and write nothing. The way back in when a
    /// startup file is broken.
    #[arg(long)]
    no_config: bool,
    /// Run this shell's own interpreter on standard input instead of `/bin/sh`.
    #[arg(long, hide = true)]
    interpret_stdin: bool,
    /// A script file and its arguments, or (with `-c`) the command and its arguments.
    #[arg(value_name = "ARG", trailing_var_arg = true)]
    operands: Vec<OsString>,
}

/// What to do, decided from the command line alone.
#[derive(Debug, PartialEq, Eq)]
enum Mode {
    /// Replace the process with `/bin/sh` run with these arguments.
    Sh(Vec<OsString>),
    /// Run the read-run loop of this shell.
    Repl(repl::Options),
    /// `-c` without a command.
    MissingCommand,
}

/// Picks the mode. `started_as_login` is true when the program name starts with `-`;
/// `stdin_is_terminal` says whether standard input is a terminal.
fn choose(cli: &Cli, started_as_login: bool, stdin_is_terminal: bool) -> Mode {
    let login = cli.login || started_as_login;
    let operands = cli.operands.clone();
    if cli.command {
        return if operands.is_empty() {
            Mode::MissingCommand
        } else {
            Mode::Sh(handoff::command_args(login, operands))
        };
    }
    if !operands.is_empty() {
        return Mode::Sh(handoff::script_args(login, operands));
    }
    let own_shell = |interactive| {
        Mode::Repl(repl::Options {
            interactive,
            login,
            no_ai: cli.no_ai,
            no_config: cli.no_config,
        })
    };
    if cli.interpret_stdin {
        return own_shell(false);
    }
    if cli.interactive || stdin_is_terminal {
        return own_shell(true);
    }
    Mode::Sh(handoff::stdin_args(login))
}

/// The entry point of the shell. Every executable that is the shell calls this and nothing
/// else.
pub fn main() -> ExitCode {
    // The program name: a leading `-` marks a login shell (how `login`, `sshd` and `su -`
    // start it). Read before anything else is done.
    let name = std::env::args_os().next();
    let started_as_login = name
        .as_deref()
        .is_some_and(|n| n.as_bytes().first() == Some(&b'-'));

    // clap handles `--help`, `--version` and invalid arguments itself: it prints the
    // output and exits the process, so this function only continues for a normal run.
    let cli = Cli::parse();

    match choose(&cli, started_as_login, io::stdin().is_terminal()) {
        Mode::Sh(args) => handoff::exec_sh(&args),
        Mode::MissingCommand => {
            let _ = writeln!(io::stderr(), "oh-my-shell: -c: option requires an argument");
            ExitCode::from(2)
        }
        Mode::Repl(options) => {
            // `$0` of a login shell is the name it was started under, like in bash.
            let login_name = if started_as_login { name } else { None };
            ExitCode::from(repl::run(options, login_name.as_deref()).code())
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

    use super::*;

    /// Catches mistakes in the argument definitions, such as conflicting flags.
    #[test]
    fn argument_definitions_are_valid() {
        Cli::command().debug_assert();
    }

    fn os(text: &str) -> OsString {
        OsString::from(text)
    }

    fn mode(args: &[&str], started_as_login: bool, tty: bool) -> Mode {
        let cli = Cli::try_parse_from(std::iter::once("oh-my-shell").chain(args.iter().copied()))
            .expect("the arguments parse");
        choose(&cli, started_as_login, tty)
    }

    fn own_shell(interactive: bool, login: bool, no_ai: bool) -> Mode {
        Mode::Repl(repl::Options {
            interactive,
            login,
            no_ai,
            no_config: false,
        })
    }

    #[test]
    fn dash_c_goes_to_sh_whatever_the_input_is() {
        let expected = Mode::Sh(vec![os("-c"), os("echo hi"), os("zero"), os("one")]);
        assert_eq!(
            mode(&["-c", "echo hi", "zero", "one"], false, false),
            expected
        );
        assert_eq!(
            mode(&["-c", "echo hi", "zero", "one"], false, true),
            expected
        );
    }

    #[test]
    fn dash_c_without_a_command_is_an_error() {
        assert_eq!(mode(&["-c"], false, false), Mode::MissingCommand);
    }

    #[test]
    fn a_hyphen_after_the_command_belongs_to_sh() {
        assert_eq!(
            mode(&["-c", "echo $0", "-x", "a"], false, false),
            Mode::Sh(vec![os("-c"), os("echo $0"), os("-x"), os("a")])
        );
    }

    #[test]
    fn combined_flags_work() {
        let expected = Mode::Sh(vec![os("-l"), os("-c"), os("true")]);
        assert_eq!(mode(&["-ilc", "true"], false, true), expected);
        assert_eq!(mode(&["-lc", "true"], false, false), expected);
    }

    #[test]
    fn a_script_goes_to_sh_with_its_arguments() {
        assert_eq!(
            mode(&["run.sh", "-v", "x"], false, true),
            Mode::Sh(vec![os("run.sh"), os("-v"), os("x")])
        );
    }

    #[test]
    fn input_that_is_not_a_terminal_goes_to_sh() {
        assert_eq!(mode(&[], false, false), Mode::Sh(vec![]));
    }

    #[test]
    fn a_terminal_gets_the_interactive_shell() {
        assert_eq!(mode(&[], false, true), own_shell(true, false, false));
    }

    #[test]
    fn dash_i_forces_the_interactive_shell_without_a_terminal() {
        assert_eq!(mode(&["-i"], false, false), own_shell(true, false, false));
    }

    #[test]
    fn the_hidden_switch_runs_our_interpreter_on_piped_input() {
        assert_eq!(
            mode(&["--interpret-stdin"], false, false),
            own_shell(false, false, false)
        );
    }

    #[test]
    fn a_login_name_is_passed_to_sh_and_to_the_interactive_shell() {
        assert_eq!(
            mode(&["-c", "true"], true, false),
            Mode::Sh(vec![os("-l"), os("-c"), os("true")])
        );
        assert_eq!(mode(&[], true, false), Mode::Sh(vec![os("-l")]));
        assert_eq!(mode(&[], true, true), own_shell(true, true, false));
        assert_eq!(
            mode(&["--login"], false, true),
            own_shell(true, true, false)
        );
    }

    #[test]
    fn no_ai_is_remembered_for_the_interactive_shell() {
        assert_eq!(
            mode(&["--no-ai"], false, true),
            own_shell(true, false, true)
        );
    }

    #[test]
    fn no_config_is_remembered_for_the_interactive_shell() {
        assert_eq!(
            mode(&["--no-config"], false, true),
            Mode::Repl(repl::Options {
                interactive: true,
                login: false,
                no_ai: false,
                no_config: true,
            })
        );
        // It changes nothing about what goes to `/bin/sh`.
        assert_eq!(
            mode(&["--no-config", "-c", "true"], false, false),
            Mode::Sh(vec![os("-c"), os("true")])
        );
    }

    #[test]
    fn dash_i_does_not_reach_sh() {
        assert_eq!(
            mode(&["-i", "-c", "true"], false, false),
            Mode::Sh(vec![os("-c"), os("true")])
        );
    }
}
