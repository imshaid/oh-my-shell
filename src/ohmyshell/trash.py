"""
Trash/Undo Manager (Build Order Step 10, half of "executor.py + trash.py").

Implements the ".trash/ move-based delete" mechanism from Section 8.3.7
("Undo ও Trash Retention") and the runtime file layout from Section 5.2:

    ~/.oh-my-shell/
    ├── .trash/
    │   ├── <timestamp>-<original-name>  # moved file
    │   └── metadata.json                # trash-entry <-> original-path mapping, expiry

Core behavior (Section 8.3.7, verbatim-backed):
- A destructive delete moves the file/dir into `.trash/` instead of actually
  deleting it (`rm`).
- Trashed entries live for `trash.retention_days` (default 8, configurable
  via `/config set trash.retention_days <n>` -- Section 5.5) before they are
  eligible for permanent deletion.
- "expire হওয়ার আগে শেল-startup-এ warning দেখানো হয়" -- a warning is shown at
  shell startup before entries expire.
- `[u] Undo this action` / `/undo` restores the most recent destructive
  action; the interactive confirm is `[Enter] Confirm undo   [Esc] Cancel`.
- `/trash status`, `/trash keep`, `/trash clear` manage trash by hand.

Documented gap + assumption (Section 16 Rule 5): the blueprint states that
expired entries "auto-delete" and that a "startup warning" is shown, but
never specifies the *trigger* for the actual deletion -- no background job,
daemon, or cron is mentioned anywhere in the document. The chosen design
(confirmed with the user) is: no background process. `check_and_expire()`
is called once at shell startup (main.py's job, Step 6/13 territory) and:
  1. permanently deletes any entry whose retention period has *already*
     fully elapsed,
  2. returns a warning for any entry that has not yet expired but will
     within a short warning window, so main.py can print it once at
     startup, matching "expire হওয়ার আগে শেল-startup-এ warning দেখানো হয়".
This keeps the whole retention mechanism deterministic and testable without
threads, timers, or a daemon -- consistent with the rest of the codebase.
"""

from __future__ import annotations

import json
import shutil
import time
import uuid
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any

from ohmyshell import config as config_module

TRASH_DIR_NAME = ".trash"
METADATA_FILENAME = "metadata.json"

# How many days before actual expiry to start warning at shell startup.
# Not specified numerically anywhere in the blueprint (only that a warning
# appears "before" expiry) -- documented default, easy to tune later.
EXPIRY_WARNING_WINDOW_DAYS = 1

SECONDS_PER_DAY = 86400


class TrashError(Exception):
    """Raised for trash operations that cannot complete (missing entry, etc.)."""


@dataclass(frozen=True)
class TrashEntry:
    """One trashed file/dir, as recorded in metadata.json."""

    trash_id: str
    original_path: str
    trashed_name: str  # the "<timestamp>-<original-name>" filename inside .trash/
    trashed_at: float  # unix timestamp
    action_id: str | None = None  # groups entries moved together in one action, for /undo


def trash_dir(base_dir: Path | None = None) -> Path:
    """The `.trash/` directory path, under `base_dir` (default: config dir)."""
    root = base_dir if base_dir is not None else config_module.CONFIG_DIR
    return root / TRASH_DIR_NAME


def _metadata_path(base_dir: Path | None = None) -> Path:
    return trash_dir(base_dir) / METADATA_FILENAME


def _load_metadata(base_dir: Path | None = None) -> list[dict[str, Any]]:
    path = _metadata_path(base_dir)
    if not path.exists():
        return []
    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise TrashError(f"Could not read trash metadata at {path}: {exc}") from exc
    if not isinstance(raw, list):
        raise TrashError(f"{path} does not contain a JSON array at its top level.")
    return raw


def _save_metadata(entries: list[dict[str, Any]], base_dir: Path | None = None) -> None:
    tdir = trash_dir(base_dir)
    tdir.mkdir(parents=True, exist_ok=True)
    _metadata_path(base_dir).write_text(json.dumps(entries, indent=2) + "\n", encoding="utf-8")


def _entries(base_dir: Path | None = None) -> list[TrashEntry]:
    return [TrashEntry(**raw) for raw in _load_metadata(base_dir)]


def _write_entries(entries: list[TrashEntry], base_dir: Path | None = None) -> None:
    _save_metadata([asdict(e) for e in entries], base_dir)


def move_to_trash(
    path: str | Path,
    *,
    action_id: str | None = None,
    base_dir: Path | None = None,
    now: float | None = None,
) -> TrashEntry:
    """
    Move a single file or directory into `.trash/`, recording it in
    metadata.json so it can later be restored or expired.

    `action_id` groups multiple files moved as part of one logical
    destructive action (e.g. one `clean_temp_files` run moving 340 files),
    so `/undo` / restore_action() can restore them together.
    """
    source = Path(path)
    if not source.exists():
        raise TrashError(f"Cannot move to trash, path does not exist: {source}")

    tdir = trash_dir(base_dir)
    tdir.mkdir(parents=True, exist_ok=True)

    trashed_at = now if now is not None else time.time()
    trash_id = uuid.uuid4().hex
    trashed_name = f"{int(trashed_at)}-{trash_id}-{source.name}"
    destination = tdir / trashed_name

    # Resolve the original absolute path *before* moving, since `source`
    # will no longer exist afterward and `.resolve()` on a gone path is
    # unreliable -- resolve the (still-existing) parent instead.
    original_absolute = str(source.parent.resolve() / source.name)

    shutil.move(str(source), str(destination))

    entry = TrashEntry(
        trash_id=trash_id,
        original_path=original_absolute,
        trashed_name=trashed_name,
        trashed_at=trashed_at,
        action_id=action_id,
    )

    entries = _entries(base_dir)
    entries.append(entry)
    _write_entries(entries, base_dir)
    return entry


def list_trash(base_dir: Path | None = None) -> list[TrashEntry]:
    """All current trash entries, most-recently-trashed first."""
    return sorted(_entries(base_dir), key=lambda e: e.trashed_at, reverse=True)


def restore_entry(trash_id: str, *, base_dir: Path | None = None) -> TrashEntry:
    """Restore one trashed entry to its original path. Raises TrashError if not found."""
    entries = _entries(base_dir)
    match = next((e for e in entries if e.trash_id == trash_id), None)
    if match is None:
        raise TrashError(f"No trash entry with id {trash_id!r}")

    source = trash_dir(base_dir) / match.trashed_name
    destination = Path(match.original_path)
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.move(str(source), str(destination))

    remaining = [e for e in entries if e.trash_id != trash_id]
    _write_entries(remaining, base_dir)
    return match


def restore_action(action_id: str, *, base_dir: Path | None = None) -> list[TrashEntry]:
    """Restore every trash entry belonging to one action_id (a grouped move)."""
    entries = [e for e in _entries(base_dir) if e.action_id == action_id]
    if not entries:
        raise TrashError(f"No trash entries for action_id {action_id!r}")
    restored = [restore_entry(e.trash_id, base_dir=base_dir) for e in entries]
    return restored


def undo_last_action(base_dir: Path | None = None) -> list[TrashEntry]:
    """
    Restore the most recent destructive action (`/undo`, Section 8.4) --
    every entry sharing the action_id of the most-recently-trashed entry.
    Entries with no action_id (a lone move_to_trash call) are their own
    one-entry "action".
    """
    entries = list_trash(base_dir)
    if not entries:
        raise TrashError("Nothing to undo -- trash is empty.")
    latest = entries[0]
    if latest.action_id is None:
        return [restore_entry(latest.trash_id, base_dir=base_dir)]
    return restore_action(latest.action_id, base_dir=base_dir)


def permanently_delete(trash_id: str, *, base_dir: Path | None = None) -> None:
    """Permanently remove one trash entry (no further undo possible)."""
    entries = _entries(base_dir)
    match = next((e for e in entries if e.trash_id == trash_id), None)
    if match is None:
        raise TrashError(f"No trash entry with id {trash_id!r}")

    target = trash_dir(base_dir) / match.trashed_name
    if target.is_dir() and not target.is_symlink():
        shutil.rmtree(target, ignore_errors=True)
    elif target.exists() or target.is_symlink():
        target.unlink(missing_ok=True)

    remaining = [e for e in entries if e.trash_id != trash_id]
    _write_entries(remaining, base_dir)


def clear_trash(base_dir: Path | None = None) -> int:
    """Permanently delete every entry in trash (`/trash clear`). Returns count removed."""
    entries = _entries(base_dir)
    for entry in entries:
        permanently_delete(entry.trash_id, base_dir=base_dir)
    return len(entries)


def keep_all(base_dir: Path | None = None, *, now: float | None = None) -> int:
    """
    `/trash keep` (Section 8.4): reset every current trash entry's
    retention timer by re-stamping `trashed_at` to now, so nothing in
    `.trash/` looks any closer to expiry than the moment this was called.

    Documented follow-up, now implemented (see meta_commands.py's prior
    "isn't wired up yet" note): this reuses trash.py's own existing
    metadata read/write path (`_entries`/`_write_entries`) rather than
    adding any new on-disk shape -- each TrashEntry is a frozen dataclass,
    so this rebuilds the list with a new `trashed_at` per entry (dataclasses
    have no in-place field assignment) and writes it back in one pass,
    matching every other bulk-metadata operation in this module (e.g.
    clear_trash's own read-then-act-on-every-entry shape).

    `trashed_name` (which embeds the original trash timestamp, per
    move_to_trash's own naming scheme) and `trash_id` are left unchanged --
    only the metadata's own `trashed_at` field, which is what
    check_and_expire() actually reads, needs to move for retention to
    reset; renaming the file on disk to match would be extra churn with no
    behavioral difference.

    Returns the number of entries whose timer was reset (0 if trash is
    already empty -- a no-op, not an error, matching clear_trash's own
    "no-op on empty trash" behavior).
    """
    current_time = now if now is not None else time.time()
    entries = _entries(base_dir)
    if not entries:
        return 0
    refreshed = [
        TrashEntry(
            trash_id=e.trash_id,
            original_path=e.original_path,
            trashed_name=e.trashed_name,
            trashed_at=current_time,
            action_id=e.action_id,
        )
        for e in entries
    ]
    _write_entries(refreshed, base_dir)
    return len(refreshed)


@dataclass(frozen=True)
class ExpiryReport:
    """Result of check_and_expire(): what got deleted, what's about to."""

    deleted: list[TrashEntry]
    warned: list[TrashEntry]


def check_and_expire(
    *,
    retention_days: int,
    base_dir: Path | None = None,
    now: float | None = None,
    warning_window_days: int = EXPIRY_WARNING_WINDOW_DAYS,
) -> ExpiryReport:
    """
    Called once at shell startup (documented design, see module docstring).

    Permanently deletes any trash entry whose retention period has fully
    elapsed, and separately reports entries that will expire within
    `warning_window_days` but haven't yet, so the caller (main.py) can print
    a one-time startup warning -- matching Section 8.3.7's "expire হওয়ার আগে
    শেল-startup-এ warning দেখানো হয়".
    """
    current_time = now if now is not None else time.time()
    retention_seconds = retention_days * SECONDS_PER_DAY
    warning_seconds = warning_window_days * SECONDS_PER_DAY

    deleted: list[TrashEntry] = []
    warned: list[TrashEntry] = []

    for entry in _entries(base_dir):
        age = current_time - entry.trashed_at
        if age >= retention_seconds:
            permanently_delete(entry.trash_id, base_dir=base_dir)
            deleted.append(entry)
        elif age >= retention_seconds - warning_seconds:
            warned.append(entry)

    return ExpiryReport(deleted=deleted, warned=warned)


def render_expiry_warning(report: ExpiryReport, *, retention_days: int) -> str | None:
    """
    Plain-text startup warning for entries about to expire (Step 10's own
    UI; rich rendering is Step 11's job, same split used throughout).
    Returns None if there's nothing to warn about.
    """
    if not report.warned:
        return None
    count = len(report.warned)
    noun = "file" if count == 1 else "files"
    return (
        f"⚠ {count} trashed {noun} will be permanently deleted soon "
        f"(retention: {retention_days} days). Run /trash status to review, "
        f"or /trash keep to extend."
    )