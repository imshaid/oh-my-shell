# Command reference

Every builtin, every `omsh` command, every option, every key and every setting of Oh My Shell,
each with a one-line description, how to use it and one example. For the story around them, read
the [user guide](user-guide.md).

The tests check this page: each builtin, each `omsh` command and each `config.toml` key must appear
here, or `cargo test` fails.

Contents: [Builtins](#builtins) · [omsh commands](#omsh-commands) · [Program options](#program-options-oh-my-shell) ·
[install.sh options](#installsh-options) · [Keys](#keys) · [Settings](#settings-configtoml)

Builtins that read the history, the trash or the AI settings (`history`, `j`, `undo`, `omsh`) exist
only in the interactive shell. `oh-my-shell -c '...'`, a script and piped input run in `/bin/sh`, which
does not have them (see [modes](user-guide.md#how-the-shell-is-started)).

## Builtins

A builtin runs inside the shell because it changes the shell's own state, or because starting a
program for it would be wasteful. Status 0 means success.

### `:`
Do nothing and succeed.
Usage: `: [ignored words]`
Example: `: > notes.txt` makes an empty file.

### `.` and `source`
Run the commands of a file in this shell, so its `cd`, `export` and functions stay.
Usage: `. FILE [ARGUMENT...]` or `source FILE [ARGUMENT...]`
Example: `source ~/work/env.sh`

### `[` and `test`
Check a condition about files, text or numbers; the status is the answer.
Usage: `test EXPRESSION` or `[ EXPRESSION ]`
Example: `[ -d /tmp ] && echo yes`

### `alias`
Define an alias, or list them. An alias replaces the first word of a typed line.
Usage: `alias [NAME=TEXT...]`
Example: `alias ll='ls -l'`

### `bg`
Continue a stopped job in the background.
Usage: `bg [%JOB]`
Example: press Ctrl+Z in `sleep 100`, then `bg %1`.

### `break`
Leave a loop.
Usage: `break [N]`
Example: `for i in 1 2 3; do [ $i = 2 ] && break; echo $i; done` prints `1`.

### `cd`
Change the current folder. `cd -` goes back; `CDPATH` is used for a relative name.
Usage: `cd [-L | -P] [DIR | -]`
Example: `cd /tmp`

### `command`
Run a command skipping functions and aliases, or describe it with `-v` or `-V`.
Usage: `command [-p] [-v | -V] NAME [ARGUMENT...]`
Example: `command -v ls`

### `continue`
Go on with the next round of a loop.
Usage: `continue [N]`
Example: `for i in 1 2 3; do [ $i = 2 ] && continue; echo $i; done` prints `1` and `3`.

### `disown`
Forget a job: the shell no longer waits for it or reports it.
Usage: `disown [%JOB]`
Example: `sleep 100 & disown %1` (in a shell with no other job; `jobs` shows the numbers).

### `echo`
Print the arguments. `-n` leaves out the final newline; `-e` reads backslash escapes.
Usage: `echo [-n] [-e] [ARGUMENT...]`
Example: `echo -e 'a\tb'`

### `eval`
Join the arguments and run the result as a command line.
Usage: `eval [ARGUMENT...]`
Example: `eval "echo $HOME"`

### `exec`
Replace the shell by a program (no new process). With no program, it applies the redirections to the shell.
Usage: `exec [PROGRAM [ARGUMENT...]]`
Example: `exec sleep 1`

### `exit`
Leave the shell with a status (the status of the last command by default).
Usage: `exit [N]`
Example: `exit 0`

### `export`
Put variables in the environment of the programs the shell starts.
Usage: `export NAME[=VALUE]...`
Example: `export EDITOR=nano`

### `false`
Do nothing and fail (status 1).
Usage: `false`
Example: `false || echo failed`

### `fg`
Bring a job to the foreground (and continue it if it was stopped).
Usage: `fg [%JOB]`
Example: `fg %1`

### `hash`
Remember or forget where programs are found. `-r` forgets all.
Usage: `hash [-r] [NAME...]`
Example: `hash -r`

### `history`
List, search or delete the stored commands (interactive shell only).
Usage: `history [--here] [--failed] [--since AGE] [-n COUNT | --all] [search TEXT...]` and `history delete ID...`
Example: `history --failed --since 2d` (AGE is a number and `s`, `m`, `h`, `d` or `w`).

### `j`
Jump to a folder you have been in before; the best match for all the words (interactive shell only).
Usage: `j WORD...` or `j -` (the previous folder)
Example: `j proj src`

### `jobs`
List background and stopped jobs. `-l` adds process ids; `-p` prints only the process group leaders.
Usage: `jobs [-l | -p]`
Example: `sleep 100 & jobs`

### `kill`
Send a signal to a process or a job. `-l` lists the signal names.
Usage: `kill [-s SIGNAL | -n NUMBER | -SIGNAL] PID | %JOB ...` and `kill -l [SIGNAL]`
Example: `kill -TERM %1`

### `omsh`
The shell's own commands (`omsh ai ...`, `omsh tour`, `omsh help`). Details [below](#omsh-commands).
Usage: `omsh COMMAND`
Example: `omsh ai status`

### `printf`
Print formatted text.
Usage: `printf FORMAT [ARGUMENT...]`
Example: `printf '%s has %d files\n' here 3`

### `pwd`
Print the current folder.
Usage: `pwd [-L | -P]`
Example: `pwd`

### `read`
Read a line from standard input into variables. `-r` keeps backslashes; `-p` shows a prompt.
Usage: `read [-r] [-p PROMPT] NAME...`
Example: `echo 'a b' | { read x y; echo $y; }` prints `b`.

### `readonly`
Make variables read-only.
Usage: `readonly NAME[=VALUE]...`
Example: `readonly LIMIT=10`

### `return`
Leave a function or a sourced file with a status.
Usage: `return [N]`
Example: `f() { return 3; }; f; echo $?` prints `3`.

### `set`
Set shell options and the positional arguments. Options: `-e` (stop on error), `-u` (unset is an error),
`-x` (trace), `-f` (no globbing), `-C` (no `>` over a file), `-m` (job control), and `-o name` for
`errexit`, `nounset`, `xtrace`, `noglob`, `noclobber`, `monitor`, `pipefail`. `set -o` lists them.
Usage: `set [-eufxCm] [-o NAME] [--] [ARGUMENT...]`
Example: `set -- a b c; echo $2` prints `b`.

### `shift`
Drop the first positional arguments.
Usage: `shift [N]`
Example: `set -- a b c; shift; echo $1` prints `b`.

### `trap`
Run a command when a signal arrives or when the shell exits.
Usage: `trap [ACTION] SIGNAL...`
Example: `trap 'echo bye' EXIT`

### `true`
Do nothing and succeed.
Usage: `true`
Example: `true && echo ok`

### `type`
Say how a name would be run: alias, function, builtin or program.
Usage: `type NAME...`
Example: `type cd`

### `umask`
Show or set the mask for the permissions of new files. `-S` prints it as letters.
Usage: `umask [-S] [MODE]`
Example: `umask 077`

### `unalias`
Remove aliases (`-a` removes all).
Usage: `unalias [-a] NAME...`
Example: `unalias ll`

### `undo`
Put back files that a deletion moved to the trash (interactive shell only). With no number it takes the
newest recorded action.
Usage: `undo [--list | NUMBER]`
Example: `undo --list`

### `unset`
Remove variables or functions.
Usage: `unset NAME...`
Example: `unset EDITOR`

### `wait`
Wait for background jobs to end; the status is that of the job waited for.
Usage: `wait [%JOB | PID]...`
Example: `sleep 1 & wait`

## omsh commands

`omsh` is also a program (the installer makes the link). `omsh` alone starts the shell; `omsh ai ...`,
`omsh tour`, `omsh help`, `omsh update` and `omsh uninstall` run that command from any other shell.
Inside Oh My Shell it is the builtin `omsh`. `omsh script` hands the script to `/bin/sh`, like the shell.

### `omsh help`
List the commands.
Usage: `omsh help`
Example: `omsh help`

### `omsh tour`
A one-minute walk through what the shell does: six short pages; Enter or `n` for the next, `b` for
the one before, `q` or Esc to leave. On a pipe it prints all pages.
Usage: `omsh tour`
Example: `omsh tour`

### `omsh ai setup`
Choose a provider and a model, store the key, check that it works with one small request.
Usage: `omsh ai setup`
Example: `omsh ai setup` (needs a terminal and the config file).

### `omsh ai status`
Show the AI settings and where the key is kept (never the key).
Usage: `omsh ai status`
Example: `omsh ai status` prints `AI: not set up yet (run: omsh ai setup)` before setup.

### `omsh ai test`
Send one small request to check the key and the model.
Usage: `omsh ai test`
Example: `omsh ai test`

### `omsh ai forget`
Remove the stored key of the chosen provider.
Usage: `omsh ai forget`
Example: `omsh ai forget`

### `omsh update`
Download the latest release, check it against `sha256.sum`, try the new program, and rename it over the
old one. `--check` only says whether a newer release exists. It refuses a copy installed by a package
manager or by `cargo`.
Usage: `omsh update [--check]`
Example: `omsh update --check`

### `omsh uninstall`
Remove the program, and, if you say yes to each question, your settings, history, trash, and stored
AI keys. Nothing is removed until every question is answered. Refused inside the shell and while Oh My
Shell is your login shell.
Usage: `omsh uninstall` (from another shell)
Example: `omsh uninstall`

## Program options (`oh-my-shell`)

| Option | What it does | Example |
|---|---|---|
| (none, on a terminal) | Starts the interactive shell. | `oh-my-shell` |
| `-c COMMAND [NAME [ARG...]]` | Runs the command in `/bin/sh` at once: no config, no history, no AI. | `oh-my-shell -c 'echo $0' name` |
| `SCRIPT [ARG...]` | Runs the script in `/bin/sh`. | `oh-my-shell build.sh` |
| (none, input from a pipe) | Runs the input in `/bin/sh`. | `echo 'echo hi' \| oh-my-shell` |
| `-l`, `--login` | Acts as a login shell: imports the environment that `/bin/sh -l` builds from `/etc/profile` and `~/.profile`. A program name that starts with `-` does the same. | `oh-my-shell -l` |
| `-i` | Forces the interactive shell. | `oh-my-shell -i` |
| `--no-ai` | Turns the AI layer off for this session, whatever `ai.enabled` says. | `oh-my-shell --no-ai` |
| `--no-config` | Reads neither `config.toml` nor `init.omsh`, and writes nothing (no history file either). The way back in when a startup file is broken. | `oh-my-shell --no-config` |
| `-V`, `--version` | Prints the version and exits. | `oh-my-shell --version` |
| `-h`, `--help` | Prints the help. | `oh-my-shell --help` |

## install.sh options

| Option | What it does | Example |
|---|---|---|
| `--version X.Y.Z` | Installs that release instead of the latest. | `sh -s -- --version 0.1.1` |
| `--system` | Installs to `/usr/local/bin` (uses `sudo` when not root). | `sh -s -- --system` |
| `--bin-dir DIR` | Installs to `DIR`. | `sh -s -- --bin-dir ~/bin` |
| `--add-to-shells` | Adds the shell to `/etc/shells` (uses `sudo` when not root). | `sh -s -- --add-to-shells` |
| `--archive FILE --sums FILE` | Uses local files and downloads nothing (for tests). | |
| `--base-url URL` | The folder that holds the release files (https, or http on this computer; for tests). | |
| `-h`, `--help` | Prints the options. | `sh -s -- --help` |

The examples are for `curl ... install.sh | sh -s -- OPTION`; see the
[install section](user-guide.md#install).

## Keys

### The line editor

The editor is [reedline](https://crates.io/crates/reedline); the usual emacs keys of its defaults
(Ctrl+A, Ctrl+E, Ctrl+W, ...) work in emacs mode, and vi insert and normal mode work with
`editor.mode = "vi"`. These are the keys Oh My Shell adds or cares about:

| Key | What it does |
|---|---|
| Enter | Runs the line, or waits for more when the command is unfinished (an open quote, `if` without `fi`). |
| Tab | Opens the completion menu; Tab again moves to the next choice. |
| Shift+Tab | Moves to the previous choice. |
| Right, End | Takes the whole gray hint. |
| Alt+F | Takes one word of the hint. |
| Up, Down | Walk through the history. |
| Ctrl+R | Opens the history explorer. |
| Ctrl+P | Opens the command palette (in emacs, vi insert and vi normal mode). |
| Alt+? | Explains why the last command failed (AI). The typed text stays. |
| Ctrl+C | Drops the half-typed line. |
| Ctrl+D | On an empty line, leaves the shell. |
| Ctrl+Z | While a program runs: stops it (the job is kept; `fg` or `bg` continues it). |
| Space or Enter after an abbreviation | Replaces it by its text. |

A line that starts with `?` is an AI request ([user guide](user-guide.md#ai-help)); `??` explains a command and
`cmd | ? question` asks about its output.

### The history explorer (Ctrl+R)

| Key | What it does |
|---|---|
| (typing) | Filters the list by fuzzy match. |
| Enter, Tab | Puts the chosen command in the line (never runs it). |
| Up, Ctrl+P, Ctrl+K | Moves to an older or worse match. |
| Down, Ctrl+N, Ctrl+J | Moves to a newer or better match. |
| PageUp, PageDown, Home, End | Move by a page, or to the first or last row. |
| Ctrl+R | Changes the scope: all, this folder, failed. |
| Esc, Ctrl+C, Ctrl+G | Closes it. |

### The command palette (Ctrl+P)

| Key | What it does |
|---|---|
| (typing) | Filters actions, aliases, abbreviations, functions and builtins. |
| Enter, Tab | Picks the entry: text goes into the line (never runs), or the action starts. |
| Up, Ctrl+P, Ctrl+K / Down, Ctrl+N, Ctrl+J | Move the choice. |
| PageUp, PageDown, Home, End | Move by a page, or to the first or last row. |
| Esc, Ctrl+C, Ctrl+G | Closes it. |

### The settings view (from the palette: "Settings")

| Key | What it does |
|---|---|
| Enter, Tab | Changes the chosen switch or choice to its next value and writes it to `config.toml`. |
| Shift+Tab | Changes it to the previous value. |
| Up, Down, Ctrl+P, Ctrl+N, Ctrl+K, Ctrl+J, PageUp, PageDown, Home, End | Move the choice. |
| Esc, Ctrl+C, Ctrl+G | Closes it. |

A change applies at the next start of the shell.

### Questions and panels

| Panel | Keys |
|---|---|
| Safety question for a dangerous line | Type `y` or `yes` and Enter to run it; `t` (or `trash`) runs it with `rm` moving files to the trash, where that is possible; anything else, Enter alone, Ctrl+C and Ctrl+D cancel. |
| Safety question for a critical line | Type `yes` or the target's name; anything else cancels. There is no trash choice. |
| AI plan panel | Enter runs; `e` edits the command; `c` chats (asks for a change); `v` shows exactly what was sent; Esc cancels. |
| `omsh tour` | Enter, `n` or Space next; `b` back; `q` or Esc leaves. |

## Settings (`config.toml`)

The first start writes `~/.config/oh-my-shell/config.toml` with every setting listed and commented
out at its default. A mistake in a value is reported with its line and column and the default is used.
The keys, with the default in brackets:

### `[ui]`

- `ui.theme` (`"nord"`): the theme, `"nord"` or the name of `themes/<name>.toml`. Example: `theme = "nord"`.
- `ui.motion` (unset): `"full"`, `"reduced"` or `"off"`; unset means reduced over ssh and full otherwise. Example: `motion = "off"`.
- `ui.icons` (`"auto"`): `"unicode"`, `"ascii"` or `"auto"`. Example: `icons = "ascii"`.

### `[editor]`

- `editor.mode` (`"emacs"`): `"emacs"` or `"vi"`. Example: `mode = "vi"`.
- `editor.hints` (`true`): gray suggestions from the history. Example: `hints = false`.
- `editor.abbreviations` (none): a table of short word to long text. Example: `[editor.abbreviations]` then `gst = "git status"`.

### `[prompt]`

- `prompt.left` (`["cwd", "git", "venv"]`): segments of the left side; the segments are `cwd`, `git`, `venv`, `status`, `duration`, `time`. Example: `left = ["cwd", "git"]`.
- `prompt.right` (`["status", "duration"]`): segments of the right side. Example: `right = ["status", "time"]`.
- `prompt.transient` (`false`): collapse old prompts to a short one. Example: `transient = true`.

### `[history]`

- `history.size` (`100000`): how many commands to keep; older ones are removed when a shell starts. Example: `size = 20000`.
- `history.ignore` (`[" *"]`): patterns of lines that are not stored (`*` any text, `?` one character, the whole line must match). Example: `ignore = [" *", "ls"]`.

### `[safety]`

- `safety.confirm` (`"dangerous"`): the lowest level that asks first: `"caution"`, `"dangerous"` or `"critical"`. Example: `confirm = "caution"`.
- `safety.trash_retention_days` (`8`): days a trashed file is kept. Example: `trash_retention_days = 30`.
- `safety.blast_radius_max_entries` (`50000`): the walk that counts what a command would touch stops at this many entries. Example: `blast_radius_max_entries = 10000`.
- `safety.blast_radius_max_ms` (`300`): or at this many milliseconds. Example: `blast_radius_max_ms = 100`.

### `[ai]`

- `ai.enabled` (`true`): the AI layer is still off until `omsh ai setup`; `false` switches it off. Example: `enabled = false`.
- `ai.provider` (unset): `"ollama"`, `"anthropic"`, `"openai"`, `"gemini"` or `"openai-compatible"`; written by `omsh ai setup`. Example: `provider = "ollama"`.
- `ai.model` (unset): the model name. Example: `model = "llama3.2"`.
- `ai.base_url` (unset): the address of an OpenAI-compatible server or of an Ollama on another computer. Example: `base_url = "http://localhost:11434"`.
- `ai.context` (a table): what may be sent along with a request, the four keys below.
- `ai.context.os` (`true`): the name of the system. Example: `os = false`.
- `ai.context.cwd` (`true`): the working directory. Example: `cwd = false`.
- `ai.context.git` (`true`): whether the folder is in a git repository, and the branch. Example: `git = false`.
- `ai.context.recent_commands` (`true`): the last 3 commands with their exit status, never their output. Example: `recent_commands = false`.

### `[notify]`

- `notify.enabled` (`true`): a desktop notification when a long command ends (`notify-send`). Example: `enabled = false`.
- `notify.min_seconds` (`10`): how long a command must run, 1 to 86400. Example: `min_seconds = 30`.
- `notify.ignore` (editors, pagers, `ssh`, `tmux`, shells and similar): programs that never notify. Example: `ignore = ["vim", "ssh"]`.

### `[update]`

- `update.check` (`false`): ask GitHub for the latest release at most once a day, after the prompt. Example: `check = true`.
- `update.channel` (`"stable"`): only `"stable"` is accepted; it is read and not used. Example: `channel = "stable"`.
