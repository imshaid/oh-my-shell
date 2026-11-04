# Oh My Shell

An interactive Linux shell, written in Rust. The goal is a shell that behaves like a
familiar POSIX shell, helps like fish, and can optionally hand a task to an AI, always
showing what will happen before anything runs.

**Status: early development.** Only the project skeleton exists today. The
`oh-my-shell` binary builds and answers `--version` and `--help`, but it cannot run
commands yet, so do not set it as your login shell.

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
