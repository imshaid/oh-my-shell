"""
Meta-Command Handler (Build Order Step 14, final step) — full slash-command
dispatch (`/help`, `/model`, `/history`, `/undo`, `/trash`, `/log`,
`/capabilities`, `/explain`, `/stats`, `/system`, `/config`, `/clear`,
`/exit`).

Section 10.1's own line (verbatim): "Meta-command handler + slash-commands
(`/help`, `/model`, ইত্যাদি) — বিদ্যমান core logic-এর উপর thin wrapper" --
a thin wrapper over already-built modules. This module intentionally adds
no new logic of its own: every command below is a dispatch into config.py,
registry.py, audit_log.py, trash.py, or hardware.py, formatting their
existing return values into the blueprint's confirmed output shapes.

Full Section 8.4 command table (verbatim, confirmed present in this
project's transcript -- resolves the earlier-suspected 501-699 gap for
this section specifically):

    /help, /help <command>       command reference
    /model, /model switch,
    /model list                   show/change current model (reuses wizard's
                                   selection UI)
    /history                      this session's earlier requests
    /undo                         revert the most recent destructive action
    /trash status/keep/clear      view/manage .trash/
    /log, /log export             view/export the audit log
    /capabilities                 list the Capability Registry
    /explain                      reasoning behind the last AI decision
    /stats                        session token usage & average latency
    /system                       full hardware & shell status
    /config, /config set <k> <v>  view/change settings (incl. model-provider)
    /clear, /exit                 clear screen / quit

Confirmed verbatim (Section 4.1, line 142): the Meta-Command Handler never
enters the raw-shell/natural-language merge point -- a slash command
responds directly and the interaction ends at session level. It never
touches the Danger Classifier, Sudo Layer, or Executor. This is enforced
by construction here: no function in this module calls classify(),
decide_step(), or run_plan().

Confirmed verbatim (Section 8.5, immediately after the 8.4 table):
`--yes`/`-y` only skips confirmation for low-risk actions and must never
bypass a high-risk confirmation -- but that's the Danger Classifier's own
hard constraint (already enforced in main.py's _handle_raw_shell, which
never special-cases these flags), not something this module needs to
implement; slash commands don't run through that path at all.

--- Disclosed gaps (Section 16 Rule 5) ---
No output mockup exists in the retrieved blueprint text for `/model`,
`/capabilities`, `/config` (view form), `/history`, `/stats`, `/trash
status`, or `/help <command>`'s per-command detail -- only their one-line
table descriptions. Their renderings below are this module's own design,
built to carry exactly the data the table row promises and formatted
consistently with the blueprint's two CONFIRMED mockups (`/help`, `/system`
-- reproduced faithfully) so the overall command-reference "feel" is
consistent. Nothing downstream depends on these exact layouts.
No blueprint text describes unrecognized-slash-command behavior either
(checked specifically, found nothing) -- this module reports it as an
unrecognized command and suggests `/help`, matching ordinary CLI convention
and main.py's own prior placeholder message from Step 6.
"""

from __future__ import annotations

import shlex
import subprocess
import time
from dataclasses import dataclass
from typing import Callable

from ohmyshell import audit_log as audit_log_module
from ohmyshell import config as config_module
from ohmyshell import hardware as hardware_module
from ohmyshell import trash as trash_module

HELP_TEXT = """\
  Oh My Shell — Command Reference
  ────────────────────────────────────────────
  Just type naturally:
    "clean up temp files"          →  AI creates a plan
    ls -la, cd, grep ...            →  runs directly, no AI involved

  Slash commands
    /model          Show/change current model
    /history        This session's earlier requests
    /undo           Revert the last destructive action
    /trash          View/manage .trash/ (status/keep/clear)
    /log            View audit log (or /log export)
    /capabilities   List what Oh My Shell can do
    /explain        Why did the AI choose that last action?
    /stats          Session token usage & average latency
    /system         Full hardware & shell status
    /config         View or change settings
    /clear          Clear the screen
    /exit           Quit

  Type /help <command> for details."""

_PER_COMMAND_HELP: dict[str, str] = {
    "model": "  /model              Show the active model and available models.\n"
    "  /model list         List all available models.\n"
    "  /model switch <n>   Switch the active model to <n> (must be in the available list).",
    "undo": "  /undo   Restore the most recent destructive action from .trash/.",
    "trash": "  /trash status   Show what's currently in .trash/ and when it expires.\n"
    "  /trash keep     Reset the retention timer, keeping everything a while longer.\n"
    "  /trash clear    Permanently delete everything in .trash/ right now.",
    "log": "  /log          Show recent audit log entries.\n  /log export   Print the full audit log as JSON Lines.",
    "capabilities": "  /capabilities   List every action Oh My Shell can take, with its risk level.",
    "explain": "  /explain   Show the reasoning (action/params/risk) behind the most recent AI decision.",
    "config": "  /config                View all current settings.\n"
    "  /config set <k> <v>   Change one setting (dot-notation, e.g. trash.retention_days).",
    "system": "  /system   Show OS, CPU, RAM, GPU, active model, and session stats.",
}


class MetaCommandError(Exception):
    """Raised when a command is recognized but cannot be carried out (e.g. bad args)."""


@dataclass(frozen=True)
class CommandOutcome:
    """What a slash command produced -- text to show, and whether to exit."""

    text: str
    should_exit: bool = False


def _parse(text: str) -> tuple[str, list[str]]:
    """Split "/model switch qwen3:8b" into ("model", ["switch", "qwen3:8b"])."""
    tokens = shlex.split(text) if text.strip() else []
    if not tokens:
        return "", []
    command = tokens[0].lstrip("/").lower()
    return command, tokens[1:]


def _handle_help(args: list[str]) -> str:
    if not args:
        return HELP_TEXT
    topic = args[0].lstrip("/").lower()
    detail = _PER_COMMAND_HELP.get(topic)
    if detail is None:
        return f"  No detailed help for /{topic}. Try /help for the full list."
    return detail


def _handle_model(args: list[str], cfg: dict) -> str:
    available = config_module.get(cfg, "model.available")
    active = config_module.get(cfg, "model.active")

    if not args or args[0] == "list":
        lines = [f"  Available models ({'active: ' + active})"]
        for name in available:
            marker = "→" if name == active else " "
            lines.append(f"  {marker} {name}")
        return "\n".join(lines)

    if args[0] == "switch":
        if len(args) < 2:
            raise MetaCommandError("Usage: /model switch <name>")
        target = args[1]
        if target not in available:
            raise MetaCommandError(f"Unknown model {target!r}. Available: {', '.join(available)}")
        config_module.set_value(cfg, "model.active", target)
        config_module.save(cfg)
        return f"  Switched active model to {target}."

    raise MetaCommandError(f"Usage: /model, /model list, or /model switch <name> (got {args[0]!r}).")


def _handle_history(session_start: float, base_dir=None) -> str:
    entries = audit_log_module.entries_since(session_start, base_dir=base_dir)
    if not entries:
        return "  No requests yet this session."
    lines = ["  This session's requests:"]
    for i, entry in enumerate(entries, start=1):
        lines.append(f"  {i}. {entry.action} ({entry.status})")
    return "\n".join(lines)


def _handle_undo(base_dir=None) -> str:
    try:
        restored = trash_module.undo_last_action(base_dir=base_dir)
    except trash_module.TrashError as exc:
        return f"  Nothing to undo: {exc}"
    noun = "file" if len(restored) == 1 else "files"
    return f"  Restored {len(restored)} {noun} from .trash/."


def _handle_trash(args: list[str], cfg: dict, base_dir=None) -> str:
    subcommand = args[0] if args else "status"

    if subcommand == "status":
        entries = trash_module.list_trash(base_dir=base_dir)
        if not entries:
            return "  .trash/ is empty."
        retention_days = config_module.get(cfg, "trash.retention_days")
        return f"  {len(entries)} item(s) in .trash/ (retention: {retention_days} days)."

    if subcommand == "clear":
        count = trash_module.clear_trash(base_dir=base_dir)
        return f"  Permanently deleted {count} item(s) from .trash/."

    if subcommand == "keep":
        # "Reset the retention timer" -- re-stamps every entry's trashed_at
        # to now via trash.keep_all() (implemented alongside this wiring;
        # see that function's own docstring for why re-stamping trashed_at
        # is the whole operation and no new on-disk shape was needed).
        count = trash_module.keep_all(base_dir=base_dir)
        if count == 0:
            return "  .trash/ is empty -- nothing to keep."
        noun = "item" if count == 1 else "items"
        retention_days = config_module.get(cfg, "trash.retention_days")
        return f"  Reset the retention timer for {count} {noun} -- kept for another {retention_days} days."

    raise MetaCommandError(f"Usage: /trash status|keep|clear (got {subcommand!r}).")


def _handle_log(args: list[str], base_dir=None) -> str:
    try:
        entries = audit_log_module.read_entries(base_dir=base_dir)
    except audit_log_module.AuditLogError as exc:
        return f"  Could not read the audit log: {exc}"

    if not entries:
        return "  Audit log is empty."

    if args and args[0] == "export":
        import json

        from dataclasses import asdict

        return "\n".join(json.dumps(asdict(e)) for e in entries)

    lines = ["  Recent audit log entries:"]
    for entry in entries[-10:]:
        lines.append(f"  {entry.action}  [{entry.status}]  risk={entry.risk}")
    return "\n".join(lines)


def _handle_capabilities() -> str:
    """
    Rewritten for the open-ended architecture (see validation.py's module
    docstring): there is no fixed capability registry to list any more —
    the AI can generate any real shell command for any request. This now
    explains that plainly instead of enumerating a static action list.
    """
    return (
        "  Oh My Shell doesn't limit itself to a fixed list of actions.\n"
        "  Describe what you want in plain language and the AI will write\n"
        "  a real shell command for it — you'll always see the exact\n"
        "  command and its risk level before anything runs.\n\n"
        "  Raw shell syntax (ls, grep, pipes, ...) still runs directly,\n"
        "  no AI involved, just like a normal shell."
    )


def _handle_explain(base_dir=None) -> str:
    entry = audit_log_module.most_recent(base_dir=base_dir)
    if entry is None:
        return "  No AI decisions recorded yet this session."
    return f"  Last action: {entry.action}  (risk: {entry.risk})\n  Outcome: {entry.status}"


def _handle_stats(session_start: float, tokens_used: int | None = None, base_dir=None) -> str:
    summary = audit_log_module.summarize_session(session_start, base_dir=base_dir, tokens_used=tokens_used)
    return f"  {audit_log_module.render_session_summary(summary)}"


def _handle_system(cfg: dict, session_start: float, *, runner=subprocess.run, base_dir=None) -> str:
    snapshot = hardware_module.read_snapshot(runner=runner)
    active_model = config_module.get(cfg, "model.active")
    provider = cfg.get("model", {}).get("provider", "google_ai_studio")
    provider_label = "Google AI Studio (Gemini)" if provider == "google_ai_studio" else "local (Ollama)"
    uptime_seconds = max(0.0, time.time() - session_start)
    uptime_minutes = int(uptime_seconds // 60)
    session_count = len(audit_log_module.entries_since(session_start, base_dir=base_dir))

    lines = [
        "  System Info",
        "  ────────────────────────────────",
        f"  {hardware_module.render_system_line(snapshot)}",
        "",
        "  Oh My Shell",
        "  ────────────────────────────────",
        f"  Active model: {active_model}  ·  Provider: {provider_label}  ·  uptime {uptime_minutes}m",
        f"  Session: {session_count} requests",
    ]
    return "\n".join(lines)


def _handle_config(args: list[str], cfg: dict) -> str:
    if not args:
        import json

        return json.dumps(cfg, indent=2)

    if args[0] == "set":
        if len(args) < 3:
            raise MetaCommandError("Usage: /config set <key> <value>")
        key, raw_value = args[1], args[2]
        value = _coerce_config_value(raw_value)
        try:
            config_module.set_value(cfg, key, value)
        except KeyError:
            raise MetaCommandError(f"Unknown config key: {key!r}") from None
        config_module.save(cfg)
        return f"  Set {key} = {value!r}"

    raise MetaCommandError(f"Usage: /config or /config set <key> <value> (got {args[0]!r}).")


def _coerce_config_value(raw: str):
    """Best-effort type coercion for /config set's value (bool/int/float/str)."""
    lowered = raw.lower()
    if lowered in ("true", "false"):
        return lowered == "true"
    try:
        return int(raw)
    except ValueError:
        pass
    try:
        return float(raw)
    except ValueError:
        pass
    return raw


# Commands recognized as "exit" (matches main.py's Step 6 EXIT_COMMANDS,
# now owned here since this module is the full slash-command handler).
EXIT_COMMANDS = {"exit", "quit"}


def dispatch(
    text: str,
    *,
    cfg: dict,
    session_start: float,
    base_dir=None,
    tokens_used: int | None = None,
) -> CommandOutcome:
    """
    Handle one slash command (already routed as InputKind.SLASH_COMMAND by
    router.py) and return its output text plus whether the REPL should exit.

    Raises:
        MetaCommandError: for a recognized command used with bad arguments
            (callers should catch this and print exc's message rather than
            crash the REPL -- same fail-soft spirit as the rest of the app).
    """
    command, args = _parse(text)

    if command == "":
        return CommandOutcome(text="")

    if command in EXIT_COMMANDS:
        return CommandOutcome(text="", should_exit=True)

    if command == "clear":
        # ANSI clear-screen + cursor-home; main.py prints this as-is.
        return CommandOutcome(text="\033[2J\033[H")

    if command == "help":
        return CommandOutcome(text=_handle_help(args))

    if command == "model":
        return CommandOutcome(text=_handle_model(args, cfg))

    if command == "history":
        return CommandOutcome(text=_handle_history(session_start, base_dir=base_dir))

    if command == "undo":
        return CommandOutcome(text=_handle_undo(base_dir=base_dir))

    if command == "trash":
        return CommandOutcome(text=_handle_trash(args, cfg, base_dir=base_dir))

    if command == "log":
        return CommandOutcome(text=_handle_log(args, base_dir=base_dir))

    if command == "capabilities":
        return CommandOutcome(text=_handle_capabilities())

    if command == "explain":
        return CommandOutcome(text=_handle_explain(base_dir=base_dir))

    if command == "stats":
        return CommandOutcome(text=_handle_stats(session_start, tokens_used=tokens_used, base_dir=base_dir))

    if command == "system":
        return CommandOutcome(text=_handle_system(cfg, session_start, base_dir=base_dir))

    if command == "config":
        return CommandOutcome(text=_handle_config(args, cfg))

    raise MetaCommandError(f"Unrecognized command: /{command}. Try /help.")