"""
Update check — background GitHub Releases lookup + `/update` self-update.

On startup, main.py spawns a daemon thread (check_for_update_async) that
queries the GitHub Releases API for the newest published release tag and
compares it against this install's own version (pyproject.toml's
[project].version, read via importlib.metadata at runtime). A newer tag
sets a shared flag main.py reads once, right after the banner, to print a
one-line, muted "update available" notice -- the same spot and tone as the
rest of the REPL's incidental chrome (see main.py's _BANNER).

The check is deliberately best-effort: any failure (no network, GitHub
unreachable, rate-limited, malformed response) is swallowed silently and
simply leaves "no update" in place. A slow or offline network must never
delay the prompt appearing, which is why this runs on a background thread
with a short timeout rather than inline in `run()`.

`/update` (dispatched from meta_commands.py, implemented here since it
shells out to git/pip rather than any of the modules meta_commands.py
already wraps) does the same thing install.sh's own repo-update step does:
`git pull --ff-only` followed by `pip install -e .` in the current venv --
i.e. this module also does a *pull*, not a checksum-verified download; see
the checksum note on install.sh once that step lands.
"""

from __future__ import annotations

import re
import subprocess
import sys
import threading
import urllib.request
from dataclasses import dataclass
from importlib import metadata
from pathlib import Path

RELEASES_API_URL = "https://api.github.com/repos/imshaid/oh-my-shell/releases/latest"
# 2.0s was found (via manual testing on a real machine) to be too tight —
# the GitHub API call itself can legitimately take longer than that even
# on an ordinary connection, so a 2s cutoff was silently discarding a
# real, working response often enough to make the whole feature look
# broken. 5s still keeps this well clear of "delays the prompt" territory
# (this only ever runs on the background thread from check_for_update_async).
REQUEST_TIMEOUT_SECONDS = 5.0

# Set by check_for_update_async's background thread; read once by main.py
# right after startup. No lock needed -- a single str-or-None write from
# the background thread, a single read from the main thread, and a stale
# read (main thread checks before the background thread finishes) just
# means the notice is skipped for that one session, which is fine for a
# best-effort check.
_latest_version: str | None = None


@dataclass(frozen=True)
class UpdateCheckResult:
    current_version: str
    latest_version: str


def current_version() -> str:
    """
    This install's own version, read from the installed package's metadata
    (pyproject.toml's `[project].version` at whatever commit `pip install
    -e .` last ran against) rather than parsed from a file on disk -- so it
    always matches what's actually running, editable install or not.
    Public (no leading underscore): also used directly by main.py's `run()`
    for `oh-my-shell --version`, not just internally by check_for_update().
    """
    try:
        return metadata.version("oh-my-shell")
    except metadata.PackageNotFoundError:
        return "0.0.0"


def _parse_version(text: str) -> tuple[int, ...]:
    """"v0.2.10" / "0.2.10" -> (0, 2, 10); non-numeric parts are dropped."""
    cleaned = text.strip().lstrip("vV")
    parts = re.findall(r"\d+", cleaned)
    return tuple(int(p) for p in parts) or (0,)


def _is_newer(latest: str, current: str) -> bool:
    return _parse_version(latest) > _parse_version(current)


def check_for_update() -> UpdateCheckResult | None:
    """
    Synchronous GitHub Releases lookup. Returns None on any failure (no
    network, non-200, malformed JSON, no tag_name) or when already current
    -- callers never need to distinguish "check failed" from "no update".
    """
    import json

    try:
        request = urllib.request.Request(
            RELEASES_API_URL,
            headers={"Accept": "application/vnd.github+json", "User-Agent": "oh-my-shell"},
        )
        with urllib.request.urlopen(request, timeout=REQUEST_TIMEOUT_SECONDS) as response:
            payload = json.loads(response.read().decode("utf-8"))
    except Exception:
        return None

    tag = payload.get("tag_name")
    if not tag:
        return None

    current = current_version()
    if not _is_newer(tag, current):
        return None
    return UpdateCheckResult(current_version=current, latest_version=tag)


def check_for_update_async() -> threading.Thread:
    """
    Starts the lookup on a daemon thread; never blocks the caller. Returns
    the Thread so main.py can give it a short, bounded chance to finish
    (via Thread.join(timeout=...)) right before the first prompt is drawn,
    without ever risking an unbounded wait -- see pending_update()'s
    docstring for why a bare fire-and-forget start left the notice almost
    never appearing in practice.
    """

    def _worker() -> None:
        global _latest_version
        result = check_for_update()
        if result is not None:
            _latest_version = result.latest_version

    thread = threading.Thread(target=_worker, daemon=True)
    thread.start()
    return thread


def pending_update() -> str | None:
    """
    The latest version string if check_for_update_async found a newer
    release before this was called, else None. Consumed once by main.py
    right after the banner -- callers don't need to clear it themselves.

    Found via manual testing on a real machine: the gap between starting
    the background thread and main.py reading this (banner print + a
    couple of startup steps) is only ever a few milliseconds, while the
    GitHub API call itself routinely takes 1-2+ seconds -- so this almost
    always returned None even when the check would have succeeded a
    moment later. main.py now joins the thread (with a short timeout)
    immediately before calling this, so the check gets a real, bounded
    window to finish first.
    """
    return _latest_version


def run_self_update(repo_root: Path) -> tuple[bool, str]:
    """
    `git pull --ff-only` + `pip install -e .` in the current interpreter's
    environment, mirroring install.sh's own update step. Returns
    (succeeded, message) -- the message is shown as-is by /update's
    handler, success or failure, so it includes the relevant git/pip
    output on failure rather than a generic "update failed".
    """
    git_dir = repo_root / ".git"
    if not git_dir.is_dir():
        return False, "Not a git checkout — can't self-update. Re-run the installer instead."

    pull = subprocess.run(
        ["git", "-C", str(repo_root), "pull", "--ff-only"],
        capture_output=True,
        text=True,
    )
    if pull.returncode != 0:
        return False, f"git pull failed:\n{pull.stderr.strip() or pull.stdout.strip()}"

    install = subprocess.run(
        [sys.executable, "-m", "pip", "install", "-e", str(repo_root)],
        capture_output=True,
        text=True,
    )
    if install.returncode != 0:
        return False, f"pip install failed:\n{install.stderr.strip() or install.stdout.strip()}"

    if "Already up to date" in pull.stdout:
        return True, "Already on the latest version."
    return True, "Updated. Restart Oh My Shell to use the new version."