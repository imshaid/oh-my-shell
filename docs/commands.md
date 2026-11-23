# Command reference

Every builtin, every `omsh` command, every key and every setting of Oh My Shell, with the options
of each that matter in daily use, a one-line description, how to use it and one example. Rarely used
corners are not all listed; the tests check that every name is here, not that every option is. For the story around them, read
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
Define an alias, or list them.
An alias is replaced at every command position (after `|`, `;`, `&&` and so on), not only the first word of the line.
Usage: `alias [NAME=TEXT...]`
Example: `alias ll='ls -l'`

### `bg`
Continue stopped jobs in the background. Several jobs may be named.
Usage: `bg [%JOB...]`
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
Forget jobs: the shell no longer waits for them or reports them. `-a` takes all jobs, `-r` only running ones,
`-h` keeps the job but stops the hang-up signal from reaching it.
Usage: `disown [-a | -r] [-h] [%JOB...]`
Example: `sleep 100 & disown %1` (in a shell with no other job; `jobs` shows the numbers).

### `echo`
Print the arguments. `-n` leaves out the final newline; `-e` reads backslash escapes; `-E` turns them off again.
Letters can be combined (`-ne`).
Usage: `echo [-neE] [ARGUMENT...]`
Example: `echo -e 'a\tb'`

### `eval`
Join the arguments and run the result as a command line.
Usage: `eval [ARGUMENT...]`
Example: `eval "echo $HOME"`

### `exec`
Replace the shell by a program. At the top level no new process is made; inside a subshell or a pipeline stage
the program runs as a child and that copy of the shell ends with its status. With no program, it applies the
redirections to the shell.
Usage: `exec [PROGRAM [ARGUMENT...]]`
Example: `exec sleep 1`

### `exit`
Leave the shell with a status (the status of the last command by default).
Usage: `exit [N]`
Example: `exit 0`

### `export`
Put variables in the environment of the programs the shell starts. With no operand, or with `-p`, it lists them.
Usage: `export [-p] [NAME[=VALUE]...]`
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
Usage: `history [--here] [--failed] [--since AGE | --since=AGE] [-n COUNT | -nCOUNT | --all] [search TEXT...]`, `history delete ID...` and `history -h` (or `--help`)
Example: `history --failed --since 2d` (AGE is a number and `s`, `m`, `h`, `d` or `w`).

### `j`
Jump to a folder you have been in before; the best match for all the words (interactive shell only).
Usage: `j WORD...` or `j -` (the previous folder)
Example: `j proj src`

### `jobs`
List background and stopped jobs. `-l` adds process ids; `-p` prints only the process group leaders; `-r` lists
only running jobs and `-s` only stopped ones. Job operands (`%1`) pick jobs. `-i` shows the jobs as a panel with
buttons (see "The jobs panel" under Keys); it cannot be combined with other options or job operands, and when
nobody can press arrow keys (`TERM=dumb`, no line editor) or standard output is not a terminal it prints the same
list as `jobs`.
Usage: `jobs [-ilprs] [%JOB...]`
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
Read a line from standard input into variables. `-r` keeps backslashes; `-p` shows a prompt. With no name the
line goes to `REPLY`.
Usage: `read [-r] [-p PROMPT] [NAME...]`
Example: `echo 'a b' | { read x y; echo $y; }` prints `b`.

### `readonly`
Make variables read-only. With no operand, or with `-p`, it lists them.
Usage: `readonly [-p] [NAME[=VALUE]...]`
Example: `readonly LIMIT=10`

### `return`
Leave a function or a sourced file with a status.
Usage: `return [N]`
Example: `f() { return 3; }; f; echo $?` prints `3`.

### `set`
Set shell options and the positional arguments. Options: `-e` (stop on error), `-u` (unset is an error),
`-x` (trace), `-f` (no globbing), `-C` (no `>` over a file), `-m` (job control), and `-o name` for
`errexit`, `nounset`, `xtrace`, `noglob`, `noclobber`, `monitor`, `pipefail`. `set -o` lists them, `set +o` prints commands
that set them back, and a `+` letter (`set +e`) turns an option off. `set` with no argument lists the variables.
Usage: `set [-+eufxCm] [-+o NAME] [--] [ARGUMENT...]`
Example: `set -- a b c; echo $2` prints `b`.

### `shift`
Drop the first positional arguments.
Usage: `shift [N]`
Example: `set -- a b c; shift; echo $1` prints `b`.

### `trap`
Run a command when a signal arrives or when the shell exits. Only `EXIT`, `HUP`, `INT`, `QUIT`, `USR1`, `USR2`,
`ALRM` and `TERM` can be trapped (`CHLD` and others are refused). With no argument it lists the traps; `-` as the
action puts the default back and an empty action ignores the signal.
Usage: `trap [-p] [ACTION SIGNAL...]`
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
Put back files that a deletion moved to the trash (interactive shell only). With no number, at a terminal, it opens
a picker of the actions that can be undone (`↑` `↓` move, type to filter, Enter reviews, then `y` restores and
`n` or Esc leaves; Cancel is chosen first, so Enter alone restores nothing); without a terminal it takes the
newest recorded action and asks `Undo it? [y/N]`. With a number it asks the same question for that action.
Usage: `undo [--list | NUMBER]`
Example: `undo --list`

### `unset`
Remove variables (the default, or with `-v`) or functions (`-f`).
Usage: `unset [-f | -v] NAME...`
Example: `unset EDITOR`

### `wait`
Wait for background jobs to end. With no operand it waits for all of them and the status is 0; with operands the
status is that of the last one named.
Usage: `wait [%JOB | PID]...`
Example: `sleep 1 & wait`

## omsh commands

`omsh` is also a program (the installer makes the link). `omsh` alone starts the shell; `omsh ai ...`,
`omsh tour`, `omsh help`, `omsh status`, `omsh update` and `omsh uninstall` run that command from any other shell.
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

### `omsh ai details`
Show what the AI calls of this shell session used: for each provider used, its newest call with the model, the context window (used, limit, percent), and the tokens in, thinking, out, cached and total, the time and, for Ollama, the speed (marked as derived). A number the provider did not report is shown as a dash, never as 0. On a terminal it is a table with a tab for each provider (`←→` switch, `Esc` close); in a pipe it is the same rows as text. With no call made yet it says so and ends with status 1. The program `omsh` has no session, so from outside the shell it always says that.
Usage: `omsh ai details`
Example: `omsh ai details` after a `? question`.

### `omsh status`
One page about this installation: the version; the build date and age (or "unknown" for a build with no release date);
whether the AI is set up (read from the settings only: no keyring, no network); whether the daily update check is on,
why it is not running here (the variable `OMSH_NO_UPDATE_CHECK`, CI, or no terminal), and what the last look found;
the motion level and what decided it; the terminal (`TERM`, size, which streams are terminals, Unicode, color,
synchronized output if a panel already asked the terminal) and the paths of the settings file and the state folder.
Nothing is sent and the terminal is not asked anything. On a pipe it prints `label: value` lines with no escape codes.
Usage: `omsh status`
Example: `omsh status`

### `omsh update`
Download the latest release, check it against `sha256.sum`, try the new program, and rename it over the
old one. `--check` does not install anything; it says whether a newer release exists, and (like the daily check) it
records the time in the check stamp unless `--no-config` is given. It refuses a copy under `/usr` (a package
manager; `/usr/local` is allowed), `/bin` or `/sbin`, in the `cargo` bin folder, or in a `target/debug` or
`target/release` build folder, as found from the program's path (a symbolic link at the front of that path can fool
the test); `--check` does not look at where the program is.
Usage: `omsh update [--check]`
Example: `omsh update --check`

### `omsh uninstall`
Remove the program, and, if you say yes to each question, your settings, history, trash, and stored
AI keys. Nothing is removed until every question is answered. Refused inside the shell and while Oh My
Shell is your login shell (if the account database cannot be read, that check cannot refuse). See the
[limitations](user-guide.md#known-limitations) for folders that are links or overlap.
Usage: `omsh uninstall` (from another shell)
Example: `omsh uninstall`

## Environment variables

| Variable | What it does |
|---|---|
| `OMSH_MOTION` | `on` (or `full`), `reduced` or `off`: how much of the interface moves. It wins over `ui.motion`. |
| `OMSH_NO_UPDATE_CHECK` | Anything but empty or `0` turns the daily update check, and the line that announces its news, off. |
| `OMSH_FIRST_RUN` | `plain` makes the first start show three lines of text instead of the welcome and its questions, on a terminal too. |
| `NO_COLOR` | Anything but empty removes the colors; bold, dim, reverse and underline stay. |

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
| `--no-config` | Reads neither `config.toml` nor `init.omsh`, and no longer saves anything by itself (no config, history or check-stamp file). It does not stop commands you give on purpose: `omsh --no-config update` still replaces the program and `--no-config uninstall` still removes after you agree. The way back in when a startup file is broken. | `oh-my-shell --no-config` |
| `-V`, `--version` | Prints the version and exits. | `oh-my-shell --version` |
| `-h`, `--help` | Prints the help. | `oh-my-shell --help` |

## install.sh options

| Option | What it does | Example |
|---|---|---|
| `--version X.Y.Z` | Installs that release instead of the latest. | `sh -s -- --version 0.1.1` |
| `--system` | Installs to `/usr/local/bin` (uses `sudo` only if that folder is not writable for you). | `sh -s -- --system` |
| `--bin-dir DIR` | Installs to `DIR`. | `sh -s -- --bin-dir ~/bin` |
| `--add-to-shells` | Adds the shell to `/etc/shells` (uses `sudo` when not root). | `sh -s -- --add-to-shells` |
| `--archive FILE --sums FILE` | Uses local files and downloads nothing (for tests). | |
| `--base-url URL` | The folder that holds the release files (for tests). The address must start with `https://` or with `http://` followed by `127.0.0.1`, `localhost` or `[::1]`; this is a check of the beginning of the text, so an address with a user name (`http://127.0.0.1:80@host`) is not refused. | |
| `-h`, `--help` | Prints the options. | `sh -s -- --help` |

Environment: `NO_COLOR` removes the colors of the installer's display and `OMSH_MOTION=off` removes its spinner
(see the [install section](user-guide.md#install)).

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
| Tab | Opens the completion menu; Tab again moves to the next choice (emacs and vi insert mode, not vi normal mode). |
| Shift+Tab | Moves to the previous choice (same modes). |
| Right, End | Takes the whole faint hint. |
| Alt+F | Takes one word of the hint. |
| Up, Down | Walk through the history. |
| Ctrl+R | Opens the history explorer. |
| Ctrl+P | Opens the command palette (in emacs, vi insert and vi normal mode). |
| Alt+? | Explains why the last command failed (AI). The typed text stays. After a failure, and when the AI is set up, the prompt shows `Alt+? explain` before the result. |
| Ctrl+C | Drops the half-typed line. |
| Ctrl+D | On an empty line, leaves the shell. |
| Ctrl+Z | While a program runs: stops it (the job is kept; `fg` or `bg` continues it). |
| Space or Enter after an abbreviation | Replaces it by its text. |

A line that starts with `?` is an AI request ([user guide](user-guide.md#ai-help)); `??` explains a command and
`cmd | ? question` asks about its output.

### The question before a risky line ("Run it?")

A dangerous line opens a panel with three buttons (`Move to trash` only when the trash can take the `rm`). It is
shown only to a person at a terminal with the line editor; everywhere else the question is the text `Run it? [y/N]`.

| Key | What it does |
|---|---|
| `←` `→` `↑` `↓`, Tab, Shift+Tab | Move between the buttons (Cancel is chosen when the panel opens). |
| Enter | Chooses the marked button. |
| `t` | Move to trash: runs the line with its `rm`s going to the trash. |
| `y` | Run anyway. |
| `n`, Esc | Cancel: nothing runs, `$?` is 1. |
| Ctrl+C | Cancel, `$?` is 130. |

A critical line is not answered with a key: type `yes` or the target.

### The `undo` picker

| Key | What it does |
|---|---|
| `↑` `↓` | Move between the actions. Type to filter. |
| Enter | Opens the question for the marked action (nothing is restored yet). |
| `y` | Restore (at the question). |
| `n`, Esc | Leave without undoing anything. |

### The jobs panel (`jobs -i`, or "Jobs" in the palette)

One row for each job: `%` and its number with the mark (`+` current, `-` previous), the state in the words of
`jobs` (`Running`, `Stopped`, `Done`, `Exit 3`, `Terminated`), the time since the job was made (`m:ss`; it stops when
the job ends) and the command. An action that does not fit the chosen job is shown in parentheses (`(Stop)`) and does nothing.

| Key | What it does |
|---|---|
| `↑` `↓` | Choose a job. |
| `←` `→`, Tab | Choose a button that fits the job. |
| `f` | Foreground: continue the job in front and wait for it (`fg`). Closes the panel. |
| `b` | Background: let a stopped job run without the terminal (`bg`). |
| `s` | Stop: stop a running job (`SIGSTOP`). |
| `k` | Kill: asks first (Cancel is chosen). Sends `SIGTERM`, as `kill %1`; a stopped job is continued so that it can end. |
| Enter | Takes the chosen button. |
| Esc, Ctrl+C | Closes the panel. Finished jobs that it showed are forgotten, as `jobs` forgets the ones it lists. |

The rows are read again once a second while the panel is open (not with `ui.motion = "off"`: then they are read at
each key). Picking "Jobs" in the palette runs `jobs -i` and leaves what you were typing in the line.

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
| Enter, Tab | Picks the entry. Aliases, abbreviations, functions and builtins put text in the line (never run it); an action opens history, opens settings, shows the jobs panel, or explains the last failure. |
| Up, Ctrl+P, Ctrl+K / Down, Ctrl+N, Ctrl+J | Move the choice. |
| PageUp, PageDown, Home, End | Move by a page, or to the first or last row. |
| Esc, Ctrl+C, Ctrl+G | Closes it. |

### The settings view (from the palette: "Settings")

| Key | What it does |
|---|---|
| Enter, Tab | Changes the chosen switch or choice to its next value and writes it to `config.toml`. `safety.confirm` is shown but not changed here; selecting it shows how to edit the file. |
| Shift+Tab | Changes it to the previous value. |
| Up, Down, Ctrl+P, Ctrl+N, Ctrl+K, Ctrl+J, PageUp, PageDown, Home, End | Move the choice. |
| Esc, Ctrl+C, Ctrl+G | Closes it. |

A change applies at the next start of the shell.

### Questions and panels

| Panel | Keys |
|---|---|
| Safety question for a dangerous line | Type `y` or `yes` and Enter to run it; `t` (or `trash`) runs it with `rm` moving files to the trash, where that is possible; anything else, Enter alone, Ctrl+C and Ctrl+D cancel. |
| Safety question for a critical line | Type `yes` or the target's name; anything else cancels. There is no trash choice. |
| AI plan panel (a terminal with the line editor) | Enter or `y` runs all; `s` step by step; `e` edits a command (with several steps, a digit or `↑↓` and Enter picks which; Esc goes back); `c` chats; `v` shows the last request as it was sent (control characters removed; earlier plans and refused retries appear only as character counts); `i` shows the details of the call; Tab shows or hides more numbers; `←` `→` move between the buttons; `n`, `q` and Esc cancel; Ctrl+C cancels with `$?` 130. |
| AI plan panel (`TERM=dumb`, the plain reader, a pipe) | The text panel: Enter or `y` runs; `e` edits; `c` chats; `v` shows the last request; Esc, `n` and `q` cancel. |
| Question before one step of a plan run step by step | The steps of the plan with a bar, the count and the time so far are above it. `y` or Enter runs the step; `s` skips it; `n` and Esc stop the plan; Ctrl+C stops it with `$?` 130. |
| Question after Ctrl+Z stopped a step of a plan (a terminal with the line editor) | **Continue** (`c`, `fg`: the step may stop again and the question comes back), **Skip** (`s`: ends the step with `SIGTERM`, continuing it so that it can end, and goes on with the next step), **Stop plan** (`n`, Enter and Esc: the step stays stopped, see `jobs`; the plan ends with `$?` 148) or **To background** (`b`: `bg`; the plan ends with `$?` 0 and the step goes on). Without the panels the plan ends as for **Stop plan**, with no question. |
| End of a plan of several steps (a terminal with the line editor) | A panel with every step and its state (done, failed, paused, in the background, skipped, not run), the count and the time. |

**Note under a plan step: `no such path:`.** For `cd`, `ls`, `cat`, `less`, `head`, `tail`, `code` and `xdg-open`, a path word that does not exist gets a line such as `no such path: ../night-before-codebase; did you mean ../night-before?` (up to 3 names, taken from the folder where the path first goes wrong, and only if the corrected path exists). It is a note: the command is not changed, the level does not change, and Run still works. The file system is read only when the plan is shown, and nothing is sent. Never looked at: `mkdir`, `touch`, `cp`, `mv`, `tee`, editors and redirections. Left alone: paths known only at run time (`$dir/x`, `$(...)`), patterns, `~user`, URLs, a command with an option that may take a value (`head -n 5`, `code -g f:12`), a bare `cd` name when `CDPATH` is set, and `..` from a folder that is a link. A path is also not noted when the name that is missing is written in an earlier step or earlier in the same step (`mkdir build && cd build`), or when the file system says anything but "no such file" (for example no permission). At most 3 notes per step, and at most 4,096 names of a folder are read.
| Jobs panel (`jobs -i`) | See "The jobs panel" above. |
| "AI is not set up" (`? question` before the setup) | `s` sets the AI up and then sends the question; `n`, Esc and Enter (Not now is chosen) send nothing. |
| Lists of `omsh ai setup` | Type to filter; `↑` `↓` move; Enter chooses; Tab switches between a provider's chat models and all of its models (Gemini); Esc leaves the model list and lets you type a name. |
| Details of a call (`omsh ai details`, `i`) | `←` `→` switch provider; Enter, `q` or Esc close. |
| `omsh tour` | Enter, `n` or Space next; `b` back; `q` or Esc leaves. |

## Settings (`config.toml`)

The first start writes `~/.config/oh-my-shell/config.toml` with every setting listed and commented
out. A setting that has a default shows it; one that is unset (`ui.motion`, `ai.provider`, `ai.model`,
`ai.base_url`) shows an example value, which is not the default. A mistake in a value is reported with its line and column and the default is used.
The keys, with the default in brackets:

### `[ui]`

- `ui.theme` (`"nord"`): the theme, `"nord"` or the name of `themes/<name>.toml`. Example: `theme = "nord"`.
- `ui.motion` (unset): `"full"` (or `"on"`, the same thing), `"reduced"` or `"off"`; unset means reduced over ssh and full otherwise. Example: `motion = "off"`.
- `ui.icons` (`"auto"`): `"unicode"`, `"ascii"` or `"auto"`. Example: `icons = "ascii"`.
- `ui.style` (`"spaced"`): `"spaced"` or `"plain"`. Spaced prints a blank line and a thin rule before the prompt of every command after the first, and shows how the last command ended at the right of the header (`✓ 4 ms` or `✘ 127`). Plain is the two-line prompt of version 1.0. Example: `style = "plain"`.
- `ui.greeting` (`"auto"`): `"auto"`, `"always"` or `"never"`. The one-line reminder of keys under the first prompt of a session: `auto` shows it for the first five sessions after the welcome, `always` at every start, `never` not at all. Example: `greeting = "never"`.
- `ui.ls` (`"ls"`): `"ls"` or `"eza"`. What the word `ls` runs. Oh My Shell does not read the aliases of fish or bash, so a person who uses `alias ls=eza` there says so here. `"eza"` takes effect only while `eza` is on `PATH`: at the start of a shell, `ls` becomes an alias for `eza` (before `init.omsh`, so the file can replace it, and `unalias ls` removes it for one session). If `eza` is not found, `ls` stays the system `ls`. AI plan steps never use aliases. The first-run questions offer it when `eza` is on `PATH`; set it back to `"ls"` to undo. Example: `ls = "eza"`.

### `[editor]`

- `editor.mode` (`"emacs"`): `"emacs"` or `"vi"`. Example: `mode = "vi"`.
- `editor.hints` (`true`): faint suggestions from the history. Example: `hints = false`.
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
- `ai.context_limit` (unset): the size of the model's context window in tokens, 1 to 100000000, when you know it. It wins over what a provider says. Without it the shell asks Gemini, Anthropic and a loaded Ollama model after you have sent a request, and shows "limit unknown" for OpenAI and compatible servers. Example: `context_limit = 128000`.
- `ai.context` (a table): what may be sent along with a request, the four keys below.
- `ai.context.os` (`true`): the name of the system. Example: `os = false`.
- `ai.context.cwd` (`true`): the working directory. Example: `cwd = false`.
- `ai.context.git` (`true`): whether the folder is in a git repository, and the branch. Example: `git = false`.
- `ai.context.recent_commands` (`true`): the last 3 commands with their exit status, never their output. Example: `recent_commands = false`.
- `ai.inline` (a table): the line under each AI answer on a terminal, the two keys below.
- `ai.inline.fields` (`["time", "in", "out"]`): what the line shows, in this order: `"model"`, `"effort"`, `"time"`, `"in"`, `"out"`, `"thinking"`, `"cached"`, `"total"`, `"ctx"`, `"speed"`. A number the server did not report is shown as a dash, never as 0. Example: `fields = ["model", "time", "in", "out", "ctx"]`.
- `ai.inline.thinking_warn` (`4`): a warning when the thinking tokens are at least this many times the answer's tokens; `0` turns it off. Example: `thinking_warn = 0`.

### `[notify]`

- `notify.enabled` (`true`): a desktop notification when a long command ends (`notify-send`). Example: `enabled = false`.
- `notify.min_seconds` (`10`): how long a command must run, 1 to 86400. Example: `min_seconds = 30`.
- `notify.ignore` (editors, pagers, `ssh`, `tmux`, shells and similar): programs that never notify. Example: `ignore = ["vim", "ssh"]`.

### `[update]`

- `update.check` (`false`): ask GitHub for the latest release about once a day, after the prompt (the time is kept in a stamp file; if that file cannot be written the check can repeat at every start). The news is shown at the next start, in one line, at most once in 24 hours. `OMSH_NO_UPDATE_CHECK` turns it off; it never runs in CI. The first-start question sets it. Example: `check = true`.
- `update.channel` (`"stable"`): only `"stable"` is accepted; it is read and not used. Example: `channel = "stable"`.
