# Oh My Shell

An interactive Linux shell, written in Rust. It behaves like a familiar POSIX shell, helps
like fish while you type, keeps a searchable history, and can optionally hand a task to an AI.
Nothing the AI suggests runs without a preview and your confirmation, and dangerous commands
are checked locally first.

**Status: pre-1.0.** The release pipeline exists (below), but no release has been published yet:
until one is, build from source. Do not set it as your login shell yet.

## Install

Linux, x86_64 or aarch64, with glibc 2.35 or newer (Ubuntu 22.04+, Debian 12+, Fedora 36+).

**Install script.** Puts `oh-my-shell` and the short name `omsh` in `~/.local/bin`. It checks the
download against the release's `sha256.sum`, runs the new binary once, and changes nothing else:
no `chsh`, no edits to your shell's startup files, and `/etc/shells` only with `--add-to-shells`.

```sh
curl --proto '=https' --tlsv1.2 -fsSL \
  https://github.com/imshaid/oh-my-shell/releases/latest/download/install.sh | sh
```

Options (`sh -s -- --help` lists them): `--version X.Y.Z`, `--system` (`/usr/local/bin`),
`--bin-dir DIR`, `--add-to-shells`.

**Debian / Ubuntu package.** Download `oh-my-shell_<version>-1_amd64.deb` (or `_arm64.deb`) from the
[releases page](https://github.com/imshaid/oh-my-shell/releases) and run
`sudo apt install ./oh-my-shell_*.deb`. It installs `/usr/bin/oh-my-shell` and `/usr/bin/omsh`,
lists the shell in `/etc/shells`, and depends on `ca-certificates` (HTTPS to an AI provider).

**From source.** Needs a recent stable Rust toolchain:

```sh
cargo install --locked --git https://github.com/imshaid/oh-my-shell omsh-cli
```

(The program is called `oh-my-shell`; the package is `omsh-cli`. It is not on crates.io.)

**Using it.** Start with the command of your terminal's profile (GNOME Terminal, Kitty, VS Code
and others can run any program), or just type `oh-my-shell`. Use it for a while before you run
`chsh`; the installer never does. `omsh` is a short name for the same program: alone it starts
the shell, and `omsh ai setup`, `omsh ai status` and `omsh tour` work from any other shell too.

## What it does

**The shell.** Pipelines, redirections (here-documents included), `;` `&&` `||` `!`, `( )`,
`{ }`, `if`, `while`, `until`, `for`, `case`, functions, aliases, the usual expansions
(`~`, `$var`, `${var:-x}`, `$(...)`, `$((1+2))`, field splitting, globbing), job control
(`&`, `jobs`, `fg`, `bg`, `wait`, `kill %1`, Ctrl+Z) and the common builtins. It is a
POSIX-compatible subset, checked against `dash` and `bash`; syntax it does not support is
reported with a clear message. Run as `oh-my-shell -c 'cmd'`, with a script, or with input from
a pipe, it hands the work to `/bin/sh`, so `ssh host cmd` and `scp` work. Only a terminal (or
`-i`) gets the interactive interpreter.

**While you type.**
- Syntax colors like fish (a command that does not exist is red, quotes are yellow, existing
  files are underlined). Only the 16 ANSI colors are used, so your terminal theme decides how
  they look. `NO_COLOR` is respected.
- Gray hints from your history, ranked by the current folder first (Right or End accepts).
- Tab completion for commands, paths, variables, git branches and options.
- Abbreviations, bracketed paste, emacs or vi keys (`editor.mode = "vi"`).
- A two-line prompt with the folder, git branch (read in the background, never slowing the
  prompt), virtual environment, status and duration.

**History.** Kept in a local SQLite file, with the folder, status and duration of every command.
Secrets (keys, tokens, passwords in commands) are never stored. `history` lists and searches it,
`j words` jumps to a folder you have used, and `Ctrl+R` opens a full-screen explorer.

**Full-screen views.** `Ctrl+R` history explorer, `Ctrl+P` command palette (actions, aliases,
functions, builtins), and a settings view that changes switches in `config.toml`.

**Safety.** Every typed line is analyzed before it runs. A dangerous one (`rm -rf` on a big
tree, for example) shows what it would touch and asks first. `rm` can
move files to a trash instead of deleting them, and `undo` brings them back.

**AI, optional and off until you set it up** (`omsh ai setup`; Ollama, Anthropic, OpenAI,
Gemini, or any OpenAI-compatible server):
- `? list the biggest files here` asks for a plan, shows each step with its risk, and runs it
  only after you confirm. Steps run exactly as written: your aliases are not applied.
- `?? tar -xzf a.tgz` explains a command without running it; `Alt+?` explains why the last
  command failed; `cmd | ? question` asks about its output; `? history ...` searches the history
  in plain words.
- No network code runs on the normal command path. `--no-ai` switches the AI off for a session.
  Keys are kept in the system keyring, or a private file, and are never sent to an address that
  is not `https://` or this computer. Secrets are masked before anything is sent. Answers and
  plans are in English.

**Also.** Desktop notifications for long commands (`notify-send`), a first-run welcome and
`omsh tour`, and themes (`~/.config/oh-my-shell/themes/<name>.toml`).

## Configuration

On the first start the shell writes `~/.config/oh-my-shell/config.toml` with every setting
listed and commented out, and then runs `~/.config/oh-my-shell/init.omsh` (aliases, exports and
functions, in the shell's own language). The `XDG_*` variables are respected. A mistake in the
config never stops the shell: it is shown with its line and column and the setting keeps its
default. `--no-config` skips both files and writes nothing; use it when `init.omsh` is broken.

## Build and run

You need Linux and a recent stable Rust toolchain ([rustup](https://rustup.rs)).

```sh
cargo build --release
target/release/oh-my-shell --version
cargo run -p omsh-cli          # the interactive shell
```

The desktop notification needs `notify-send`; hints and the prompt's git segment need `git` in
`PATH`. Neither is required.

## Check your changes

CI runs these checks on every push, plus a performance gate (start-up time, idle memory,
keystroke latency), `cargo deny`, and the test suite again with the network switched off:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

## License

[MIT](LICENSE)
