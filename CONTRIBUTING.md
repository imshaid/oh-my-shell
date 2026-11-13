# Contributing to Oh My Shell

Thank you for looking. Issues and pull requests are welcome. This page says how to build the project, what the checks are,
and the rules the code follows. To understand how the program is put together, read [docs/design.md](docs/design.md) first;
the [user guide](docs/user-guide.md) and the [command reference](docs/commands.md) describe what it does.

## Build and run

You need Linux and a recent stable Rust toolchain ([rustup](https://rustup.rs)). CI pins the exact version in
`.github/workflows/ci.yml` (`RUSTUP_TOOLCHAIN`).

```sh
cargo build                    # debug build
cargo run -p omsh-cli          # the interactive shell
cargo build --release          # target/release/oh-my-shell
```

To try the shell without touching your own files, give it a throw-away home:

```sh
mkdir -p /tmp/omsh-try && HOME=/tmp/omsh-try XDG_CONFIG_HOME=/tmp/omsh-try/c XDG_DATA_HOME=/tmp/omsh-try/d \
  XDG_STATE_HOME=/tmp/omsh-try/s XDG_CACHE_HOME=/tmp/omsh-try/k cargo run -p omsh-cli
```

`--no-config` reads and writes no config or history file at all.

## The checks

CI runs these on every push and pull request; run them before you push:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Notes on the tests:

- The end-to-end tests (`cargo test -p omsh-tests`) start the shell in a pseudo-terminal. They need `setsid` and `/proc`; some
  need `scp` and an `sftp-server` (the login-shell test), `strace` and `bash` (the "no socket" test).
- **Send the output of `cargo test` to a file or pipe it to `cat`.** Capturing it with `out=$(cargo test ...)` in a
  non-interactive bash makes the Ctrl+Z tests fail.
- Keep `TMPDIR` on a normal disk outside the repository and outside your home (the update and uninstall tests refuse a
  sandbox in the real home).
- `scripts/test-offline.sh` runs the whole suite in a network namespace that has only loopback (it asks for `sudo` once). CI runs
  it as the job "Test with the network off".
- `scripts/perf-gate.sh` builds the release program and checks the performance budgets (needs `hyperfine`, `python3`, `git`).
  CI runs it as "Performance gate"; a missed budget fails the job.
- `cargo deny check licenses bans sources` (see `deny.toml`) runs in CI; a new license needs a discussion.

## Rules for the code

- **English only** in code, comments, commit messages and docs.
- **Conventional commits:** `feat:`, `fix:`, `test:`, `docs:`, `refactor:`, `perf:`, `chore:`. `main` always builds and passes
  the tests.
- **`unsafe` is allowed only in `omsh-exec`**, in the `spawn` and `signals` modules, each block with a `// SAFETY:` comment.
  A workspace lint (`unsafe_code = "deny"`) enforces it.
- **No `unwrap()` or `expect()` outside tests** (Clippy denies them). A documented invariant may opt out with
  `#[expect(clippy::expect_used, reason = "...")]`.
- **Every bug fix comes with a regression test** that fails without the fix.
- **A new dependency needs a reason**: check that it is maintained, that its license passes `cargo deny`, and say in the pull
  request what it is for and what it costs in binary size. Check the current version on crates.io.
- **No network on the normal command path.** Network code lives in `omsh-ai` and runs only after an explicit AI command,
  `omsh update`, or the optional update check. A test fails on any socket created in a normal session.
- **Never print numbers or progress that were not measured.**
- **Each crate has a short module comment** that says what it is for; keep it true.
- **Respect the crate boundaries** (the arrows are in `Cargo.toml` and in [docs/design.md](docs/design.md#shape-of-the-program)):
  for example `omsh-exec` may not depend on the history; use a trait that the program implements.
- **Performance budgets** are part of the tests: start to first prompt under 30 ms, `--version` under 10 ms, `-c true` within 5 ms
  of `/bin/sh -c true`, idle memory under 15 MB, a keystroke under 5 ms (p99), the completion menu under 50 ms, a history lookup
  on 100,000 rows under 20 ms.

## Rules for writing tests

- **Wait for what the shell makes on a background thread.** The history file and its folder appear on the writer thread shortly after
  the first prompt. Use the helpers in `crates/omsh-tests/src/files.rs` (`wait_for_file`, `wait_for_wal_database`),
  `Session::wait_for_history()`, and `Ctx::wait_for_history_file()` instead of opening the file at once.
  To check a changed test, temporarily add a 500 ms sleep at the top of `Writer::open` in `crates/omsh-history/src/store.rs`
  and run `cargo test -p omsh-tests`; it must still pass. Do not commit the sleep.
- **At the editor, after Ctrl+C use `Session::cancel_line()`**, never `key(3)` followed by a search for `$ `. Between two lines
  use `command`/`sync`; two `line` calls in a row are type-ahead, and the editor may drop the second.
- **Tests of `omsh update` and `omsh uninstall` run only through `omsh_tests::sandbox::Sandbox`.** It points `HOME` and every
  `XDG_*` folder and `CARGO_HOME` inside a temporary home and refuses to start if any of them is the real home or inside it.
  The program that runs is a hard link to a copy of the test binary, never the file in `target/`.
- Every test has a time limit; no test may be able to hang. AI tests use the fake provider and the fake HTTP server on the
  loopback; none needs a real model.
- When you change the commands, builtins or settings, update [docs/commands.md](docs/commands.md): tests fail if a builtin, an
  `omsh` command or a `config.toml` key is missing there, or if a relative link in the Markdown files points nowhere.

## Reporting a problem

Please say the version (`oh-my-shell --version`), the distribution, the terminal, what you typed, what you expected and what
happened. For a crash or a broken terminal, say whether `oh-my-shell --no-config` behaves the same. Do not paste API keys or
history that holds secrets.

## License

By contributing you agree that your contribution is licensed under the [MIT license](LICENSE).
