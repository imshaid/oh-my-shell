# Changelog

All notable changes to Oh My Shell. The newest version is first. Settings and commands are described in
[docs/commands.md](docs/commands.md); how to use them in [docs/user-guide.md](docs/user-guide.md).

## 1.1.1 (not released yet)

Plans from the AI are checked more before they run. No new settings, and still no network use on the normal
command path.

### The AI plan

- **A step whose background job fails is a failure.** A step such as `cmd &>/dev/null &` returns 0 as soon as the
  job starts, so the plan used to say "plan done" and the job's `Exit 127` only showed up later, in the notice
  before the next prompt. Now, after a step that returned 0, the plan watches the jobs that step started for up
  to 300 ms. A job that ends with a nonzero status or a signal makes the step fail: the plan prints
  `background job [1]: Exit 127` (or the signal name, such as `Terminated`) and then
  `step 2 did not succeed (status 127)`, and the steps after it do not run. A job that is still running after
  the wait is left alone (it may be a server), and a step that started no job does not wait.
- **Commands that are not found are noted before anything runs.** The plan panel puts `not found: name` under a
  step whose command is not a keyword, builtin, function or program in `PATH`, with the same "did you mean"
  names as a typed mistake. It is only a note: the level does not change and Run still works, because an
  earlier step may install the program. Aliases do not count, as plan steps run without them.
- **A missing command is fixed from the desktop entries.** When exactly one installed program fits the name,
  the command word is replaced before the panel is shown (`google-chrome` becomes
  `/usr/bin/google-chrome-stable`) and the panel says `fixed: old -> new (App Name)` in place of `not found:`.
  You still see the corrected plan and confirm it. Arguments, `&` and parentheses are kept. The `.desktop` files
  are read locally, only when a command is missing, and nothing is sent anywhere. The freedesktop rules are
  followed: `$XDG_DATA_HOME` and `$XDG_DATA_DIRS`, the `[Desktop Entry]` group only, and `Hidden` or a missing
  `TryExec` leaves an entry out. An entry whose ID or `Name` says the name comes before one that only runs a
  program of that name (a web-app shortcut that runs the browser); two different programs in the best group stay
  a plain `not found:`, and a fix found only through a program name shows no application name. Entries that run
  `flatpak`, `env` or a shell are not used. With no entry folders (a server, a container, WSL) nothing happens.
  The same fix is applied when you edit a step.

### Known limits

- A background job started inside a subshell, `( cmd & )`, is not checked: the job belongs to the subshell, not
  to the shell, so the plan cannot see it fail.
- A background job that fails after the 300 ms watch is not seen by the plan; its `Exit` notice still appears
  before the next prompt.
- Path arguments that do not exist are not checked, and neither are commands written as a path.

## 1.1.0 (2026-11-18)

A new look for the interactive parts. Nothing about how the language runs commands changed, and there is still
no network use on the normal command path.

### What you see while you use the shell

- **The prompt.** `ui.style = "spaced"` is the new default: a blank line and a thin rule between commands, the
  folder in blue, and at the right of the header how the last command ended (`✓ 4 ms` or `✘ 127`). While the line
  you type is a request to the AI, an `ask AI` tag shows. `ui.style = "plain"` is the two-line prompt of 1.0.
- **While you type.** A command that exists is green and one that does not is red; options, quotes, paths and
  operators have their own colors. Only the 16 ANSI colors are used, so your terminal theme decides how they look.
- **The hint bar.** One line above the first prompt names the keys worth knowing. It is shown for the first five
  sessions and then never (`ui.greeting`).
- **Terminal marks.** On a terminal the shell writes the OSC 133 marks, so that terminals that understand them
  (kitty, WezTerm, iTerm2) can jump between prompts.
- **`Ctrl+P` and `Ctrl+R`.** The palette, the history explorer and the settings view show names and numbers at
  full weight (see "Fixed" below), and the settings view says "view only" for a setting it cannot change.

### Panels that ask a question

- **Run it?** A dangerous line opens a panel under the prompt with the reasons, what the command would touch and
  the buttons **Move to trash**, **Run anyway** and **Cancel**. Cancel is chosen when it opens. `t`, `y` and `n`
  choose at once. A critical line has no buttons: you still have to type `yes` or the target.
- **`undo` picker.** `undo` with no number lists what can be undone. Enter opens a **Restore** / **Cancel**
  question; it never restores at once.
- **Explain hint.** After a failure, and only when an AI provider is set up, the prompt shows `Alt+? explain`.
- **The AI plan.** A panel with a risk word for each step and the buttons **Run all**, **Step by step**, **Edit**,
  **Chat** and **Cancel**. Run all is chosen when it opens, so Enter runs the plan as before.
- **Pausing a plan.** Ctrl+Z on a running step asks **Continue**, **Skip**, **Stop plan** or **To background**
  (Stop plan is chosen). A plan of two or more steps ends with an overview of every step.
- **Jobs.** `jobs -i`, or "Jobs" in `Ctrl+P`, shows the jobs as a table: Foreground, Background, Stop or Kill
  (Kill asks first). Plain `jobs` is unchanged.
- **Without a terminal.** Where nobody can press arrow keys (a pipe, `TERM=dumb`, CI, `-c`) every question is the
  plain text it was in 1.0, with the same answers.

### The AI

- **What a call cost.** Under each answer a line shows the time and the tokens the server reported (`time · in ·
  out`, configurable with `ai.inline.fields`). A number the provider does not report is shown as `–`, never as 0.
  `omsh ai details`, or `i` in the plan panel, shows a table for the last call with the context window when the
  provider says what it is.
- **Setup.** The provider and model lists can be filtered by typing. For Gemini the list shows only the models that
  can chat; Tab shows all of them.
- **Not set up.** `? question` with no provider set up opens a small panel (**Set up AI now** / **Not now**)
  instead of an error line.

### Install, first start and updates

- **The installer** (`scripts/install.sh`) shows a spinner for each step, the real download progress, and the time
  the new program took to start, measured. It prints the exact `PATH` line for your shell when `~/.local/bin` is
  not in `PATH`; it still edits no startup file.
- **First start**, once and only on a terminal: a welcome, "Stay up to date?" (the default answer is **No**), and,
  when `eza` is installed, "Use eza for ls?" (`ui.ls = "eza"`; the default answer is **Not now**).
- **Update news.** With `[update] check = true`, the next start shows one line when a newer release exists, at
  most once in 24 hours. With the check off, a release build older than 60 days says so once. Neither uses the
  network to show the line. `OMSH_NO_UPDATE_CHECK=1` turns both off.
- **`omsh status`** shows the version, the build age, whether AI is set up, whether the daily check is on, the
  motion mode and what the terminal can do.

### New settings

`ui.style`, `ui.greeting`, `ui.motion`, `ui.ls`, `ai.inline.fields`, `ai.inline.thinking_warn`. `ui.motion` and
`OMSH_MOTION` choose how much moves (`full`, `reduced`, `off`); nothing moves on a pipe, in `-c` or in CI.

### Fixed

- **Faint text.** Some terminal themes draw "bright black" almost like the background, and faint text on top of it
  could not be read. Text that carries information (descriptions in the palette, counters like `55 of 55`, footer
  words, timers, the line under an AI answer, labels in panels) is now drawn in your terminal's own text color at
  full weight. Faint is left for decoration only: rules, separators and the empty part of a bar. Where dimness
  alone used to tell two groups apart, a position or a `·` does it now, and an action that does not fit a job is
  written in parentheses. The suggestion after the cursor is faint and nothing else (it was faint bright black
  before). In `ui.style = "plain"` the virtual environment, status, duration, time and git text are plain text
  next to the bold folder, no longer faint. The comment colors are unchanged.
- A report said a stopped job that ends through the jobs panel's Kill, or when the shell exits, is shown as
  `Done`. It could not be reproduced: the table says `Exit 9` or `Terminated`, as it should. Three tests now pin
  that. If you see it, the exact steps would help.

### Not done, known limits

- `undo 3` and `cmd | ? question` still ask in plain text, and there is no `omsh changelog` command, so the update
  line does not say what is new.
- **No live progress while a plan step runs.** The step owns the terminal and the shell waits for it, so there
  is no spinner and no running clock; the panels appear where the shell has the keyboard.
- Not tested: the plan panel and the pause panel with a real AI key (only the fake provider and the fake server
  were used), and the panels inside tmux or over ssh, apart from the Run it? panel in tmux for the website.
- `ui.style = "blocks"` of the design notes was not built.
- aarch64: the files are built and smoke-tested on a CI runner only, as for 1.0.

## 1.0.0 (2026-11-17)

The first stable release: the POSIX-compatible shell language, job control, the line editor with highlighting,
hints and completion, the SQLite history, the safety analyzer with trash and `undo`, the optional AI layer,
`omsh update` and `omsh uninstall`, the installer, `.deb` packages and the documentation.

## 0.1.1 (2026-11-13)

`scripts/install.sh` used the glibc number as the release (a helper function overwrote a variable), so 0.1.0
could not install without `--base-url`. Fixed, with a test; the latest release is now found from the redirect of
`releases/latest` and not from the API.

## 0.1.0 (2026-11-13)

The first release. Superseded by 0.1.1 because of the installer problem above; the release and the tag are kept.
