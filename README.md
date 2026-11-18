# Oh My Shell

An interactive Linux shell, written in Rust. It behaves like a familiar POSIX shell, helps while you type
(colors, hints, completion), keeps a searchable history, checks dangerous commands before they run, and can
optionally hand a task to an AI. Nothing the AI suggests runs without a preview and your confirmation.

**Status: version 1.1.0.** It is young. Use it from a terminal profile for a while
before you make it your login shell; the installers never run `chsh`.

- **AI is off until you set it up** (`omsh ai setup`). The shell is fully usable without it, and `--no-ai` switches it off.
- **No network access on the normal command path.** Typing, running commands, completion, history and the safety check
  make no connection of their own (a test checks that no network socket is created). One caveat: `~user` expansion asks the
  system's account lookup, and on a machine set up with a network account service (LDAP, sssd) that lookup may use the network. The network is used only when you ask: an AI request, `omsh update`, and an
  optional daily update check that is off by default.
- **No telemetry** of any kind.

## Install

Linux, x86_64 or aarch64, glibc 2.35 or newer (Ubuntu 22.04+, Debian 12+, Fedora 36+).

```sh
curl -fsSL https://github.com/imshaid/oh-my-shell/releases/latest/download/install.sh | sh
```

This puts `oh-my-shell` in `~/.local/bin`, and the short name `omsh` next to it if that name is free (a different file
already called `omsh` is left alone, with a warning). It does not run `chsh`, does not edit your shell's startup files, and
touches `/etc/shells` only with `--add-to-shells`.

**A `.deb`** for Debian and Ubuntu is on the [releases page](https://github.com/imshaid/oh-my-shell/releases):
`sudo apt install ./oh-my-shell_<version>-1_amd64.deb` (or `_arm64.deb`).

**From source.** It is **not on crates.io**. Install it from the repository:

```sh
cargo install --locked --git https://github.com/imshaid/oh-my-shell omsh-cli
```

(The package is called `omsh-cli`; the program is `oh-my-shell`.)

**What the checksum protects.** Each release has a `sha256.sum`, and the installer and `omsh update` compare the download
with it before using it. That catches a corrupted or truncated download. It does not prove who made the file: the checksum
comes from the same GitHub release as the archive, so your trust rests on GitHub and on https. There are no signatures.
`install.sh` itself is **not listed in `sha256.sum`**: it is the program that does the checking, and a sum next to it would add
nothing to https. To read it first, download it and run `sh install.sh`.

More in the [user guide](docs/user-guide.md#install): the options, updating with `omsh update`, uninstalling with `omsh uninstall`.

## What it does

- **The language.** Pipelines, redirections (here-documents too), `&&` `||` `;` `&`, `( )`, `{ }`, `if`, `while`, `until`, `for`,
  `case`, functions, aliases, the usual expansions, job control (`&`, `jobs`, `fg`, `bg`, Ctrl+Z). A POSIX-compatible subset,
  compared with `/bin/sh` by the tests; syntax it does not support (arrays, `[[ ]]`, ...) is reported with the place marked.
  `-c`, scripts and piped input go to `/bin/sh`, so `ssh host cmd` and `scp` keep working.
- **While you type.** Syntax colors, gray hints from your history, Tab completion, abbreviations, emacs or vi keys, a prompt with
  the git branch, `Ctrl+R` history explorer, `Ctrl+P` command palette, a settings view, `j` to jump to a folder you used.
- **Panels.** On a terminal, the questions the shell asks (a dangerous command, `undo`, an AI plan, the jobs list,
  a plan step you paused with Ctrl+Z) are small panels with buttons and keys, and the safe answer is chosen when
  they open. Where nobody can press keys (a pipe, `TERM=dumb`, CI) they are the plain text questions they always were.
- **History.** A local SQLite file with folder, status and duration. A line that matches one of the known secret patterns is not
stored; a secret in a shape the patterns do not know can be (`history delete ID` removes it).
- **Safety.** Every typed line is analyzed first. A dangerous one shows what it would touch and asks; `rm` can move files to a
  trash, and `undo` brings them back.
- **AI (optional).** `? request` plans commands and shows the whole plan for one confirmation (it asks again only if a step turns out riskier
when it is about to run); `?? command` explains one; `Alt+?` explains the last
  failure; `cmd | ? question` asks about output; `? history ...` searches your history in plain words. Providers: Ollama,
  Anthropic, OpenAI, Gemini, any OpenAI-compatible server. Text that matches the known secret patterns is masked before anything is sent, and keys are kept in
  the system keyring or a file only you can read.

## Documentation

- [User guide](docs/user-guide.md): install, use, safety, AI, configuration, troubleshooting.
- [Changelog](CHANGELOG.md): what changed in each version.
- [Command reference](docs/commands.md): every builtin, `omsh` command, key and setting, with the main options.
- [Design](docs/design.md): how it is built, with the measured numbers.
- [Report material](docs/report.md): notes for a written report.
- [Contributing](CONTRIBUTING.md): build, checks and rules.

## License

[MIT](LICENSE)
