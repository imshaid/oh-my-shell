# Oh My Shell: design

How Oh My Shell is built, and why. It is written for someone who wants to read or change the code. The
[user guide](user-guide.md) says what the shell does; this page says how, with links to the source. Numbers
come from the sources named next to them.

Contents: [Shape of the program](#shape-of-the-program) · [One typed line](#one-typed-line-end-to-end) ·
[Processes, threads and the fork path](#processes-threads-and-the-fork-path) · [The terminal and signals](#the-terminal-and-signals) ·
[The language front end](#the-language-front-end) · [Execution](#execution) · [History](#history) ·
[Safety](#safety) · [The AI layer](#the-ai-layer) · [The screen](#the-screen) · [Configuration](#configuration) ·
[Release and update](#release-and-update) · [Testing](#testing) · [Measured numbers](#measured-numbers)

## Shape of the program

A Cargo workspace of eleven crates under `crates/`. About 131,200 lines of Rust, of which about 18,000 are the end-to-end
test harness (`git ls-files '*.rs' | xargs cat | wc -l`).

| Crate | Responsibility | Lines |
|---|---|---|
| [`omsh-syntax`](../crates/omsh-syntax/src/lib.rs) | Lexer, parser, syntax tree with byte spans, diagnostics. Depends on nothing internal. | 10,160 |
| [`omsh-exec`](../crates/omsh-exec/src/lib.rs) | Expansion, executor, builtins, jobs, signals, the terminal, the variable table. The only crate with `unsafe`. | 19,507 |
| [`omsh-config`](../crates/omsh-config/src/lib.rs) | Settings schema, loading with line and column errors, XDG paths, the first-run file, in-place editing. | 3,963 |
| [`omsh-secrets`](../crates/omsh-secrets/src/lib.rs) | Finds and masks secret-looking text. One rule set for history and for AI context. | 833 |
| [`omsh-history`](../crates/omsh-history/src/lib.rs) | The SQLite history, its writer thread and its queries. | 3,575 |
| [`omsh-safety`](../crates/omsh-safety/src/lib.rs) | Rule table and analyzer, blast radius, trash, undo journal. | 7,765 |
| [`omsh-ai`](../crates/omsh-ai/src/lib.rs) | Provider layer, key storage, plan schema, the lazy runtime, the HTTP download used by `omsh update`. | 7,096 |
| [`omsh-ui`](../crates/omsh-ui/src/lib.rs) | Theme tokens, degrade rules, motion, full-screen views. | 6,995 |
| [`omsh-editor`](../crates/omsh-editor/src/lib.rs) | The line editor (reedline): prompt, highlighter, hinter, completer, validator. | 11,605 |
| [`omsh-cli`](../crates/omsh-cli/src/lib.rs) | The `oh-my-shell` program: arguments, modes, wiring, the AI flows, `omsh update`, `omsh uninstall`. | 22,124 |
| [`omsh-tests`](../crates/omsh-tests/src/lib.rs) | End-to-end tests in a pseudo-terminal, and the performance programs. Development only. | 13,982 |

Which crate may use which (from the `[dependencies]` of each crate's `Cargo.toml`), with no cycles:

```
cli    -> editor, exec, ui, ai, safety, history, config, secrets, syntax
editor -> syntax, history, ui, config
exec   -> syntax, config
safety -> syntax
ai     -> config, secrets
history-> secrets
ui     -> config
tests  -> cli, history (development only)
```

The direction does real work. `omsh-exec` may not use the history, so the `history` builtin is a trait
(`HistoryCommand`) that the program implements and hands to the shell. The same pattern serves `j`, `undo` and `omsh`.
`omsh-ai` may not use the history either, so the program masks text with `omsh-secrets` before handing it over.
Gate C (below) depends on the network code being in one crate.

## One typed line, end to end

1. **Read.** `omsh-editor` reads a line with reedline. Highlighting uses `omsh-syntax`; hints use the history. A validator
   asks the parser whether the line is finished, so Enter waits for more after an open quote.
2. **Trigger check.** A line that starts with `?` or `??`, or ends with `| ?`, goes to the AI flow. This comes before parsing,
   so `? what's here` (an open quote) is not a syntax error.
3. **Aliases** are replaced on the text, then the line is **parsed**. A syntax error, or syntax the shell does not support,
   prints a diagnostic with the place marked and runs nothing.
4. **Safety.** The analyzer walks the tree and gives a level. A line at or above `safety.confirm` shows its panel and asks
   ([`confirm.rs`](../crates/omsh-cli/src/confirm.rs)).
5. **Expansion and execution** in `omsh-exec`: builtins in the shell's own process, programs in a child process in a
   process group of their own.
6. **Record.** The command, folder, exit status and duration go to the history writer (secrets and ignored patterns are
   filtered first).
7. **Prompt** is drawn again with the status and the duration.

The loop is in [`repl.rs`](../crates/omsh-cli/src/repl.rs). Modes are chosen in
[`lib.rs`](../crates/omsh-cli/src/lib.rs): `-c`, scripts and piped input replace the process with `/bin/sh` by `execv`
before anything is read ([`handoff.rs`](../crates/omsh-cli/src/handoff.rs)), so `ssh host cmd` and `scp` never meet our
code.

## Processes, threads and the fork path

**Why threads matter.** In a program with several threads, only the calling thread exists in the child after `fork`. A
lock held by another thread stays locked in the child forever, and so does the allocator's. So the child may only call
functions that are safe in a signal handler, and then `execve`. [`spawn.rs`](../crates/omsh-exec/src/spawn.rs) prepares
every string and pointer array before the fork. The child then calls `setpgid`, `tcsetpgrp` (foreground), puts signal
dispositions back, applies the redirections with `dup2`, sets `umask`, and calls `execve`. No allocation, no locks, no
logging.

**How the parent learns that `execve` failed.** A pipe with `O_CLOEXEC` is made before the fork. If `execve` works, the
pipe closes by itself and the parent reads end-of-file. If it fails, the child writes the error number and exits. That
gives the right status (126 or 127) without guessing.

**Pipelines are threads, not forks.** A pipeline stage is a thread that runs on a copy of the shell and forks only when it
reaches a program ([`jobs.rs`](../crates/omsh-exec/src/jobs.rs), [`exec/pipeline.rs`](../crates/omsh-exec/src/exec/pipeline.rs)).
A stage such as `{ sleep 1; cat; }` starts its programs late, and different stages start theirs from different threads, so
the job is a shared object that says which process group to join. The first program becomes the leader and makes the group;
later ones join it. The leader is kept as a zombie until the job ends, so the group still exists for late joiners and its
process id cannot be reused. Nobody calls `waitpid(-1)`, which would steal the children of other threads.

**Command substitution** (`$(...)`) runs on a copy of the shell with its standard output on a pipe, and a second thread
reads the pipe while the commands run, because a program that prints more than the pipe holds (64 KiB) would otherwise wait for
a reader that is waiting for it ([`expand/mod.rs`](../crates/omsh-exec/src/expand/mod.rs)).

**Other threads, all started on purpose** (the history writer, hint ranking and git work are started while the first
prompt is being set up, before it is drawn; the rest later):

| Thread | Why |
|---|---|
| `history-writer` | Writes to SQLite so the prompt never waits for the disk. |
| hint ranking | One long-lived thread that ranks history hints for the current folder. |
| git status | The prompt's git segment, with a 150 ms limit; `git` is killed and collected after that. |
| stopped-job waiter | A small thread that calls `waitpid` for a stopped program when the main thread is blocked. |
| `update-check` | The daily release check, only when `update.check = true`. |
| notification | Runs `notify-send` for a long command. |
| key reader | Reads keys for a full-screen view, so a closed terminal ends the shell. |
| AI | None. A `current_thread` tokio runtime is built on the calling thread at the first AI request and has no worker threads. |

## The terminal and signals

[`terminal.rs`](../crates/omsh-exec/src/terminal.rs) and [`signals.rs`](../crates/omsh-exec/src/signals.rs) follow the usual
rules for an interactive shell:

- The shell puts itself in its own process group and takes the terminal with `tcsetpgrp`. It ignores `SIGTTOU` first,
  or `tcsetpgrp` from the background would stop the shell itself.
- A foreground job gets the terminal and Ctrl+C reaches its group only. The shell catches `SIGINT` with a handler that
  only sets a flag, and ignores `SIGQUIT`, `SIGTERM`, `SIGTSTP`, `SIGTTIN` and `SIGTTOU`. `SIGHUP` is caught: the handler only sets a flag, and the jobs are hung up and the shell ends when the code next looks
  at it. During a foreground wait that is after the foreground program ends.
- The terminal modes (termios) are saved before a job runs. After a job that was stopped or killed by a signal the saved
  modes are put back, so a program that dies in raw mode does not leave the terminal broken; after a job that ends normally
  the modes it left are kept (a `stty -echo` stays). A panic hook restores the terminal too.
- Ctrl+Z stops the job, which is kept as stopped. The state of a job is its own record (running, stopped, finished); the
  state "continued" is recorded before `SIGCONT` is sent, because writing it after erased a stop that happened in between (a bug found and fixed
  in the job-control tests).
- Signals ignored when the shell was started stay ignored for the programs it starts (read with `sigaction` before any
  change); the exception is `SIGPIPE`, which every child gets back at its default.

Full-screen views ([`omsh-ui/src/screen.rs`](../crates/omsh-ui/src/screen.rs)) use the alternate screen and raw mode, and a
panic hook gives the terminal back before the message is printed.

## The language front end

[`omsh-syntax`](../crates/omsh-syntax/src/lib.rs) has three parts.

- **Lexer.** Every token carries its byte span. `lex_for_highlight` never builds a tree and is what the highlighter calls on
  each key.
- **Parser.** It builds a tree in which every node has a span. It has two modes: *strict* for execution and *tolerant* for
  half-typed lines. The highlighter uses `lex_for_highlight`; the validator calls the strict parser and looks at what kind
  of error it gives. Constructs the shell does not support (arrays, `[[ ]]`,
  process substitution, ...) are detected and reported as one clear diagnostic ([`miette`](https://crates.io/crates/miette)).
- **Round trip.** A printer turns a tree back into text; a property test checks that printing and parsing again gives the
  same tree on generated input.

## Execution

[`omsh-exec`](../crates/omsh-exec/src/lib.rs) holds the variable table (the environment is built for each `exec` from
it), the expansion in POSIX order (tilde, parameters, command substitution, arithmetic, field splitting, pathname expansion
by its own `glob`, quote removal), the builtins, and the executor.

**Redirections** use a table of files per command ([`io.rs`](../crates/omsh-exec/src/io.rs)). The shell cannot change its
own descriptors for `2>&1` or `3>file`, because other pipeline stages on other threads need the originals. A builtin writes
to its table directly. A program gets the table applied by the child with `dup2` just before `execve`, from a plan built
before the fork. Here-documents are written to an anonymous memory file (`memfd_create`), which has no size limit like a
pipe and needs no helper process ([`redirect.rs`](../crates/omsh-exec/src/redirect.rs)).

**Login environment.** A login shell runs `sh -l -c 'env -0 >&3'` and merges the result, so the profile files are
interpreted by a real `sh` and never parsed by us ([`login.rs`](../crates/omsh-exec/src/login.rs)).

## History

[`omsh-history`](../crates/omsh-history/src/lib.rs) keeps a SQLite file in WAL mode (`synchronous = NORMAL`), so several
shells read and write at once. Writes go through a channel to one writer thread. The shell never waits for it: the
history file may appear a moment after the first prompt, and the `history` builtin waits up to two seconds for it. Reads
use their own connections with small page caches (256 KiB), which keeps the idle memory low even with a 100,000-row
history. Hints are ranked in memory from a 64-bit hash and three counters per command, and the text of only the best 5,000
is read. Lines that match `history.ignore`, or that `omsh-secrets` finds a secret in, are never written.

## Safety

[`omsh-safety`](../crates/omsh-safety/src/lib.rs) is an offline analyzer on the syntax tree.

- **Rules** are data: a table in [`rules.rs`](../crates/omsh-safety/src/rules.rs) with an id, a matcher, a level, a plain
  reason, and whether a trash alternative exists. Twenty-two rules, for example `rm-critical-path`, `rm-recursive-or-force`,
  `rm-glob`, `find-delete`, `dd-to-device`, `format-or-wipe`, `chmod-chown-system-recursive`, `fork-bomb`, `firewall-open`,
  `download-to-shell`, `git-force-push`, `git-reset-hard`, `overwrite-file`. Each has tests for lines that match and lines
  that must not.
- **Levels** are safe, caution, dangerous, critical. A `sudo` in front raises a command that is already flagged one step.
- **Blast radius** ([`walk.rs`](../crates/omsh-safety/src/walk.rs), [`blast.rs`](../crates/omsh-safety/src/blast.rs)): expands
  the targets and walks them with two limits (entries and milliseconds). When a limit stops the walk, the panel says "at least".
- **Trash** ([`trash.rs`](../crates/omsh-safety/src/trash.rs)): a move is a `rename`. Across file systems (`EXDEV`) it is a
  copy, a check that the copy is the same, and only then the removal. **Undo journal**
  ([`journal.rs`](../crates/omsh-safety/src/journal.rs)): a SQLite file with one row for each recorded action, so `undo`
  can put files back with their permissions.

## The AI layer

[`omsh-ai`](../crates/omsh-ai/src/lib.rs) is the only crate that opens network sockets.

- **Rules.** Off until set up; nothing on the normal path; every plan is shown and confirmed first, and each command goes through the parser, the analyzer and the
  executor like a typed one; the higher of the analyzer's and the model's level applies to an unedited step, the analyzer's
  alone to an edited one.
- **Lazy start.** No thread, no network socket and no runtime exist before the first AI request (`omsh update` and the daily check also build the runtime when they run)
  ([`runtime.rs`](../crates/omsh-ai/src/runtime.rs)). The runtime has no worker threads; a request is driven from the
  calling thread, which also looks at the Ctrl+C flag every 50 ms.
- **Providers.** A `Provider` trait with an implementation through [`genai`](https://crates.io/crates/genai) for Ollama,
  Anthropic, OpenAI, Gemini and OpenAI-compatible servers. A provider (and its connection) is made for each request and
  dropped at the end, so no socket stays open while a panel waits for a key. Keys are kept in the system keyring or a file with
  mode 0600, held in wiping types where we hold them (`zeroize`; copies inside the genai library and some plain strings are not wiped), and sent only to `https://` addresses and to this computer.
- **Plans** are JSON with a summary and 1 to 8 steps (title, command, explanation, risk, `needs_root`). Each command must
  parse with our parser, may not hold placeholders like `/path/to/...`, and is checked again just before it runs. A bad plan gets
  one retry with the error; then a clean message.
- **Privacy.** The request text and the recent commands are masked with `omsh-secrets`; `[v]` in the panel shows the last request as it was sent (earlier plans and refused retries only as character counts); each context item can be switched off.
- **Gate C.** A test runs the shell under `strace -f` and fails on any socket that is not a local Unix socket during a
  normal session ([`offline.rs`](../crates/omsh-tests/tests/offline.rs)). CI also runs the whole test suite with the network
  switched off ([`scripts/test-offline.sh`](../scripts/test-offline.sh)).
- **Fake providers.** AI tests use a fake provider and a fake HTTP server on the loopback; no test needs a real model.

## The screen

[`omsh-ui`](../crates/omsh-ui/src/lib.rs) draws everything the shell shows, in layers that can each be tested without a
terminal.

- **Theme.** The colors are tokens and roles made only of the 16 ANSI color names, so a terminal's own theme decides the
  look ([`theme.rs`](../crates/omsh-ui/src/theme.rs)). The token `muted` is the terminal's own text color, for secondary text
  that carries information (labels, counts, timers). The token `dim` is faint (SGR 2) and only for decoration: a rule, a
  separator, the empty part of a bar. A test fails if a faint span or cell holds a letter or a digit, because faint text
  is hard to read on some terminal themes.
- **What the terminal allows.** One pure function ([`caps.rs`](../crates/omsh-ui/src/caps.rs), [`look.rs`](../crates/omsh-ui/src/look.rs))
  decides color, Unicode and motion from the environment (`NO_COLOR`, `TERM`, the locale, ssh, CI) and from which of
  standard input, output and error are terminals. The rules are tested without a terminal.
- **Panels are data.** A panel is `render(state, caps)`, which gives lines of styled segments (a text and a tone such as
  `Danger`, `Ok`, `Ai`, `Key`, `Muted`). [`text.rs`](../crates/omsh-ui/src/text.rs) is the only code that writes an escape
  sequence: it maps a tone to a theme token and the token to SGR, so a panel never names a color. Panels that show text we
  fully control have a box on all sides; panels that show a file name or a command have an open right edge (a left rail
  and rules), because the width of Bangla or combining text cannot be trusted ([`boxes.rs`](../crates/omsh-ui/src/boxes.rs)).
- **Widgets and the menu driver.** Buttons, a filterable list and the numbered fallback for where nobody can press keys
  ([`widgets/`](../crates/omsh-ui/src/widgets/buttons.rs)); the inline driver draws a menu below the prompt in raw mode, reads
  keys on a thread of its own, redraws only the lines that changed and erases itself on exit, so the scrollback stays clean
  ([`inline.rs`](../crates/omsh-ui/src/inline.rs)). It does not use the alternate screen.
- **Synchronized output.** Before the first menu the shell asks the terminal about DEC mode 2026 (`CSI ? 2026 $ p`) with a
  limit of 150 ms and remembers the answer, and wraps each redraw in the mode when it is supported
  ([`sync.rs`](../crates/omsh-ui/src/sync.rs)).
- **Motion** is our own small module: easing, a frame clock, the spinner and the step markers; nothing moves on a pipe,
  in `-c` or in CI, and `ui.motion` or `OMSH_MOTION` choose how much ([`motion.rs`](../crates/omsh-ui/src/motion.rs)).
- **The panels** ([`panels/`](../crates/omsh-ui/src/panels/mod.rs)): "Run it?", the `undo` picker, the AI plan and its step
  questions, the info line and the details of a call, the setup lists, the first-run questions, `omsh status`, the jobs
  table and the panels of a running plan. The look of the prompt (the rule, the status chip, the hint bar, the OSC 133
  marks) is [`shell_look.rs`](../crates/omsh-ui/src/shell_look.rs).
- **The scene on the website.** [`docs/index.html`](index.html) shows two captures of the 1.1.0 release build of commit f5ed352
  (`cargo build --release`), run in tmux in a Debian 12 container with the network off, through the host's own C library and
  loader (the container's was older than the one the file was built against). They are `tmux capture-pane -e -p` at two window
  widths, turned into rows by a script that only turns color codes into classes and adds no character. The Run it? panel was open
  when the screen was read, so no cursor is drawn.
- **Full-screen views.** The history explorer, the palette and the settings view share one loop
  ([`view/run.rs`](../crates/omsh-ui/src/view/run.rs)) built on `ratatui` on the alternate screen, with its own fuzzy matcher
  ([`fuzzy.rs`](../crates/omsh-ui/src/fuzzy.rs)).

## Configuration

[`omsh-config`](../crates/omsh-config/src/lib.rs) loads `config.toml` into typed settings with defaults. Every error carries a
line and column; a wrong value falls back to its default and the rest of the file applies. The first-run file is a template
in which each setting is a commented line, and a test removes the `#` of each commented setting line (not of `# ` text comments) and checks that the file means
exactly the defaults; unset settings are shown as commented examples. Changing a setting from the settings view edits that one key in place (temporary file, rename, mode kept).

## Release and update

[`release.yml`](../.github/workflows/release.yml) builds on Ubuntu 22.04 (x86_64 and aarch64) so the program needs glibc
2.35 at most. [`scripts/check-binary.sh`](../scripts/check-binary.sh) fails the build if the binary needs a newer glibc symbol
or links a library other than libc, libm, libgcc_s, libpthread, libdl and the loader. The release has tarballs, `.deb` packages
(`cargo-deb`), `install.sh` and `sha256.sum`; a draft release is made public after the job downloads the archives and packages again and checks them; `install.sh` is
added after that check and is not part of it.

`omsh update` ([`update/`](../crates/omsh-cli/src/update/mod.rs)) finds the latest tag from the redirect of
`github.com/<repo>/releases/latest` (no API call, so no API rate limit), downloads the archive, compares it with
`sha256.sum` **before** opening it, reads the one file from the tar with its own reader, writes it next to the old program,
runs `--version` on it, and renames it over the old one (a rename is atomic; running shells keep the old inode). It refuses
a copy under `/usr` (not `/usr/local`, which is the installer's `--system` target) or in the cargo bin folder. The checksum catches a bad download; it does not authenticate the release, which is
trusted because it comes from GitHub over https.

## Testing

| Kind | Where | What it checks |
|---|---|---|
| Unit tests | in each crate | Lexer, parser, expansion, builtins, rules, matcher, config, and so on. |
| Snapshot tests | `omsh-syntax/tests` (insta) | The tree for each construct. |
| Panel snapshots | `omsh-ui/tests` (insta) | Each panel drawn in the looks a terminal can give (normal, `NO_COLOR`, ASCII, plain numbered text without a terminal, Bangla file names), with the escape codes shown as text. Every snapshot also fails if a faint (SGR 2) run holds a letter or a digit. |
| No-RGB guard | [`omsh-ui/tests/palette_only.rs`](../crates/omsh-ui/tests/palette_only.rs) | No source file holds an RGB or 256-color escape, and every SGR parameter that the output layer writes, for every tone, attribute and look, is from the 16-color palette. |
| Property tests | `omsh-syntax/tests`, `omsh-safety`, `omsh-secrets` (proptest) | No panics, spans in bounds, print and parse round trip. |
| Conformance | [`omsh-cli/tests/conformance.rs`](../crates/omsh-cli/tests/conformance.rs) | The same input through this shell and through `/bin/sh` gives the same output and status. |
| End-to-end | [`omsh-tests/tests/`](../crates/omsh-tests/tests/ai.rs) | The shell in a pseudo-terminal: Ctrl+C, Ctrl+Z, `fg`, `bg`, terminal restore, hints, completion, the views, update and uninstall in a sandbox. The pseudo-terminal harness gives every wait a deadline and has a watchdog for leftover processes; ordinary unit tests have no time limit. |
| Network off | `scripts/test-offline.sh`, CI job "Test with the network off" | The whole suite with the network disabled. |
| Performance | `scripts/perf-gate.sh`, CI job "Performance gate" | The budgets below. |
| Dependencies | `cargo deny` in CI | Licenses, bans and sources block; advisories only warn. |

The end-to-end tests that touch real files (`omsh update`, `omsh uninstall`) run only through a sandbox that refuses any
home or `XDG_*` folder that is the real home or inside it.

CI run 43894149788 on `main` (commit 1df718f, the commit before the version bump to 1.0.0) was green in all six jobs, so every
test passed there. A local `cargo test --workspace --locked` on the same source ran 2469 tests (2444 passed, 1 ignored); the 25 that
failed all compare with `/bin/sh`, which on that machine is bash, not dash (CI has dash).

For the 1.1.0 source (commit 620ae39, version changed) a local `cargo test --workspace` ran 3,022 tests: 2,995 passed, 2 were
ignored and 25 failed, the same 25 as on the commit before (all compare with `/bin/sh`, which is bash here). CI has not run
on this version yet.

## Measured numbers

From the "Performance gate" job of CI run 43894149788 on `main` (commit 1df718f, the commit before the version bump to 1.0.0, an
`ubuntu-24.04` runner).
Start-up numbers are for a warm file system cache.

| What | Measured | Budget |
|---|---|---|
| `oh-my-shell --version` (median) | 0.62 ms | 10 ms |
| `oh-my-shell -c true` minus `/bin/sh -c true` | 0.55 ms | 5 ms |
| Start to first prompt, no repository | 1.63 ms | 30 ms |
| Start to first prompt, repository, real `git` | 2.57 ms | 30 ms |
| Start to first prompt, repository, `git` that never answers | 21.72 ms | 30 ms |
| Idle memory (RSS) at the prompt | 9.91 MB | 15 MB |
| Idle memory with a 100,000-row history | 11.10 MB | 15 MB |
| Idle memory, 100,000 rows, newest 20,000 all different | 11.95 MB | 15 MB |
| Keystroke to end of redraw, worst p99 (100- and 499-character lines) | 0.44 ms | 5 ms |
| First Tab (completion menu), worst p99, 20,000 programs on `PATH` | 8.1 ms | 50 ms |
| History lookups on 100,000 rows, worst p99 | 8.74 ms | 20 ms |

Other sizes: the release binary was 13,324,912 bytes in a local release build of the last measured source before the 0.1.1
fix (fat LTO, one code-generation unit, stripped); the v0.1.1 archives are 6.4 MB (x86_64) and 6.0 MB (aarch64); the v1.0.0 archives are 6,357,232 bytes (x86_64) and
5,983,796 bytes (aarch64).

Not measured: a cold disk cache, the cost of `git status` in a very large work tree, and anything on aarch64 hardware.

**Version 1.1.0 against 1.0.0, one machine, local (not CI).** Release builds of the tagged commit 8323395 and of the 1.1.0 source
(commit 620ae39 with the version number changed), the median of three rounds in which the builds were measured in turn;
`hyperfine` 2.0.0 (20 warm-up and 300 runs) for the first rows and `omsh-perf` for the others. The 1.1.0 rows are not CI
numbers: the performance gate has not run on this version yet, and the benchmarks of the completion menu, the history
lookups, the explorer, the palette and the settings view were not run here.

| What | 1.0.0 | 1.1.0 | Budget |
|---|---|---|---|
| `oh-my-shell --version` (median) | 0.71 ms | 0.71 ms | 10 ms |
| `oh-my-shell -c true` minus `/bin/sh -c true` | 0.69 ms | 0.67 ms | 5 ms |
| Start to first prompt, no repository | 2.11 ms | 1.88 ms | 30 ms |
| Start to first prompt, repository, real `git` | 3.36 ms | 3.22 ms | 30 ms |
| Start to first prompt, repository, `git` that never answers | 22.28 ms | 22.22 ms | 30 ms |
| Idle memory (RSS) at the prompt | 9.49 MB | 9.78 MB | 15 MB |
| Idle memory with a 100,000-row history | 10.75 MB | 10.99 MB | 15 MB |
| Idle memory, 100,000 rows, newest 20,000 all different | 11.40 MB | 11.50 MB | 15 MB |
| Keystroke to end of redraw, worst p99 | 0.72 ms | 0.68 ms | 5 ms |
| Slowest key of all | 1.24 ms | 1.09 ms | 25 ms |
| Release binary | 13,438,568 bytes | 13,864,936 bytes (+3.2%) | |

The rounds differ by up to about 10% from each other (for example the start with no repository was 2.20, 1.99 and 2.11 ms
for 1.0.0), so differences of a few percent are noise.
