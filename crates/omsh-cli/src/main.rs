//! The `oh-my-shell` binary: argument parsing, mode selection and wiring of the
//! other crates.
//!
//! Only `--version` and `--help` work for now. Running the binary without arguments
//! says so and fails, rather than pretending to be a working shell.

use std::io::{self, Write};
use std::process::ExitCode;

use clap::Parser;

/// An interactive Linux shell in early development: it cannot run commands yet.
#[derive(Debug, Parser)]
#[command(name = "oh-my-shell", version)]
struct Cli {}

fn main() -> ExitCode {
    // clap handles `--help`, `--version` and invalid arguments itself: it prints the
    // output and exits the process, so this function only continues for a normal run.
    Cli::parse();

    // Not `eprintln!`, which panics when stderr cannot be written. There is nowhere
    // left to report such a failure, so the result is deliberately ignored.
    let _ = writeln!(
        io::stderr(),
        "oh-my-shell: the interactive shell is not implemented yet"
    );
    ExitCode::FAILURE
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
