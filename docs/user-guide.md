# Oh My Shell: user guide

Oh My Shell is an interactive Linux shell written in Rust. It runs the familiar POSIX shell language,
helps while you type, remembers your commands, checks dangerous commands before they run, and can
optionally ask an AI for help. This guide explains how to install it and use it. The
[command reference](commands.md) lists every builtin, option, key and setting.

**Status:** version 1.0.0.

Contents: [Install](#install) · [First start](#first-start) · [How the shell is started](#how-the-shell-is-started) ·
[The language](#the-language) · [Job control](#job-control) · [Typing](#typing-editor-prompt-and-views) ·
[History](#history-and-privacy) · [Safety](#safety) · [AI help](#ai-help) · [Configuration](#configuration) ·
[Update and uninstall](#update-and-uninstall) · [Limitations](#known-limitations) · [Troubleshooting and FAQ](#troubleshooting-and-faq)

## Install

Linux on x86_64 or aarch64 with glibc 2.35 or newer: Ubuntu 22.04 and later, Debian 12 and later, Fedora 36 and
later. Alpine (musl) is not supported by the release files; build from source there.

Three ways, all from <https://github.com/imshaid/oh-my-shell>:

**The install script** puts `oh-my-shell` in `~/.local/bin`, and the short name `omsh` beside it when that name is free:

```sh
curl -fsSL https://github.com/imshaid/oh-my-shell/releases/latest/download/install.sh | sh
```

For a stricter download, add `--proto '=https' --tlsv1.2` after `curl`: the first lets curl start only with an https
address, the second refuses TLS older than 1.2.

It makes the short name `omsh` only when that name is free (another file called `omsh` is left alone, with a
warning). It never runs `chsh`, never edits your shell's startup files, and touches `/etc/shells` only with
`--add-to-shells`. Options: `--version X.Y.Z`, `--system`, `--bin-dir DIR`,
`--add-to-shells` ([list](commands.md#installsh-options)).

**The `PATH` note.** If `~/.local/bin` is not in your `PATH`, the installer prints the line to add for your shell
(`fish_add_path ~/.local/bin` for fish; an `export PATH=...` line and the name of the startup file for bash and zsh;
the generic `export` line for any other shell). It only prints it: it never edits a startup file for you.

**What the installer shows.** On a terminal it shows a header, a spinner for each step that settles into `✓`, a
real download line (percent and speed, only when the server says how big the file is), and, at the end, the time
the new program took for `--version`, measured by running it ten times. A pipe, a file or `TERM=dumb` gets one
plain line for each step and no escape codes. A step that fails is settled into `✘` (`x` in ASCII) with its label on a line
of its own, and the message and what curl or wget said follow on lines of their own. `NO_COLOR` removes the colors, a locale that is not UTF-8 gives
ASCII symbols, and `OMSH_MOTION=off` removes the spinner.

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

The first start writes `~/.config/oh-my-shell/config.toml` (every setting is a commented line: where a default exists it shows the default, and for the unset ones, such as the
motion, provider, model and server address, it shows an example, not a default)
and, once, asks up to three questions on a terminal. Each starts on the answer that changes nothing, and `Esc` is
that answer:

1. **Welcome:** take the tour, set up AI now (not offered with `--no-ai` or `ai.enabled = false`), or skip.
2. **Stay up to date?** Yes means one request a day to GitHub for the name of the newest release (nothing is
   downloaded or installed) and one line at the next start when there is news; it writes `update.check = true`.
   The first button is "Yes", but the chosen one is **"No, I'll run omsh update --check myself"**, which writes
   `update.check = false`.
3. **Use eza for ls?** Asked only when `eza` is on your `PATH`. Oh My Shell does not read the aliases of fish or
   bash, so `ls` runs the system `ls` unless you say otherwise. Yes writes `ui.ls = "eza"` and `ls` runs `eza` from
   then on (at once in this session, and at every start, while `eza` is on `PATH`). **To undo it:** `Ctrl+P`,
   Settings, `ui.ls` (Enter switches it back to `ls`), or `unalias ls` for the current session. Nothing is changed
   unless you choose it.

Without a terminal with keys (a pipe, `TERM=dumb`, CI) or with `OMSH_FIRST_RUN=plain`, nothing is asked and the
three lines of text are shown instead: where the settings are, the keys `Ctrl+R` and `Ctrl+P`, and `omsh tour`.
`omsh tour` is a one-minute walk through the features. Your own startup commands (aliases, exports, functions) go in
`~/.config/oh-my-shell/init.omsh`, written in the shell's own language; the file is not created for you.

Files (the `XDG_*` variables are respected):

| Path | Contents |
|---|---|
| `~/.config/oh-my-shell/config.toml` | Settings. |
| `~/.config/oh-my-shell/init.omsh` | Your startup commands. |
| `~/.config/oh-my-shell/themes/<name>.toml` | Your themes. |
| `~/.config/oh-my-shell/credentials` | AI keys, when there is no system keyring or storing in it failed (mode 0600). |
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
| `--no-config` | No `config.toml`, no `init.omsh`, and nothing saved by itself (no config, history or check-stamp file). It does not stop commands you give on purpose (`omsh update`, `omsh uninstall`). The way back in when a startup file is broken. |

So the builtins that need the interactive shell (`history`, `j`, `undo`, `omsh`) do not exist in `-c` and
scripts. `oh-my-shell script.sh` always hands the file to `/bin/sh`, whatever its first line says, so a script that
needs bash must be run as `bash script.sh`.

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

Each job runs in its own process group (the programs of a pipeline share one; builtins run inside the shell; with `set +m`
no new groups are made), so Ctrl+C stops the job and not the shell. A line that ends
with `&` runs in the background. `jobs` lists jobs, `fg %1` brings one back, `bg %1` lets a stopped one go on in
the background, `wait` waits, `kill %1` signals, `disown %1` forgets one. Ctrl+Z stops the foreground job and
keeps it. Leaving the shell with stopped jobs gives a warning first. Finished background jobs are reported
once, before the next prompt. At a terminal with the line editor, `jobs -i` (or "Jobs" in `Ctrl+P`) shows the jobs
as a panel: choose a job, then Foreground, Background (a stopped job), Stop (a running one) or Kill (asks first).

## Typing: editor, prompt and views

**Colors.** While you type, a command that exists is green and one that does not is red (also after `|`, `&&`,
`||`, `;` and `&`), options are cyan, quotes are yellow, an existing file is underlined, and what joins commands or
redirects (`|`, `&&`, `||`, `;`, `&`, `>`) is magenta. Plain arguments keep your terminal's text color. A word
that holds an expansion (`ls$x`) cannot be checked before it runs, so it is neither green nor red. Only the 16
ANSI colors are used, so your terminal theme decides how they look. `NO_COLOR` is respected. Any of these can be
changed in a theme file (see "Themes").

**Hints.** Faint text after the cursor suggests the rest of a command from your history, with commands from the
current folder first. Right or End takes all of it, Alt+F one word.

**Completion.** Tab completes commands (programs, builtins, functions, aliases), paths, variables and git
branches, and options of programs when fish completion files are installed on the system. Tab again moves
through the choices.

**Abbreviations.** In `[editor.abbreviations]`, `gst = "git status"` turns `gst` followed by a space (or Enter) into
`git status` where a command name goes. The history stores the long form.

**Pasting** several lines inserts them; nothing runs until you press Enter. **Enter** waits for more when the
command is unfinished (an open quote, `if` without `fi`).

**The prompt** has the folder, the git branch with a dirty marker (read in the background, never slowing the
prompt), a virtual environment, and, at the right, how the last command ended. `prompt.left`,
`prompt.right` and `prompt.transient` change it. If a command is not found, a line suggests a similar name.

**The look of the prompt** is `ui.style`. The default, `spaced`, prints a blank line and a thin dim rule above the
prompt of every command after the first (not after Ctrl+C or an empty line), puts the folder in blue, and shows at
the right of the header how the last command ended: `✓ 4 ms` in green, or `✘ 127` in red (with the time added
when it took over two seconds). The time is the real time of that command; before the first command there is no
chip. The `status` and `duration` segments of `prompt.right` are that chip. While the line you type is a request
to the AI (`?`, `??`, `| ?`) the tag `ask AI` shows in magenta at the right; it is not shown when the AI is
switched off. Without Unicode the marks are `ok` and `x`, and the rule is made of `-`. `ui.style = "plain"` gives
the two-line prompt of version 1.0, with no rule and a status only after a failure; since 1.1 its text after the
bold folder is plain, not faint.

**The hint bar** is one line above the first prompt of a session that names the keys worth knowing:
`? ask AI · Ctrl+P palette · Ctrl+R history · omsh tour` (and `omsh ai setup` while the AI is not set up). With
`ui.greeting = "auto"` (the default) it is shown for the first five sessions after the welcome and then never;
the count is a small file, `greeting`, in the state folder (`~/.local/state/oh-my-shell`). `always` shows it at
every start and `never` not at all. It is not shown without the line editor or with `--no-config`.

**Terminal marks.** On a terminal the shell also writes the OSC 133 marks (where each prompt starts, where a
command's output starts and ends with its status) that kitty, WezTerm and iTerm2 use to jump between prompts and
copy the last output. They are not written for `-c`, scripts, pipes, `TERM=dumb` or `TERM=linux`.

**Views** take the whole screen and give it back:
- `Ctrl+R`: the history explorer. One row for each distinct command, fuzzy filter, scope all / this folder /
  failed (Ctrl+R again changes it). Enter puts the command in the line; it never runs it. It lists the newest 10,000
  runs and says so.
- `Ctrl+P`: the command palette: actions (settings, history explorer, explain the last failure, undo, jobs, AI
  status, AI setup, edit `config.toml`), aliases, abbreviations, functions and builtins with a one-line help.
  Picking an alias, abbreviation, function or builtin puts its name in the line; picking an action opens the history
  explorer, the settings or the jobs panel, or explains the last failure. Nothing is run by picking.
- Settings (from the palette): every key with its value, where it comes from, and a mark when the value
  changes at the next start. Switches and fixed choices can be changed there and are written to `config.toml` (`update.check` and
  `safety.confirm` are shown, with instructions to edit the file, but not changed there).

**Notifications.** A command that ran 10 seconds or more ends with a desktop notification through
`notify-send` (package `libnotify-bin`), unless it was ended with Ctrl+C or stopped, was a `?` request, or is in
the skip list (editors, pagers, `ssh`, `tmux`, shells). Over ssh, or without `notify-send`, nothing is shown
and the shell says so once. `[notify]` changes or switches it off. The notification shows the typed command
(secrets masked).

**Themes.** `ui.theme` names a built-in theme (`nord`) or `themes/<name>.toml`. A theme file lists only the
colors you change: nine tokens (`accent`, `key`, `path`, `ai`, `success`, `warning`, `danger`, `muted`, `dim`) and the
command-line roles (`command`, `error`, `quote`, ...), each one of the 16 ANSI color names. `muted` is the secondary
text that carries information (labels, counts, timers, descriptions) and is your terminal's own text color; `dim` is
faint and only for decoration (rules and separators), because faint text is hard to read on some terminal themes.

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

Every line you type is analyzed offline (no network) after aliases are replaced and
before anything runs. Levels: **safe** runs; **caution** shows a one-line note and runs; **dangerous** shows
a panel and asks; **critical** asks you to type `yes` or the target. `[safety] confirm` sets the lowest level
that asks (default `dangerous`); critical always asks. The rules cover `rm` of big or important paths,
`dd` and formatting, `chmod`/`chown` of system folders, fork bombs, opening the firewall, creating
accounts, `curl ... | sh`, `git push --force`, `git reset --hard`, `git clean`, and overwriting a file with `>`. A `sudo` in front raises a
command that is already flagged by one step.

In a terminal with the line editor, a dangerous line opens a small panel under the prompt. It shows the
level, the command and each reason (long text is wrapped, never cut), what the command would touch, and a row
of buttons. The panel goes away when you answer and leaves the screen as it was.

```
╭─ Run it? ──────────────────────────────────────────────────────────────
│ this command is dangerous:
│ rm -rf *
│ - Deletes files without asking, and folders with everything in them.
│   This cannot be undone, unless you choose Move to trash.
│     on: *
│     touches 1 folder
│     none outside the current folder; none owned by someone else
│     for example: src
│
│ [Move to trash]  [Run anyway]   Cancel
╰─ ←→↑↓ move · Enter choose · Esc cancel ────────────────────────────────
```

**Cancel is chosen when the panel opens**, so pressing Enter at once does nothing. `←` `→` `↑` `↓` and Tab move
between the buttons, Enter chooses the one that is marked, and `t`, `y` and `n` choose **Move to trash**, **Run
anyway** and **Cancel** at once. Esc and Ctrl+C cancel (Ctrl+C sets `$?` to 130, as for a program stopped by
Ctrl+C). **Move to trash** is there only when every `rm` in the line can go to the trash. Anything that was
typed before the panel appeared, or while it was being prepared, is thrown away: a stray `y` never answers it.
A critical line has the same panel without buttons: you still have to type `yes` or the target, so one key can
never run it. If the terminal closes while the panel is open, nothing runs.

Where there is no line editor to give keys to (`TERM=dumb`, a pipe, input that is not typed, the plain reader,
an AI plan step), the question is plain text, with the same answers (`y`, `yes`, `t`, `trash`; anything else
cancels):

```
oh-my-shell: this command is dangerous:
  rm -rf *
  - Deletes files without asking, and folders with everything in them. This cannot be undone, unless you answer t to move it to the trash.
    on: *
    touches 1 folder
    none outside the current folder; none owned by someone else
    for example: src
Run it? [y/N] (t = move to the trash instead) n
oh-my-shell: cancelled; nothing ran
```

**Blast radius.** For file commands the panel counts what would be touched: files and folders, total size,
how many are outside the current folder or not owned by you, and up to five example paths. The count stops
at `safety.blast_radius_max_entries` entries or `safety.blast_radius_max_ms` milliseconds and then says "at least";
it never prints a number it did not count.

**Trash and undo.** Answer `t` and the `rm` moves the files to the trash (`~/.local/share/oh-my-shell/trash`,
kept 8 days by default) and writes an undo entry. `undo` with no number opens a picker (below); `undo --list`
shows the actions; `undo 3` picks one and asks `Undo it? [y/N]`; everything is put back with its original
permissions. A plain `rm file` that the analyzer calls safe just
deletes: only deletions that went through the question or through an AI plan are in the journal. Commands you run
directly are not journaled.

**The `undo` picker.** At a terminal, `undo` with no number lists the actions that can still be undone, newest
first, each as the line you typed with its number of items and how long ago. `↑` and `↓` move (type to filter),
and a preview under the list says where the line ran and what undoing it would do. **Enter does not restore:**
it opens a question for that action with **Restore** and **Cancel**, and Cancel is chosen, so restoring takes `y`
(or `←` and Enter). Esc and `n` leave without undoing anything. Without a terminal, or with `undo 3`, the question
is the plain `Undo it? [y/N]`.

**The explain hint.** After a command fails, and only when an AI provider is set up, the header of the next prompt
shows `Alt+? explain` before the result (`✘ 127`). `Alt+?` sends the failed command and its exit status, as
before: its error text is never captured or sent. The hint is not shown after a success, Ctrl+C or Ctrl+Z, with
`--no-ai` or `ai.enabled = false`, in the `plain` style, or once you have used `Alt+?` at that prompt.

**Not analyzed:** functions when they are called later, files run with `.`/`source`, traps, `init.omsh`, `-c`,
and scripts. The rules do not know `rsync --delete`, `truncate`, `tee file`, `ssh host 'rm -rf /'`, or a command
name that only exists at run time. `rm $flags /` (flags in a variable) counts as dangerous, not critical.

## AI help

**The AI layer is off until you set it up** (`omsh ai setup`). The shell is fully usable without it, and
`--no-ai` turns it off for that run only (it writes no setting); `ai.enabled = false` in `config.toml` is the lasting switch.

**There is no network access on the normal command path.** Typing, running commands, completion, the safety analysis,
history, the prompt and `-c` make no connection of their own and create no network socket (the one caveat: `~user` expansion uses the
system account lookup, which on a network-account setup, LDAP or sssd, may reach the network). A test runs the shell under `strace` and the whole test suite
with the network switched off to check this. The network is used only when you ask: an AI request (`?`, `??`,
`| ?`, `Alt+?`), `omsh ai setup` and `omsh ai test`, `omsh update`, and the daily update check, which is off by
default. The AI code starts on the first AI request, not before.

**Setup.** `omsh ai setup` asks for a provider (Ollama, Anthropic, OpenAI, Gemini, or any OpenAI-compatible
server), a model and a key, checks them with one small request, and writes `provider`, `model` and `base_url` to
`config.toml`. Keys are never written to `config.toml`, to the history or to logs: they go in the system keyring,
or in a file only you can read, which is used when there is no keyring or storing in it fails (a key is read from the file if
the keyring does not have it or cannot be read). A key is sent only to `https://` addresses and to this
computer. `omsh ai status` shows what is set (never the key); `omsh ai forget` removes the key.

**Asking.**

| You type | What happens |
|---|---|
| `? list the biggest files here` | The model proposes a plan of up to 8 steps. You see each command with its explanation and risk, and run them only after you confirm. |
| `?? tar -xzf a.tgz` | Explains the command flag by flag. Nothing runs. |
| `??` alone, or `Alt+?` | Explains why the last command failed, from the command and its exit status. The error text is not captured, so it is not sent. |
| `ls -l \| ? which is the biggest` | Runs `ls -l`, sends its (truncated, masked) output with the question. |
| `? history what did I run with docker last week` | Searches your history in plain words; the answer comes from the local database. |

**The plan panel.** At a terminal with the line editor the plan is a panel: each step has a risk word (`safe`, `caution`, `danger`, `critical`) next to its title, and the buttons **Run all**, **Step by step**, **Edit**, **Chat** and **Cancel** are under it. Run all is chosen when it opens, so Enter runs the plan, as it always did. The keys are the old ones plus a few: Enter or `y` runs all, `s` runs it step by step (see below), `e` edits a command (with several steps it asks which), `c` chats ("make it recursive"), `v` shows the last request as it was sent (control characters removed; earlier plans and refused retries show only as character counts), `i` shows the details of the call, Tab shows or hides more numbers under the plan, `n`, `q` and Esc cancel. Anywhere else (`TERM=dumb`, the plain reader, a pipe) the panel is the text it was: `[Enter] run  [e] edit  [c] chat  [v] view context  [Esc] cancel`. **Every plan needs your confirmation, whatever its risk.** One confirmation covers the plan you see; its steps then run one
after the other, and a step is asked about again only if the safety analysis, run again just before it, rates it higher. Each
command goes through the same parser, safety analyzer and executor as typed commands. For a step you did not edit, the
higher of the analyzer's level and the model's applies; for a step you edited, the analyzer's alone. A failed step stops the
plan. Steps run exactly as written: your aliases are not applied. A deletion that the shell can recognize (a plain `rm`)
goes to the trash and into the undo journal when the journal is available; a wrapped `rm`, `find -delete` and the like are
not followed, and the panel says which steps are permanent.

**Paths that do not exist.** If a step runs `cd`, `ls`, `cat`, `less`, `head`, `tail`, `code` or `xdg-open` on a path that is not there, the panel adds a note under the step: `no such path: ../night-before-codebase; did you mean ../night-before?`. The names come from the real entries of the folder where the path first goes wrong. It is only a note: the command is not rewritten, its level does not change and you can still run it. Only that folder is read, only when the plan is shown, and nothing is sent anywhere. Limits: other commands are never checked (a missing path is normal for `mkdir`, `touch`, `cp`, `mv` and redirections); paths built at run time, patterns, `~user`, URLs, commands with an option that may take a value (`head -n 5`), a bare `cd` name when `CDPATH` is set, and `..` from a folder that is a link are left alone; and a path is not noted when the missing name is written in an earlier step or earlier in the same step (`mkdir build && cd build`). A path that a program the plan never names would create can still get a note, and a typo far from every name in the folder gets a note with no suggestion. The disk can change between the panel and the run.

**Pausing a plan.** While a step runs it owns the terminal, so the shell cannot draw a progress display or read a key until the step ends or you stop it: there is no live spinner. What you can do is press Ctrl+Z. At a terminal with the line editor the stopped step then gets a question: **Continue**, **Skip** (ends that step and goes on with the next one), **Stop plan** (chosen when it opens: the step stays stopped, as `jobs` shows) or **To background** (the step goes on, and the rest of the plan does not run). The question shows the plan with a bar and the time so far. A plan of several steps ends with one panel that lists every step and its state. Where there are no panels, Ctrl+Z ends the plan as before.

**What is sent.** The system name, the shell name, the working folder, whether it is a git repository and the
branch, and the last 3 commands with their exit status (never their output), each switchable in
`[ai.context]`. File contents are never sent unless you pipe them with `| ?`. Secrets are replaced by `[MASKED]`
first. For `| ?`, the system prompt tells the model to ignore any instruction found in the piped output.
The prompt asks for English answers and plans; this is not checked.

## Configuration

`config.toml` is TOML; every [setting](commands.md#settings-configtoml) is listed in it as a commented line (the default, or an example for an unset setting).
A mistake never stops the shell: it is reported as `omsh: ...` with its line and column, the setting keeps its
default, and the rest of the file applies. Changes apply at the next start. The settings view (`Ctrl+P`, then
"Settings") changes the switches for you.

`init.omsh` runs before the first prompt; put aliases, exports and functions there:

```sh
alias ll='ls -l'
export EDITOR=nano
```

## Update and uninstall

**Update.** `omsh update` replaces the program with the newest release. In order, it:

1. looks at where the running program is, before it uses the network (see "What it refuses" below);
2. finds the newest release, from where GitHub's `releases/latest` address redirects (the GitHub API is not used),
   and stops with "is the newest release" when that is not newer than the one you run: it never downgrades;
3. downloads the archive for your CPU (`x86_64` or `aarch64`; there is no release for another one) and the
   release's `sha256.sum`, and compares the two before anything is unpacked (see
   [what that protects](#what-the-checksum-protects-and-what-it-does-not));
4. unpacks the one program file and writes it next to the old one as `.oh-my-shell.update-<number>`, so you need the
   right to write in that folder (if you do not have it, the command says so and suggests running it with that
   right, for example `sudo <path> update`);
5. runs the new file once with `--version` and requires it to name the new version; and only then
6. renames it over the old one.

If a step fails, the old file is left as it was and the temporary file is removed. A shell already running keeps the
old file until you restart it; the command ends by saying so (`Updated Oh My Shell 1.0.0 -> 1.1.0 at <path>.`). The
progress lines go to standard error.

`omsh update --check` installs nothing and does not look at where the program is; it prints `Oh My Shell N is
available (you have M). Run: omsh update`, or says you have the newest release. Both forms, like the daily check,
record the time and the release they found in the stamp file unless `--no-config` is given. `omsh update --help` shows
the usage. **Exit status:** 0 when it updated, when you already have the newest release, and for `--check` whatever
it found; 1 for any failure (no network, a bad checksum, a refused place); 2 for a wrong argument; 130 when you press
Ctrl+C.

**What it refuses.** A copy that something else owns: one under `/usr` (but not `/usr/local`), `/bin` or `/sbin` (a
package manager), one in the `cargo` bin folder (`$CARGO_HOME/bin` or `~/.cargo/bin`, found by the program's path),
and a build in a `target/debug` or `target/release` folder of a source tree. It says how to update that one:
`sudo apt install ./new.deb`, or the `cargo install` command again, or to build it again or install a release with
`install.sh`. Two updates running at the same moment are not guarded against each other.

**The daily check.** For a notice when a release comes out, set
`check = true` under `[update]` (the first-start question sets it too): the shell then asks GitHub for the latest
version about once a day (if the stamp file cannot be written it can repeat at each start), a moment after the prompt,
and keeps the answer in the stamp file. Nothing is downloaded. **The news is shown at the next start**, not in the
session that asked: one line where the hint bar goes (`↑ omsh 1.1.0 available · run: omsh update`), at most once
in 24 hours. It is off by default because it needs the network. The environment variable `OMSH_NO_UPDATE_CHECK`
(anything but empty or `0`) turns the check and its line off, and the check never runs in CI or when standard error
is not a terminal.

**An old build, with the check off.** If a release build is more than 60 days old and the daily check is off, the
first prompt of a session says once for that build, `this build is N days old · check for a new one: omsh update
--check`. No network is used to show it. A build made without a release date (a `cargo install`, a development
build) says nothing, because its age would be a guess.

**`omsh status`** shows one page: the version, the build date and age, whether the AI is set up (from the settings
alone), whether the daily check is on and what it last found, the motion mode and what decided it, and what the
terminal can do. It asks nothing and sends nothing; on a pipe it prints `label: value` lines.

**Uninstall.** Run `omsh uninstall` from another shell (it is refused inside Oh My Shell, which has its history
open). It asks one question at a time: the program, your settings, your history, the trash with its undo journal, the
stored AI keys. Nothing is removed until every question is answered, and Esc or Ctrl+C stops with nothing removed.
It refuses while Oh My Shell is your login shell (change that with `chsh` first); if the account database cannot be read,
that check cannot refuse. See the [limitations](#known-limitations) for folders that are links or overlap. It never edits `/etc/shells`; it
tells you what to remove there. A `.deb` is removed with `sudo apt remove oh-my-shell`, a cargo install with
`cargo uninstall omsh-cli`.

## Known limitations

Only the `trap` case in the last entry was run by hand; every other item here was read in the source and was not run.

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
- The trash copy across file systems keeps content, mode and the modification time of files (for folders it tries), and
  the owner when it can (as root); it does not keep access times, the times of symbolic links, hard links, extended
  attributes or ACLs.
- The release files were run on x86_64 hardware here. The aarch64 files were built and smoke-tested (install script, `.deb`
  install, run and removal) on GitHub's native aarch64 runner, and never run on aarch64 hardware of our own.
- `omsh uninstall` follows a symbolic link at the config folder (for example a dotfiles repository) and empties what it
  points to; and if the data or state folder is the same as, or inside, the config folder, removing the program can also
  remove data you said no to. Check `omsh uninstall`'s questions against your folder layout first.
- The check of `--base-url` and `OMSH_RELEASE_SERVER` in `install.sh` looks at the start of the text: an address with a user
  name, such as `http://127.0.0.1:80@host`, passes it. These switches are for tests; do not use addresses you do not trust.
- `install.sh` names its temporary file after the process id, so on a machine with other users a planted link in a shared
  `--bin-dir` could be followed. The default folder is your own.
- `~/.cargo` or `CARGO_HOME` reached through a symbolic link is not recognized as a cargo install, so `omsh update` may replace
  that copy.
- Found while reviewing the process code, not fixed (the `trap` case was reproduced by hand; the others were read in the code and not run): in an interactive shell a background builtin such as `read &` is not cut off from the terminal and can take the keys you type; `trap '' SIGNAL` set in a
  subshell is not passed on to the programs it starts; a hang-up that arrives while a foreground program runs is acted on
  only after that program ends; and if the thread that waits for a stopped program cannot be created, a zombie process can
  remain.
- `history.size` is applied when a shell starts.
- `undo 3` and `cmd | ? question` still ask in plain text, not in a panel, and there is no `omsh changelog` command, so
  the line that says a new version exists does not say what is new.
- While a plan step runs, the step owns the terminal and the shell waits for it, so there is no live progress (no
  spinner or running clock). The panels appear where the shell has the keyboard: the plan, the question after
  Ctrl+Z, and the overview at the end.
- **Not tested:** the plan panel and the pause panel with a real AI key (only the fake provider and the fake
  server were used), and the panels inside tmux and over ssh. The one exception is the Run it? panel, which was run
  once in tmux while the 1.1.0 release was prepared.
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

**`history`, `j`, `undo` or `omsh` are "not found" or "not available".** You are in `-c`, a script or piped input; those run
in `/bin/sh`, which has no such commands and prints its own message. They exist in the interactive shell.

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
