//! The `oh-my-shell` program: argument parsing, mode selection and wiring of the
//! other crates.
//!
//! The binary (`main.rs`) is one line that calls [`main`]. The code lives in a library so
//! that the end-to-end tests (`omsh-tests`) can build a copy of the shell that runs
//! exactly the same entry function, which Cargo cannot do across packages for a binary.
//!
//! For now every run starts the line-by-line shell on standard input. Choosing between
//! that and handing the work to `/bin/sh` (`-c`, scripts, a pipe) comes in S11.

mod repl;
mod report;

use std::process::ExitCode;

use clap::Parser;

/// An interactive Linux shell in early development. It runs the POSIX shell language
/// (pipelines, redirections, `&&` and `||`, `if`, loops, `case`, functions, the usual
/// expansions) with a few builtins. Each command runs in its own process group, so Ctrl+C
/// stops the command and not the shell. A command that ends with `&` runs in the background
/// (`jobs`, `wait` and `kill %n` work with it). Ctrl+Z stops the foreground job; `fg` and `bg`
/// continue it, and `disown` forgets it.
#[derive(Debug, Parser)]
#[command(name = "oh-my-shell", version)]
struct Cli {}

/// The entry point of the shell. Every executable that is the shell calls this and nothing
/// else.
pub fn main() -> ExitCode {
    // clap handles `--help`, `--version` and invalid arguments itself: it prints the
    // output and exits the process, so this function only continues for a normal run.
    Cli::parse();

    ExitCode::from(repl::run().code())
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::Cli;

    /// Catches mistakes in the argument definitions, such as conflicting flags.
    #[test]
    fn argument_definitions_are_valid() {
        Cli::command().debug_assert();
    }
}
