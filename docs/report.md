# Report material: Oh My Shell

This is raw material for a written report, not a finished report. Every fact in it comes from the repository, a CI log
or a local run named next to it; statements that are not about this project are marked **general knowledge**. The
details of the design are in [design.md](design.md), the use of the program in [user-guide.md](user-guide.md).

Report header (fill in):

- Title: `<TITLE>`
- Author: `<AUTHOR NAME>`
- Course: `<COURSE>`
- Instructor: `<INSTRUCTOR>`
- Date: `<DATE>`

## 1. Problem and goals

An interactive Linux shell is the program people use most to run other programs, and most shells ask for a trade-off:
a stable POSIX-style language with little help while typing, or a friendlier interactive experience with a language of its
own. Oh My Shell is an attempt at both and at one more thing: letting an AI help with a task without letting it act
unseen.

Goals the project set itself (they are the headings of [user-guide.md](user-guide.md)):

1. **Behave like a POSIX shell** for the common language, and do not break `ssh host cmd`, `scp` and scripts.
2. **Help while typing:** syntax colors, history hints, completion, a searchable history, a command palette.
3. **Check before harm:** analyze each typed line offline, show what a dangerous command would touch, offer a trash and an
   undo.
4. **Optional, explicit AI:** off until set up, never on the normal command path, every suggested command shown and
   confirmed, secrets masked.
5. **Fast and small:** budgets for start-up, memory and keystroke latency, checked in CI (section 6).

Non-goals: full bash compatibility; being a terminal emulator; Windows or macOS.

## 2. Method

- **Small steps with checkable endings.** The work was split into segments, each with a "done when" condition, a
  green format, lint and test run, and one commit. Hard segments were planned first.
- **Gates.** Three project gates were defined: a daily-use gate (the shell is used for real), a performance gate (every budget
  met and enforced in CI) and an offline gate (no network on the normal path, the full test suite passing with the network
  off). The performance and offline gates are enforced by CI jobs. **The daily-use gate was waived by the owner:** instead of a
  week of daily use there was a short manual smoke test (basic commands, job control, a full-screen editor, creation of the
  config file). The week did not happen.
- **Continuous integration.** Format, Clippy with warnings as errors, tests (including the pseudo-terminal tests and an
  `ssh`/`scp` login-shell test), tests with the network off, a performance gate, and `cargo deny` for licenses.
- **Rules for risk.** `unsafe` is allowed only in the process-spawning and signal code of `omsh-exec`, each block with a
  `SAFETY` comment, enforced by a workspace lint; `unwrap` and `expect` are denied outside tests; every bug fix gets a
  regression test; a new dependency needs a recorded reason and must pass `cargo deny`.

## 3. Implementation summary

Eleven crates, about 107,000 lines of Rust (`git ls-files '*.rs' | xargs cat | wc -l`); see the table in
[design.md](design.md#shape-of-the-program). Main components: a lexer and parser with spans; an executor that forks, execs and
manages process groups; a line editor built on reedline; a SQLite history; a rule-based safety analyzer with blast radius,
trash and undo; an AI layer with several providers; full-screen views; and a release pipeline with its own installer and
updater.

### Which feature uses which operating-system concept

Each row was read from the code: the system call or mechanism, then the file where it lives.

| Shell feature | Operating-system concept | Source |
|---|---|---|
| Running an external program | `fork` then `execve` in the child; everything the child needs is prepared before the fork, because only the forking thread exists in the child | [spawn.rs](../crates/omsh-exec/src/spawn.rs) |
| Telling "not found" (127) and "not executable" (126) apart from success | A pipe closed on `exec` (`O_CLOEXEC`): end-of-file means `execve` worked, an error number means it failed | [spawn.rs](../crates/omsh-exec/src/spawn.rs) |
| Finding a program | Searching `PATH` directories, with a cache keyed by the `PATH` value; `hash -r` clears it | [path.rs](../crates/omsh-exec/src/path.rs) |
| `a \| b \| c` | `pipe2`: one pipe between neighbours; each stage is a thread that forks only when it reaches a program | [exec/pipeline.rs](../crates/omsh-exec/src/exec/pipeline.rs), [jobs.rs](../crates/omsh-exec/src/jobs.rs) |
| `$(...)` | A pipe for standard output, read by a second thread so a large output cannot block | [expand/mod.rs](../crates/omsh-exec/src/expand/mod.rs) |
| `2>&1`, `3>file`, `<` | A table of open files per command; the child applies it with `dup2` before `execve` | [io.rs](../crates/omsh-exec/src/io.rs), [redirect.rs](../crates/omsh-exec/src/redirect.rs), [spawn.rs](../crates/omsh-exec/src/spawn.rs) |
| Here-documents `<<EOF` | An anonymous memory file (`memfd_create`), so there is no pipe size limit and no helper process | [redirect.rs](../crates/omsh-exec/src/redirect.rs) |
| `set -C` (noclobber) and `>\|` | Opening with `O_CREAT \| O_EXCL`, which fails if the file exists | [redirect.rs](../crates/omsh-exec/src/redirect.rs) |
| `umask` | The process file-mode mask: set with `umask(2)`, read back from `/proc/self/status` | [builtins/umask.rs](../crates/omsh-exec/src/builtins/umask.rs) |
| Variables and `export` | A private table; the environment (`envp`) is built for each `exec` | [vars.rs](../crates/omsh-exec/src/vars.rs) |
| `exec cmd` | Replacing the process image with `execve`, without a fork | [builtins/exec.rs](../crates/omsh-exec/src/builtins/exec.rs), [spawn.rs](../crates/omsh-exec/src/spawn.rs) |
| Ctrl+C stops the job, not the shell | Process groups: `setpgid` in both parent and child; the terminal sends the signal to the foreground group | [spawn.rs](../crates/omsh-exec/src/spawn.rs), [jobs.rs](../crates/omsh-exec/src/jobs.rs) |
| Giving the terminal to a job | `tcsetpgrp`; the shell ignores `SIGTTOU` first, or it would stop itself | [terminal.rs](../crates/omsh-exec/src/terminal.rs), [signals.rs](../crates/omsh-exec/src/signals.rs) |
| Ctrl+Z, `fg`, `bg` | `waitpid` with `WUNTRACED` to see a stop, `SIGCONT` through `killpg` to continue | [spawn.rs](../crates/omsh-exec/src/spawn.rs), [exec/stopped.rs](../crates/omsh-exec/src/exec/stopped.rs), [jobs.rs](../crates/omsh-exec/src/jobs.rs) |
| Keeping the process group alive for late stages | A zombie leader held with `waitid` and `WNOWAIT`; its process id is not reused while the group exists | [spawn.rs](../crates/omsh-exec/src/spawn.rs), [jobs.rs](../crates/omsh-exec/src/jobs.rs) |
| `kill`, `trap`, hang-up, Ctrl+C handling | `sigaction`: handlers that only set a flag, ignored signals, defaults restored for a child | [signals.rs](../crates/omsh-exec/src/signals.rs), [traps.rs](../crates/omsh-exec/src/traps.rs), [builtins/trap.rs](../crates/omsh-exec/src/builtins/trap.rs) |
| A program started with a signal ignored keeps ignoring it | Ignored dispositions are inherited across `execve`; the shell reads what it inherited with `sigaction` before changing anything | [signals.rs](../crates/omsh-exec/src/signals.rs), [spawn.rs](../crates/omsh-exec/src/spawn.rs) |
| A crashed program does not leave the terminal broken | `termios` saved and restored (`tcgetattr`, `tcsetattr`), and a panic hook | [terminal.rs](../crates/omsh-exec/src/terminal.rs) |
| Full-screen views | Raw mode and the alternate screen, with a panic hook that restores both | [screen.rs](../crates/omsh-ui/src/screen.rs) |
| Hidden input for an API key and single-key questions | `termios` with `ICANON` and `ECHO` turned off, put back afterwards | [builtins/omsh.rs](../crates/omsh-exec/src/builtins/omsh.rs) |
| Login shell environment | A child `sh -l -c 'env -0 >&3'` with a pipe on descriptor 3 | [login.rs](../crates/omsh-exec/src/login.rs) |
| `-c`, scripts and piped input | `execv` of `/bin/sh`: the same process continues as `sh` | [handoff.rs](../crates/omsh-cli/src/handoff.rs) |
| `*.txt` | Reading directories (`read_dir`) and matching names | [expand/pathname.rs](../crates/omsh-exec/src/expand/pathname.rs) |
| Prompt git branch and dirty marker | A helper thread; `git` runs in its own process group, is killed at a deadline and collected | [git_status.rs](../crates/omsh-editor/src/git_status.rs) |
| History shared by several shells | SQLite in WAL mode; a writer thread fed by a channel | [store.rs](../crates/omsh-history/src/store.rs) |
| Trash | `rename`; across file systems (`EXDEV`) copy, verify, then remove | [trash.rs](../crates/omsh-safety/src/trash.rs) |
| Undo journal | A SQLite file (WAL) | [journal.rs](../crates/omsh-safety/src/journal.rs) |
| `omsh update` | Write a temporary file next to the program, `fsync`, run it, `rename` over the old one (atomic), `fsync` the folder | [update/mod.rs](../crates/omsh-cli/src/update/mod.rs) |
| Editing `config.toml` from the settings view | Temporary file and `rename`; the mode is kept | [edit.rs](../crates/omsh-config/src/edit.rs) |
| First-run config without a race | Write a temporary file, then `link`, which fails if the name is taken | [first_run.rs](../crates/omsh-config/src/first_run.rs) |
| `omsh uninstall` refuses for a login shell | The account database entry of the effective user (`getpwuid` through `nix`) | [omsh.rs](../crates/omsh-cli/src/omsh.rs) |
| AI requests only on demand | A `current_thread` async runtime built on the first request, driven from the calling thread | [runtime.rs](../crates/omsh-ai/src/runtime.rs) |
| "No network on the normal path" is tested | Running the shell under `strace -f` and listing every `socket` and `connect` | [offline.rs](../crates/omsh-tests/tests/offline.rs) |
| Testing a terminal program | A pseudo-terminal pair (`openpty`) with the shell on the slave side | [session.rs](../crates/omsh-tests/src/session.rs) |

## 4. Results

### 4.1 Tests

`cargo test --workspace --locked` at version 0.1.1 (before this documentation was added): **2460 passed, 0 failed, 1 ignored**
(local run). CI for the same commit ran six jobs, all green: format, Clippy, tests, tests with the network off, the performance
gate and `cargo deny` (run 43387684502 on `main`). The test kinds are listed in [design.md](design.md#testing).

### 4.2 Performance against the budgets

From the "Performance gate" job of that run (an `ubuntu-24.04` runner, warm file-system cache):

| Measure | Result | Budget |
|---|---|---|
| `--version` | 0.87 ms | 10 ms |
| `-c true` overhead over `/bin/sh -c true` | 0.81 ms | 5 ms |
| Start to first prompt (no repository / real `git` / `git` that never answers) | 1.95 / 4.32 / 22.41 ms | 30 ms |
| Idle memory at the prompt | 9.84 MB | 15 MB |
| Idle memory with a 100,000-row history | 11.21 MB | 15 MB |
| Keystroke to end of redraw, worst p99 | 0.21 ms | 5 ms |
| Completion menu, worst p99 over 20,000 programs | 11.5 ms | 50 ms |
| History lookups over 100,000 rows, worst p99 | 13.6 ms | 20 ms |

The same job also measures `/bin/sh -c true` at 0.52 ms median on that runner, so `-c true` is about 2.5 times slower than
`sh` in absolute terms (1.34 ms against 0.52 ms) while staying inside the budget.

### 4.3 Compatibility with `/bin/sh`

The conformance tests ([conformance.rs](../crates/omsh-cli/tests/conformance.rs), 87 tests) run the same input through this shell
and `/bin/sh` and require the same standard output and exit status. Where the shell follows bash instead of dash on purpose
(for example `${#x}` counting characters in a UTF-8 locale, or `[^a]` as negation), those cases are tested on their own and
named in the file.

### 4.4 Releases and installation

- Version 0.1.0 was the first real release. Its installer built a wrong download address on systems without `--base-url`
  (a variable of a helper function overwrote the version number: POSIX `sh` has no local variables). The owner chose to
  keep the release, mark it superseded, and publish 0.1.1.
- Version 0.1.1 was installed with its own `install.sh` in containers of Debian 12, Ubuntu 24.04, Fedora 45 and Arch Linux
  (x86_64, host network); each then ran `--version`, `omsh --version`, `omsh ai status` and `omsh update --check`. On Alpine
  the installer refused with a clear message (glibc is required). The `.deb` was installed in Debian 12, listed the shell in
  `/etc/shells`, and was removed cleanly.
- Not tested on hardware: the aarch64 files (checked by checksum and by build tests only).

## 5. Limitations

See [the user guide](user-guide.md#known-limitations). The main ones: keys that arrive in one read together with Ctrl+C are
dropped; the analyzer is a list of rules and does not know every dangerous command; masking does not know every kind of secret;
there is no proxy support; a cold disk cache and large work trees were not measured; aarch64 was not run on hardware.

## 6. Future work

- Tag 1.0.0 and test `omsh update` for real, from an installed 0.1.1 to 1.0.0 through GitHub.
- The success criteria for being useful in practice are not yet met: use as a daily shell for a month, use by a group of other
  people for a week, and reports or contributions from people the author does not know.
- Safety rules for more commands (`rsync --delete`, `truncate`, `tee`), trash coverage for `find -delete` and wrapped `rm`.
- Run the release files on aarch64 hardware; measure a cold disk cache.

## 7. Related work

General knowledge, not measured by this project: `dash` and `bash` are the usual `/bin/sh` implementations on Debian-family
Linux systems; `fish` is known for syntax highlighting and history suggestions on by default, and uses a language that is not
POSIX. The only measured comparison in this project is the one in section 4.3 and the `/bin/sh -c true` timing in section 4.2.

## 8. Timeline

Dates are those of the commits in the repository (71 commits, 2026-11-04 to 2026-11-13).

| Date | What was added |
|---|---|
| 2026-11-04 | Workspace, continuous integration, lexer |
| 2026-11-05 | Parser, diagnostics, program execution, expansion, pipelines, redirections, builtins, aliases |
| 2026-11-08 | Process groups and signals, background and stopped jobs, the pseudo-terminal test harness, invocation modes, config file, line editor, syntax highlighting |
| 2026-11-10 | SQLite history, AI provider layer and key storage |
| 2026-11-11 | Safety analyzer, AI plans, context and masking, blast radius, trash and undo, conversation, explain features, history search, hints, abbreviations, `j` |
| 2026-11-12 | Completion, git prompt, performance gate, robustness fixes, themes and motion, history explorer, command palette and settings view |
| 2026-11-13 | Notifications and tour, idle-memory work, polish, release pipeline, `omsh update` and `omsh uninstall`, version 0.1.0 and 0.1.1 |
