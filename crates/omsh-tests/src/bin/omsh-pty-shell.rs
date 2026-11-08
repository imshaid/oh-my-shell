//! The shell as the PTY tests run it: the same entry function as the `oh-my-shell` binary,
//! with no setup of its own. (Cargo only gives a test the path of a binary of its own
//! package, so the tests cannot use `oh-my-shell` itself.)

fn main() -> std::process::ExitCode {
    omsh_cli::main()
}
