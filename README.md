# Oh My Shell

An interactive Linux shell, written in Rust. The goal is a shell that behaves like a
familiar POSIX shell, helps like fish, and can optionally hand a task to an AI, always
showing what will happen before anything runs.

**Status: early development.** The `oh-my-shell` binary reads commands from standard input
and runs simple ones: a program name, arguments and `NAME=value` prefixes, separated by `;`
or line endings. Words get the usual POSIX expansions: `~`, `$var` and `${var:-x}`, `$(...)`,
`$((1+2))`, field splitting and globbing. Pipes, redirections, `&&`, job control and the rest
of the shell language are not there yet, and a line that needs them is refused whole. Do not
set it as your login shell.

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
