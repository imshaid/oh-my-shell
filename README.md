# Oh My Shell

An AI-powered, natural-language Linux shell. Type a request in plain English,
review a step-by-step plan, discuss/adjust it, confirm, and watch it execute
with live streaming progress. Raw shell syntax runs straight through, with
the AI staying silent unless a command looks destructive.

Oh My Shell is a standalone, login-shell-capable executable — not a terminal
emulator and not a plugin for another shell.

Built for CSE324 (Operating Systems Lab, Complex Engineering Problem).

## Install

```bash
curl -fsSL https://oh-my-shell.pages.dev/install.sh | bash
```

This downloads the latest release, verifies its SHA256 checksum, installs
Oh My Shell into `~/.local/share/oh-my-shell`, and (with `sudo`, optional)
symlinks it into `/usr/local/bin` and registers it in `/etc/shells` so it
can be set as a login shell.

On first run, a short wizard asks for a free Google AI Studio API key
(no card required — get one at https://aistudio.google.com/apikey).

```bash
oh-my-shell                          # start it
chsh -s /usr/local/bin/oh-my-shell   # optional: set as your login shell
```

## Requirements

- Ubuntu 22.04+ / Debian-family Linux
- Python 3.11+
- A Google AI Studio API key (free — the first-run wizard asks for it)

## Updating

Oh My Shell checks GitHub Releases in the background on startup and shows a
notice when a newer version is available:

```
╭─ ✦ Update available ──────────────────╮
│ A new version (v0.3.0) is available.  │
│ Run /update to install it.            │
╰───────────────────────────────────────╯
```

Update in place with:

```
/update
```

or check your currently installed version at any time:

```bash
oh-my-shell --version
```

## Development setup

```bash
git clone https://github.com/imshaid/oh-my-shell.git
cd oh-my-shell
python3 -m venv .venv
source .venv/bin/activate
pip install -e ".[dev]"
```

`./install.sh` does the same venv-creation + editable-install steps for an
existing checkout, without the git-clone/checksum logic `site/install.sh`
(the curl-pipe script above) uses for a fresh install.

Run tests:

```bash
pytest
```

## Releasing (maintainers)

```bash
git tag vX.Y.Z
git push origin vX.Y.Z
```

A tag push alone is enough — `.github/workflows/tag-release.yml` bumps
`pyproject.toml` to match, commits it, creates the GitHub Release with
generated notes, and computes/uploads `checksums.txt`.

## Status

Early development. See `Oh-My-Shell-Blueprint-FINAL.md` (project source of
truth) for full architecture, model-selection benchmarks, and specs.

## License

MIT
