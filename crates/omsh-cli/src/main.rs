//! The `oh-my-shell` binary. All of the work is in the library (`lib.rs`).

fn main() -> std::process::ExitCode {
    omsh_cli::main()
}
