"""
Audit Log & Reporting (Build Order Step 12, half of "audit_log.py + hardware.py").

Records every action, confirmation, outcome, and interrupted/skipped state
to an append-only JSON Lines file (Section 5.3, verbatim: "**Audit log:**
JSON Lines (`~/.oh-my-shell/audit.log.jsonl`), append-only — প্রতি
action/event একটা JSON object, এক লাইনে") and the Section 4.2 Component
table row: "Audit Log & Reporting | প্রতিটা action, confirmation, outcome,
ও interrupted-state রেকর্ড করে | Bash, log files (CSV/JSON) | Module 5".

--- Disclosed gap (Section 16 Rule 5) ---
The blueprint's exact per-entry JSON field schema was never captured in
this project's transcript (same 501-699 gap documented in ui/panels.py and
ui/streaming.py). What IS confirmed verbatim, and anchors this module's
design:
  - the file itself: JSON Lines, one JSON object per action/event, append-only.
  - a `"status"` field with at least the value `"interrupted"` (Section
    8.3.4: "Audit log-এ এটা `"status": "interrupted"` হিসেবে রেকর্ড হয় (error
    বা success না)"), confirming status is a distinct enum-like field, not
    folded into a generic "success: bool".
  - skipped sudo steps get "noted" in the log (Section 8.3.6: "audit log-এ
    '১টা step skipped' হিসেবে নোট থাকে").
  - `/log` and `/log export` (Section 8.4) read this file back; `/explain`
    (Section 8.4) explains "সর্বশেষ AI decision-এর reasoning" from it.
    Neither command's exact rendering was captured, so main.py/meta_commands.py
    (Step 14) will build their own display on top of this module's read API.

Field set (confirmed with the user, comprehensive per Rule 5): each entry
carries everything executor.py/sudo_layer.py/danger_classifier.py already
produce, so nothing needs re-deriving later --
    timestamp, action, source, params, risk, status, duration_seconds,
    used_sudo, error, detail
-- status values are "done" | "failed" | "interrupted" | "skipped" |
"cancelled" (a superset of executor.StepStatus's names plus "cancelled",
for a plan the user declined at the Confirmation + Discussion Loop before
any execution happened at all).
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

    Forward-compat note (found via manual end-to-end testing): a log line
    may carry a field this AuditEntry doesn't declare -- e.g. the user ran
    a newer oh-my-shell version that added a per-entry field (such as the
    `tokens_used` telemetry this module's own SessionSummary docstring
    already anticipates) against this same ~/.oh-my-shell/audit.log.jsonl,
    then downgraded, or the log is shared/copied onto a machine running an
    older version. `AuditEntry(**raw)` would previously raise TypeError on
    any unrecognized keyword, crashing the ENTIRE read (and therefore
    `/log`, `/log export`, `/explain`, and the session summary) over one
    forward-compatible line. Unknown fields are now dropped before
    construction so old code degrades gracefully -- it just can't see the
    field it doesn't know about, rather than refusing to read anything.

    Raises:
        AuditLogError: if a line exists but isn't valid JSON (a truncated
            write, e.g. from a crash mid-append, is the realistic cause --
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
    """The single most recent entry, or None if the log is empty -- what
    `/explain` ("সর্বশেষ AI decision-এর reasoning") reads from."""
    entries = read_entries(base_dir)
    return entries[-1] if entries else None


def entries_since(session_start: float, *, base_dir: Path | None = None) -> list[AuditEntry]:
    """Entries recorded at or after `session_start` -- used for the session
    summary (Section 8.3.9) and `/system`'s "Session: N requests · N tokens"
    line's request count."""
    return [e for e in read_entries(base_dir) if e.timestamp >= session_start]


@dataclass(frozen=True)
class SessionSummary:
    """
    What Section 8.3.9's exit summary needs (verbatim mockup):

        Session summary
        ────────────────────────────────
        8 requests processed  ·  1,240 tokens used  ·  2 files cleaned up
        Goodbye! 👋

    Scope note (Section 16 Rule 5): "tokens used" isn't something this
    module can compute from AuditEntry alone -- no token count is recorded
    per entry (intent_parser.py's ParseResult carries no token telemetry
    either, the same gap noted in ui/streaming.py's docstring for the
    "AI: N tokens" execution-summary line). `tokens_used` is therefore an
    optional field the caller supplies from elsewhere if/when that
    telemetry exists; it's not derived here. "files cleaned up" is also
    not literally in AuditEntry -- it's approximated as the count of
    successfully completed (status="done") entries whose action involved
    file operations; since capabilities.json has no "touches files" flag,
    this module reports the more honest, directly-supported number instead:
    total successfully completed requests.
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
    """
    Plain-text rendering matching Section 8.3.9's verbatim mockup shape
    (the boxed/rich version is ui/panels.py's job, same split as the rest
    of this codebase).
    """
    parts = [f"{summary.requests_processed} requests processed"]
    if summary.tokens_used is not None:
        parts.append(f"{summary.tokens_used:,} tokens used")
    parts.append(f"{summary.completed} completed")
    return "  ·  ".join(parts)