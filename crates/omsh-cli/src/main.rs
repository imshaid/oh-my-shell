//! The `oh-my-shell` binary: argument parsing, mode selection and wiring of the
//! other crates.
//!
//! For now every run starts the line-by-line shell on standard input. Choosing between
//! that and handing the work to `/bin/sh` (`-c`, scripts, a pipe) comes in S11.

mod repl;
mod report;

use std::process::ExitCode;

use clap::Parser;

/// An interactive Linux shell in early development. It runs simple commands with the usual
/// expansions (variables, `$(...)`, arithmetic, globbing), but has no pipes or redirections yet.
#[derive(Debug, Parser)]
#[command(name = "oh-my-shell", version)]
struct Cli {}

fn main() -> ExitCode {
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
