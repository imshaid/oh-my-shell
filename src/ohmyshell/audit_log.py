"""
Audit Log & Reporting (Build Order Step 12).

Records every action, confirmation, outcome, and interrupted/skipped state
to an append-only JSON Lines file (`~/.oh-my-shell/audit.log.jsonl`, one
JSON object per line — Section 5.3).

Each entry carries: timestamp, action, source, params, risk, status,
duration_seconds, used_sudo, error, detail. `status` is one of "done" |
"failed" | "interrupted" | "skipped" | "cancelled" ("cancelled" covers a
plan the user declined at the Confirmation + Discussion Loop before any
execution happened).

`/log` and `/log export` (Section 8.4) read this file back; `/explain`
explains the most recent AI decision from it — both built on top of this
module's read API in main.py/meta_commands.py.
"""

from __future__ import annotations

import json
import time
from dataclasses import asdict, dataclass, fields
from pathlib import Path
from typing import Any, Iterator

from ohmyshell import config as config_module

AUDIT_LOG_FILENAME = "audit.log.jsonl"

VALID_STATUSES = ("done", "failed", "interrupted", "skipped", "cancelled")


class AuditLogError(Exception):
    """Raised when the audit log file exists but can't be read/parsed."""


@dataclass(frozen=True)
class AuditEntry:
    """One recorded action/event (one line of audit.log.jsonl)."""

    timestamp: float
    action: str
    source: str  # "natural_language" | "raw_shell" | "sudo_escalation" | ...
    params: dict[str, Any]
    risk: str | None
    status: str
    duration_seconds: float | None = None
    used_sudo: bool = False
    error: str | None = None
    detail: str = ""

    def __post_init__(self) -> None:
        if self.status not in VALID_STATUSES:
            raise ValueError(f"Invalid audit status {self.status!r}; expected one of {VALID_STATUSES}")


def audit_log_path(base_dir: Path | None = None) -> Path:
    """The audit.log.jsonl path, under `base_dir` (default: config dir)."""
    root = base_dir if base_dir is not None else config_module.CONFIG_DIR
    return root / AUDIT_LOG_FILENAME


def record(entry: AuditEntry, *, base_dir: Path | None = None) -> None:
    """Append one entry to the audit log (creates the file/dir if needed)."""
    path = audit_log_path(base_dir)
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a", encoding="utf-8") as f:
        f.write(json.dumps(asdict(entry)) + "\n")


def record_action(
    *,
    action: str,
    source: str,
    params: dict[str, Any] | None = None,
    risk: str | None = None,
    status: str,
    duration_seconds: float | None = None,
    used_sudo: bool = False,
    error: str | None = None,
    detail: str = "",
    base_dir: Path | None = None,
    now: float | None = None,
) -> AuditEntry:
    """Convenience wrapper: build an AuditEntry with the current time and record it."""
    entry = AuditEntry(
        timestamp=now if now is not None else time.time(),
        action=action,
        source=source,
        params=params if params is not None else {},
        risk=risk,
        status=status,
        duration_seconds=duration_seconds,
        used_sudo=used_sudo,
        error=error,
        detail=detail,
    )
    record(entry, base_dir=base_dir)
    return entry


_AUDIT_ENTRY_FIELD_NAMES = frozenset(f.name for f in fields(AuditEntry))


def read_entries(base_dir: Path | None = None) -> list[AuditEntry]:
    """
    Read every entry from the audit log, oldest first.

    A log line may carry a field this AuditEntry doesn't declare (e.g. an
    older/newer oh-my-shell version wrote it) — unknown fields are dropped
    before construction, so one forward-compatible line doesn't crash the
    entire read.

    Raises:
        AuditLogError: if a line exists but isn't valid JSON (a truncated
            write, e.g. from a crash mid-append, is the realistic cause —
            callers such as `/log` can catch this and report a partial log
            rather than crashing the whole command).
    """
    path = audit_log_path(base_dir)
    if not path.exists():
        return []

    entries: list[AuditEntry] = []
    for line_number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
        if not line.strip():
            continue
        try:
            raw = json.loads(line)
        except json.JSONDecodeError as exc:
            raise AuditLogError(f"{path}:{line_number}: invalid JSON ({exc})") from exc
        known = {k: v for k, v in raw.items() if k in _AUDIT_ENTRY_FIELD_NAMES}
        entries.append(AuditEntry(**known))
    return entries


def iter_entries(base_dir: Path | None = None) -> Iterator[AuditEntry]:
    """Same as read_entries(), but as a generator (for large logs / `/log export`)."""
    yield from read_entries(base_dir)


def most_recent(base_dir: Path | None = None) -> AuditEntry | None:
    """The single most recent entry, or None if the log is empty — what
    `/explain` reads from."""
    entries = read_entries(base_dir)
    return entries[-1] if entries else None


def entries_since(session_start: float, *, base_dir: Path | None = None) -> list[AuditEntry]:
    """Entries recorded at or after `session_start` — used for the session
    summary (Section 8.3.9) and `/system`'s request count."""
    return [e for e in read_entries(base_dir) if e.timestamp >= session_start]


@dataclass(frozen=True)
class SessionSummary:
    """
    What Section 8.3.9's exit summary needs:

        Session summary
        ────────────────────────────────
        8 requests processed  ·  1,240 tokens used  ·  2 files cleaned up
        Goodbye! 👋

    `tokens_used` isn't derivable from AuditEntry alone (no per-entry token
    count is recorded), so it's an optional field the caller supplies from
    elsewhere. `completed` reports total successfully completed requests
    rather than a "files touched" count, since no field tracks that.
    """

    requests_processed: int
    completed: int
    tokens_used: int | None = None


def summarize_session(session_start: float, *, base_dir: Path | None = None, tokens_used: int | None = None) -> SessionSummary:
    """Build a SessionSummary from every entry recorded since session_start."""
    entries = entries_since(session_start, base_dir=base_dir)
    completed = sum(1 for e in entries if e.status == "done")
    return SessionSummary(requests_processed=len(entries), completed=completed, tokens_used=tokens_used)


def render_session_summary(summary: SessionSummary) -> str:
    """Plain-text rendering matching Section 8.3.9's mockup shape (the
    boxed/rich version is ui/panels.py's job)."""
    parts = [f"{summary.requests_processed} requests processed"]
    if summary.tokens_used is not None:
        parts.append(f"{summary.tokens_used:,} tokens used")
    parts.append(f"{summary.completed} completed")
    return "  ·  ".join(parts)