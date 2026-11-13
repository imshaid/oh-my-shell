# Oh My Shell: user guide

Oh My Shell is an interactive Linux shell written in Rust. It runs the familiar POSIX shell language,
helps while you type, remembers your commands, checks dangerous commands before they run, and can
optionally ask an AI for help. This guide explains how to install it and use it. The
[command reference](commands.md) lists every builtin, option, key and setting.

**Status:** version 0.1.1. A 1.0 release has not been tagged yet.

Contents: [Install](#install) · [First start](#first-start) · [How the shell is started](#how-the-shell-is-started) ·
[The language](#the-language) · [Job control](#job-control) · [Typing](#typing-editor-prompt-and-views) ·
[History](#history-and-privacy) · [Safety](#safety) · [AI help](#ai-help) · [Configuration](#configuration) ·
[Update and uninstall](#update-and-uninstall) · [Limitations](#known-limitations) · [Troubleshooting and FAQ](#troubleshooting-and-faq)

## Install

Linux on x86_64 or aarch64 with glibc 2.35 or newer: Ubuntu 22.04 and later, Debian 12 and later, Fedora 36 and
later. Alpine (musl) is not supported by the release files; build from source there.

Three ways, all from <https://github.com/imshaid/oh-my-shell>:

**The install script** puts `oh-my-shell` and the short name `omsh` in `~/.local/bin`:

```sh
curl --proto '=https' --tlsv1.2 -fsSL \
  https://github.com/imshaid/oh-my-shell/releases/latest/download/install.sh | sh
```

It changes nothing else: it never runs `chsh`, never edits your shell's startup files, and touches
`/etc/shells` only with `--add-to-shells`. Options: `--version X.Y.Z`, `--system`, `--bin-dir DIR`,
`--add-to-shells` ([list](commands.md#installsh-options)). If `~/.local/bin` is not in your `PATH`, it says so.

**A `.deb` package** (Debian, Ubuntu): download `oh-my-shell_<version>-1_amd64.deb` (or `_arm64.deb`) from the
[releases page](https://github.com/imshaid/oh-my-shell/releases) and run `sudo apt install ./oh-my-shell_*.deb`.
It installs `/usr/bin/oh-my-shell` and `/usr/bin/omsh`, adds the shell to `/etc/shells`, and depends on
`ca-certificates`. It never runs `chsh`. Remove it with `sudo apt remove oh-my-shell`.

**From source.** It is **not on crates.io**; install it from the repository (needs a recent stable Rust toolchain):

```sh
cargo install --locked --git https://github.com/imshaid/oh-my-shell omsh-cli
```

The package is called `omsh-cli` and the program `oh-my-shell`.

### What the checksum protects, and what it does not

Every release has a file `sha256.sum` with the SHA-256 of each archive and package. The install script and
`omsh update` compare the download with it before anything is unpacked or run. **That catches a corrupted or
truncated download. It does not prove who made the file:** `sha256.sum` is served from the same GitHub release
as the archive, so what you trust is GitHub and the https connection to it. There are no signatures.

`install.sh` is **not listed in `sha256.sum`**. It is the program that does the checking, so a sum served next to
it would prove nothing more than https already does. If you want to read it first, download it and run it as a
file: `curl -fsSLO https://github.com/imshaid/oh-my-shell/releases/latest/download/install.sh`, then
`sh install.sh`.

### Using it

Start it from your terminal's profile (GNOME Terminal, Kitty, VS Code and others can run any program), or
type `oh-my-shell` (or `omsh`). **Use it for a while before you run `chsh`.** Programs such as `ssh host cmd`
and `scp` keep working when it is the login shell, because `-c` commands, scripts and piped input are run
by `/bin/sh` (see below).

## First start

The first start writes `~/.config/oh-my-shell/config.toml` (with every setting commented out at its default)
and shows three lines once: where the settings are, the keys `Ctrl+R` and `Ctrl+P`, and `omsh tour`. `omsh tour`
is a one-minute walk through the features. Your own startup commands (aliases, exports, functions) go in
`~/.config/oh-my-shell/init.omsh`, written in the shell's own language; the file is not created for you.

Files (the `XDG_*` variables are respected):

| Path | Contents |
|---|---|
| `~/.config/oh-my-shell/config.toml` | Settings. |
| `~/.config/oh-my-shell/init.omsh` | Your startup commands. |
| `~/.config/oh-my-shell/themes/<name>.toml` | Your themes. |
| `~/.config/oh-my-shell/credentials` | AI keys, only when there is no system keyring (mode 0600). |
| `~/.local/share/oh-my-shell/history.db` | The history (SQLite). |
| `~/.local/share/oh-my-shell/trash/` and `undo.db` | Trashed files and the undo journal. |
| `~/.local/state/oh-my-shell/` | Small state, such as the time of the last update check. |

## How the shell is started

| How | What happens |
|---|---|
| `oh-my-shell` in a terminal | The interactive shell, with everything in this guide. |
| `oh-my-shell -c 'cmd'` | `/bin/sh` runs it at once. No config, no history, no AI, nothing printed by us. |
| `oh-my-shell script.sh` | `/bin/sh` runs the script. |
| Input from a pipe | `/bin/sh` runs it. |
| `-l` or a name starting with `-` | Login shell: the environment that `/etc/profile` and `~/.profile` build (by running `/bin/sh -l`) is imported before the first prompt. |
| `-i` | Forces the interactive shell. |
| `--no-ai` | The AI layer is off for the session. |
| `--no-config` | No `config.toml`, no `init.omsh`, nothing written. The way back in when a startup file is broken. |

So the builtins that need the interactive shell (`history`, `j`, `undo`, `omsh`) do not exist in `-c` and
scripts, and scripts written for bash keep running in `/bin/sh` as they always did.

## The language

The interactive language is a POSIX-compatible subset.

**Supported:** simple commands and assignment prefixes (`FOO=1 cmd`); quoting (`'...'`, `"..."`, backslash);
`|`, `;`, `&&`, `||`, `&`, `!`, `( ... )`, `{ ...; }`; redirections `<`, `>`, `>>`, `n>`, `n<`, `2>&1`, `>&2`,
`n>&-`, `<>`, `>|`, `&>`, `&>>`, here-documents `<<EOF` (quoted or not) and here-strings `<<<`; expansions:
`~`, `$var`, `${var}` with `:-`, `:=`, `:?`, `:+`, `#`, `##`, `%`, `%%`, `${#var}`, the special parameters
(`$? $$ $! $0 $# $@ $* $1 ...`), `$(...)` and backticks, `$((...))` with integers, field splitting on `IFS`,
globbing (`*`, `?`, `[...]`), quote removal; `if`, `while`, `until`, `for`, `case`, functions; aliases;
comments. The test suite runs the same input through this shell and through `/bin/sh` (`dash` on Debian and Ubuntu) and compares the output and the exit status; in a few places it follows bash on purpose.

**Not supported:** arrays, `[[ ]]`, process substitution `<( )`, brace expansion `{a,b}`, `select`,
coprocesses, `shopt`, and bash-only parameter forms such as `${var//a/b}`. When you type one, the shell
says so with the place marked and a hint, and runs nothing:

```
❯ x=(1 2)
omsh::syntax::unsupported

  × array syntax is not supported in Oh My Shell
   ╭────
 1 │ x=(1 2)
   · ─┬─
   ·  ╰── not supported
   ╰────
  help: use separate variables, or the positional parameters with `set --`
```

Run such a script with `bash script.sh`. The [builtins](commands.md#builtins) are `:`, `.`, `[`, `alias`, `bg`,
`break`, `cd`, `command`, `continue`, `disown`, `echo`, `eval`, `exec`, `exit`, `export`, `false`, `fg`, `hash`,
`history`, `j`, `jobs`, `kill`, `omsh`, `printf`, `pwd`, `read`, `readonly`, `return`, `set`, `shift`, `source`,
`test`, `trap`, `true`, `type`, `umask`, `unalias`, `undo`, `unset`, `wait`.

## Job control

Each command runs in its own process group, so Ctrl+C stops the command and not the shell. A line that ends
with `&` runs in the background. `jobs` lists jobs, `fg %1` brings one back, `bg %1` lets a stopped one go on in
the background, `wait` waits, `kill %1` signals, `disown %1` forgets one. Ctrl+Z stops the foreground job and
keeps it. Leaving the shell with stopped jobs gives a warning first. Finished background jobs are reported
once, before the next prompt.

## Typing: editor, prompt and views

**Colors.** A command that exists is plain, one that does not is red, quotes are yellow, options and arguments
are cyan, an existing file is underlined. Only the 16 ANSI colors are used, so your terminal theme decides how
they look. `NO_COLOR` is respected.

**Hints.** Gray text after the cursor suggests the rest of a command from your history, with commands from the
current folder first. Right or End takes all of it, Alt+F one word.

**Completion.** Tab completes commands (programs, builtins, functions, aliases), paths, variables and git
branches, and options of programs when fish completion files are installed on the system. Tab again moves
through the choices.

**Abbreviations.** In `[editor.abbreviations]`, `gst = "git status"` turns `gst` followed by a space (or Enter) into
`git status` where a command name goes. The history stores the long form.

**Pasting** several lines inserts them; nothing runs until you press Enter. **Enter** waits for more when the
command is unfinished (an open quote, `if` without `fi`).

**The prompt** has the folder, the git branch with a dirty marker (read in the background, never slowing the
prompt), a virtual environment, the status of the last command and its duration when long. `prompt.left`,
`prompt.right` and `prompt.transient` change it. If a command is not found, a line suggests a similar name.

**Views** take the whole screen and give it back:
- `Ctrl+R`: the history explorer. One row for each distinct command, fuzzy filter, scope all / this folder /
  failed (Ctrl+R again changes it). Enter puts the command in the line; it never runs it. It lists the newest 10,000
  runs and says so.
- `Ctrl+P`: the command palette: actions (settings, history explorer, explain the last failure, undo, jobs, AI
  status, AI setup, edit `config.toml`), aliases, abbreviations, functions and builtins with a one-line help.
  Picking puts text in the line; it never runs it.
- Settings (from the palette): every key with its value, where it comes from, and a mark when the value
  changes at the next start. Switches and fixed choices can be changed there and are written to `config.toml`.

**Notifications.** A command that ran 10 seconds or more ends with a desktop notification through
`notify-send` (package `libnotify-bin`), unless it was ended with Ctrl+C or stopped, was a `?` request, or is in
the skip list (editors, pagers, `ssh`, `tmux`, shells). Over ssh, or without `notify-send`, nothing is shown
and the shell says so once. `[notify]` changes or switches it off. The notification shows the typed command
(secrets masked).

**Themes.** `ui.theme` names a built-in theme (`nord`) or `themes/<name>.toml`. A theme file lists only the
colors you change: eight tokens (`accent`, `path`, `ai`, `success`, `warning`, `danger`, `muted`, `dim`) and the
command-line roles (`command`, `error`, `quote`, ...), each one of the 16 ANSI color names.

## History and privacy

Every command is stored in a local SQLite file with its folder, exit status, duration, session and host. Shells
running at the same time share it safely. `history` lists and searches it; `j words` jumps to a folder you used;
`Ctrl+R` explores it. See the [options of `history`](commands.md#history).

- A line that starts with a space is not stored (`history.ignore`).
- A line that looks like it holds a secret is **never stored**, whatever the settings say: `*_KEY=`, `*_TOKEN=`,
  `*_SECRET=`, `password=`, `Authorization: Bearer ...`, key formats of AWS, GitHub, Google, Anthropic and
  OpenAI, private key blocks, and `user:pass@` in addresses. These rules are the same ones that mask text before
  it is sent to an AI. A secret that matches none of them can still be stored; `history delete ID` removes an entry.
- `--no-config` opens no history file. There is no telemetry of any kind.

## Safety

Every line you type is analyzed offline (no network, a fraction of a millisecond) after aliases are replaced and
before anything runs. Levels: **safe** runs; **caution** shows a one-line note and runs; **dangerous** shows
a panel and asks; **critical** asks you to type `yes` or the target. `[safety] confirm` sets the lowest level
that asks (default `dangerous`); critical always asks. The rules cover `rm` of big or important paths,
`dd` and formatting, `chmod`/`chown` of system folders, fork bombs, opening the firewall, creating
accounts, `curl ... | sh`, `git push --force`, `git reset --hard`, `git clean`, and overwriting a file with `>`. A `sudo` in front raises a
command that is already flagged by one step.

```
❯ rm -rf *
oh-my-shell: this command is dangerous:
│ rm -rf *
│ - Deletes files without asking, and folders with everything in them. This cannot be undone, unless you answer t to move it to the trash.
│   on: *
│   touches 1 folder
│   none outside the current folder; none owned by someone else
│   for example: src
Run it? [y/N] (t = move to the trash instead) n
oh-my-shell: cancelled; nothing ran
```

**Blast radius.** For file commands the panel counts what would be touched: files and folders, total size,
how many are outside the current folder or not owned by you, and up to five example paths. The count stops
at `safety.blast_radius_max_entries` entries or `safety.blast_radius_max_ms` milliseconds and then says "at least";
it never prints a number it did not count.

**Trash and undo.** Answer `t` and the `rm` moves the files to the trash (`~/.local/share/oh-my-shell/trash`,
kept 8 days by default) and writes an undo entry. `undo` puts back the newest recorded action with its original
permissions; `undo --list` shows them; `undo 3` picks one. A plain `rm file` that the analyzer calls safe just
deletes: only deletions that went through the question or through an AI plan are in the journal. Commands you run
directly are not journaled.

**Not analyzed:** functions when they are called later, files run with `.`/`source`, traps, `init.omsh`, `-c`,
and scripts. The rules do not know `rsync --delete`, `truncate`, `tee file`, `ssh host 'rm -rf /'`, or a command
name that only exists at run time. `rm $flags /` (flags in a variable) counts as dangerous, not critical.

## AI help

**The AI layer is off until you set it up** (`omsh ai setup`). The shell is fully usable without it, and
`--no-ai` or `ai.enabled = false` turns it off for good.

**There is no network access on the normal command path.** Typing, running commands, completion, the safety analysis,
history, the prompt and `-c` open no network socket. A test runs the shell under `strace` and the whole test suite
with the network switched off to check this. The network is used only when you ask: an AI request (`?`, `??`,
`| ?`, `Alt+?`), `omsh ai setup` and `omsh ai test`, `omsh update`, and the daily update check, which is off by
default. The AI code starts on the first AI request, not before.

**Setup.** `omsh ai setup` asks for a provider (Ollama, Anthropic, OpenAI, Gemini, or any OpenAI-compatible
server), a model and a key, checks them with one small request, and writes `provider`, `model` and `base_url` to
`config.toml`. Keys are never written to `config.toml`, to the history or to logs: they go in the system keyring,
or in a file only you can read when there is no keyring. A key is sent only to `https://` addresses and to this
computer. `omsh ai status` shows what is set (never the key); `omsh ai forget` removes the key.

**Asking.**

| You type | What happens |
|---|---|
| `? list the biggest files here` | The model proposes a plan of up to 8 steps. You see each command with its explanation and risk, and run them only after you confirm. |
| `?? tar -xzf a.tgz` | Explains the command flag by flag. Nothing runs. |
| `??` alone, or `Alt+?` | Explains why the last command failed, from the command and its exit status. The error text is not captured, so it is not sent. |
| `ls -l \| ? which is the biggest` | Runs `ls -l`, sends its (truncated, masked) output with the question. |
| `? history what did I run with docker last week` | Searches your history in plain words; the answer comes from the local database. |

**The plan panel** keys: Enter runs, `e` edits a command, `c` chats ("make it recursive"), `v` shows exactly
what was sent, Esc cancels. **Every AI command needs your confirmation, whatever its risk.** It goes through the
same parser, safety analyzer and executor as typed commands, and the analyzer's level wins over the model's. A
failed step stops the plan. Steps run exactly as written: your aliases are not applied. AI deletes go to the
trash and into the undo journal.

**What is sent.** The system name, the shell name, the working folder, whether it is a git repository and the
branch, and the last 3 commands with their exit status (never their output), each switchable in
`[ai.context]`. File contents are never sent unless you pipe them with `| ?`. Secrets are replaced by `[MASKED]`
first. For `| ?`, the system prompt tells the model to ignore any instruction found in the piped output.
Answers and plans are in English.

## Configuration

`config.toml` is TOML; every [setting](commands.md#settings-configtoml) is listed in it, commented out, with its default.
A mistake never stops the shell: it is reported as `omsh: ...` with its line and column, the setting keeps its
default, and the rest of the file applies. Changes apply at the next start. The settings view (`Ctrl+P`, then
"Settings") changes the switches for you.

`init.omsh` runs before the first prompt; put aliases, exports and functions there:

```sh
alias ll='ls -l'
export EDITOR=nano
```

## Update and uninstall

**Update.** `omsh update` looks up the latest GitHub release, downloads the archive for your CPU, checks it against
`sha256.sum` (see [what that protects](#what-the-checksum-protects-and-what-it-does-not)), runs the new program once
with `--version`, and only then renames it over the old one. A shell already running keeps the old file until you
restart it. `omsh update --check` only says whether a newer release exists. It refuses a copy installed by a
package manager (under `/usr`) or by `cargo` and says how to update that one: `sudo apt install ./new.deb`
or the `cargo install` command again. It never installs an older version. For a notice when a release comes out, set
`check = true` under `[update]`: the shell then asks GitHub for the latest version at most once a day, a moment after
the prompt, and shows one line (nothing is downloaded). It is off by default because it needs the network.

**Uninstall.** Run `omsh uninstall` from another shell (it is refused inside Oh My Shell, which has its history
open). It asks one question at a time: the program, your settings, your history, the trash with its undo journal, the
stored AI keys. Nothing is removed until every question is answered, and Esc or Ctrl+C stops with nothing removed.
It refuses while Oh My Shell is your login shell (change that with `chsh` first). It never edits `/etc/shells`; it
tells you what to remove there. A `.deb` is removed with `sudo apt remove oh-my-shell`, a cargo install with
`cargo uninstall omsh-cli`.

## Known limitations

- Keys that arrive in the same read as Ctrl+C or Ctrl+D are dropped (a script on a pty, `tmux send-keys` with
  several keys, a paste that is not bracketed). Typing by hand is not affected. The second of two lines that
  arrive together is also dropped.
- A secret that the masking rules do not know is stored in the history and can be sent as a recent command;
  `history delete ID` removes it. A key glued to a word (`fix-ghp_...`) is not recognized.
- A dangerous line you cancelled at the question is still noted as a recent command for the AI.
- The shell cannot use a proxy for `omsh update` and the AI providers.
- HTTPS trusts the system certificate store; without `ca-certificates` the message is "TLS certificate or
  handshake was not accepted".
- `undo` with no number takes the newest action of any shell, not only this one.
- The trash copy across file systems keeps content, mode, times and (as root) owner, but not hard links,
  extended attributes or ACLs.
- The release files were run on x86_64 hardware; the aarch64 files were checked by checksum and by build tests
  only.
- `history.size` is applied when a shell starts.
- The shell runs on Linux only.

## Troubleshooting and FAQ

**My `init.omsh` or `config.toml` is broken and the shell misbehaves.** Start with `oh-my-shell --no-config`: it
reads neither file and writes nothing. A mistake in `config.toml` is only reported, and the default is used.

**There are no colors.** Colors are off when `NO_COLOR` is set to something that is not empty, when `TERM` is `dumb`, and when the output is not a terminal.

**The editor looks plain, with no hints or completion.** With `TERM=dumb`, an unset `TERM` or inside Emacs
(`INSIDE_EMACS`), the shell uses a plain line reader. The same happens if the terminal does not answer the
cursor-position question within half a second; the shell then says so once.

**"The history cannot be read right now."** The history file is still opening, a moment after the start; try again.
It waits for the file, so this should be rare.

**`history`, `j`, `undo` or `omsh` say "not available in this shell".** You are in `-c`, a script or piped input; those
run in `/bin/sh`. They exist in the interactive shell.

**The AI says "AI is not set up".** Run `omsh ai setup`. For a local model, install Ollama and choose it there.

**"The model gave no answer" after a long wait.** Some models spend their whole output budget thinking (seen with
a small local model). Try another model.

**`omsh ai setup` waits for a long time.** A locked system keyring can wait for your desktop's unlock prompt; Ctrl+C
cannot interrupt that call. Unlock the keyring, or end the program from another terminal.

**"cannot reach the release server" / TLS errors.** The machine has no `ca-certificates`, no network, or needs a proxy,
which is not supported.

**"too many requests" from `omsh update` on version 0.1.0.** 0.1.0 asked the GitHub API, which allows 60 requests an
hour per address. 0.1.1 and later do not. Install 0.1.1 or later with `install.sh` or the `.deb`.

**Why is a notification missing?** It needs `notify-send` and a desktop session; commands shorter than
`notify.min_seconds`, ended with Ctrl+C, stopped, or in the skip list never notify.

**Is it safe to make it my login shell?** `ssh host cmd`, `scp`, cron and scripts are run by `/bin/sh`, so they
do not depend on Oh My Shell. The project is young; use it from a terminal profile first, and keep another shell
installed. `chsh` is yours to run; the installers never do.

**Does it send my commands anywhere?** No, unless you use an AI trigger; then only what is under
[What is sent](#ai-help). No telemetry of any kind.

**Does the safety check protect me from everything?** No. It is a list of rules ([not analyzed](#safety)), meant to catch
common mistakes. It asks; it does not replace care.
