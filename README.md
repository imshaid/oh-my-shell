# Oh My Shell

An interactive Linux shell, written in Rust. The goal is a shell that behaves like a
familiar POSIX shell, helps like fish, and can optionally hand a task to an AI, always
showing what will happen before anything runs.

**Status: early development.** The `oh-my-shell` binary reads commands from standard input
and runs it: pipelines, redirections (including here-documents), `;` `&&` `||` `!`, `( )` and
`{ }`, `if`, `while`, `until`, `for`, `case` and functions. Words get the usual POSIX
expansions: `~`, `$var` and `${var:-x}`, `$(...)`, `$((1+2))`, field splitting and globbing.
The first builtins are there (`cd`, `pwd`, `export`, `unset`, `set --`, `shift`, `echo`,
`eval`, `.`, `exit`, `return`, `break`, `continue`). In a terminal, each command runs in its own
process group, so Ctrl+C stops the command and not the shell. A command that ends with `&` runs
in the background, and `jobs`, `wait` and `kill %1` work with it. `fg`/`bg` and stopped foreground
jobs (Ctrl+Z) are not there yet.
Do not set it as your login shell.

## Build and run

You need Linux and a recent stable Rust toolchain ([rustup](https://rustup.rs)).

```sh
cargo build
cargo run -p omsh-cli -- --version
```

## Check your changes

CI runs these checks on every push:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

## License

[MIT](LICENSE)
