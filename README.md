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
in the background, and `jobs`, `wait` and `kill %1` work with it. Ctrl+Z stops the foreground
job and keeps it in the job table; `fg` and `bg` continue it, `disown` forgets it, and `exit` warns
once about stopped jobs.
How it is started decides what runs: `oh-my-shell -c 'cmd'`, `oh-my-shell script.sh` and input
from a pipe are handed to `/bin/sh` at once (so `ssh host cmd` and `scp` work with it as a
login shell), and only a terminal (or `-i`) gets the shell's own interpreter. `-l`, or a name that
starts with `-`, makes a login shell that imports the environment from `/etc/profile` and
`~/.profile` through `/bin/sh -l`. `--no-ai` is accepted and does nothing yet.
The interactive shell reads `~/.config/oh-my-shell/config.toml` (created, with every setting listed
and commented out, the first time it starts) and then runs `~/.config/oh-my-shell/init.omsh`, a file of
commands in the shell's own language for aliases, exports and functions. The `XDG_*` variables are
respected. A mistake in `config.toml` never stops the shell: it is shown with its line and column, and
the setting keeps its default. `--no-config` skips both files and writes nothing; it is the way back in
when `init.omsh` is broken (an `exit` in it ends the shell).
Do not set it as your login shell yet.

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
