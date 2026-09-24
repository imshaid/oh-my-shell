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

from rich.markup import escape as _escape_markup

from ohmyshell import audit_log as audit_log_module
from ohmyshell import config as config_module
from ohmyshell import hardware as hardware_module
from ohmyshell import trash as trash_module

# Fixed accent palette (post-Build-Order, user-requested full-UI color
# audit): every user-facing string in this module now carries rich markup
# using ui/theme.py's fixed "omsh.*" style names, the same rule the rest
# of the app's own chrome already follows -- see ui/theme.py's own module
# docstring for the full rationale. This module was previously plain text
# by explicit design ("thin wrapper... no new logic" -- see this module's
# own docstring above); embedding markup here is a deliberate scope change
# confirmed with the user, not a violation of that "thin wrapper" intent
# -- these strings still carry no new LOGIC, only presentation markup
# around the exact same data. Every string returned by this module is
# printed via `active_console.print(outcome.text, highlight=False)`
# (main.py) -- `highlight=False` only disables rich's automatic pattern
# detection (numbers, paths, etc.), it does not disable markup parsing,
# so `[omsh.accent]...[/omsh.accent]` tags below render correctly.
HELP_TEXT = """\
  [bold][omsh.accent]✦ Oh My Shell[/omsh.accent][/bold] [omsh.muted]— Command Reference[/omsh.muted]
  [omsh.muted]────────────────────────────────────────────[/omsh.muted]
  Just type naturally:
    [omsh.path]"clean up temp files"[/omsh.path]          →  AI creates a plan
    [omsh.path]ls -la, cd, grep ...[/omsh.path]            →  runs directly, no AI involved

  Slash commands
    [omsh.accent]/model[/omsh.accent]          Show/change current model
    [omsh.accent]/history[/omsh.accent]        This session's earlier requests
    [omsh.accent]/undo[/omsh.accent]           Revert the last destructive action
    [omsh.accent]/trash[/omsh.accent]          View/manage .trash/ (status/keep/clear)
    [omsh.accent]/log[/omsh.accent]            View audit log (or /log export)
    [omsh.accent]/capabilities[/omsh.accent]   List what Oh My Shell can do
    [omsh.accent]/explain[/omsh.accent]        Why did the AI choose that last action?
    [omsh.accent]/stats[/omsh.accent]          Session token usage & average latency
    [omsh.accent]/system[/omsh.accent]         Full hardware & shell status
    [omsh.accent]/config[/omsh.accent]         View or change settings
    [omsh.accent]/clear[/omsh.accent]          Clear the screen
    [omsh.accent]/exit[/omsh.accent]           Quit

  [omsh.muted]Type /help <command> for details.[/omsh.muted]"""

_PER_COMMAND_HELP: dict[str, str] = {
    "model": "  [omsh.accent]/model[/omsh.accent]              Show the active model and available models.\n"
    "  [omsh.accent]/model list[/omsh.accent]         List all available models.\n"
    "  [omsh.accent]/model switch <n>[/omsh.accent]   Switch the active model to <n> (must be in the available list).",
    "undo": "  [omsh.accent]/undo[/omsh.accent]   Restore the most recent destructive action from .trash/.",
    "trash": "  [omsh.accent]/trash status[/omsh.accent]   Show what's currently in .trash/ and when it expires.\n"
    "  [omsh.accent]/trash keep[/omsh.accent]     Reset the retention timer, keeping everything a while longer.\n"
    "  [omsh.accent]/trash clear[/omsh.accent]    Permanently delete everything in .trash/ right now.",
    "log": "  [omsh.accent]/log[/omsh.accent]          Show recent audit log entries.\n  [omsh.accent]/log export[/omsh.accent]   Print the full audit log as JSON Lines.",
    "capabilities": "  [omsh.accent]/capabilities[/omsh.accent]   List every action Oh My Shell can take, with its risk level.",
    "explain": "  [omsh.accent]/explain[/omsh.accent]   Show the reasoning (action/params/risk) behind the most recent AI decision.",
    "config": "  [omsh.accent]/config[/omsh.accent]                View all current settings.\n"
    "  [omsh.accent]/config set <k> <v>[/omsh.accent]   Change one setting (dot-notation, e.g. trash.retention_days).",
    "system": "  [omsh.accent]/system[/omsh.accent]   Show OS, CPU, RAM, GPU, active model, and session stats.",
}


class MetaCommandError(Exception):
    """Raised when a command is recognized but cannot be carried out (e.g. bad args)."""


@dataclass(frozen=True)
class CommandOutcome:
    """What a slash command produced -- text to show, and whether to exit."""

    text: str
    should_exit: bool = False


def _parse(text: str) -> tuple[str, list[str]]:
    """Split "/model switch gemini-3.1-flash-lite" into ("model", ["switch", "gemini-3.1-flash-lite"])."""
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
        return f"  [omsh.warning]No detailed help for /{topic}. Try /help for the full list.[/omsh.warning]"
    return detail


def _handle_model(args: list[str], cfg: dict) -> str:
    available = config_module.get(cfg, "model.available")
    active = config_module.get(cfg, "model.active")

    if not args or args[0] == "list":
        lines = [f"  Available models [omsh.muted](active: [/omsh.muted][omsh.accent]{active}[/omsh.accent][omsh.muted])[/omsh.muted]"]
        for name in available:
            if name == active:
                lines.append(f"  [omsh.accent]→ {name}[/omsh.accent]")
            else:
                lines.append(f"  [omsh.muted]  {name}[/omsh.muted]")
        return "\n".join(lines)

    if args[0] == "switch":
        if len(args) < 2:
            raise MetaCommandError("Usage: /model switch <name>")
        target = args[1]
        if target not in available:
            raise MetaCommandError(f"Unknown model {target!r}. Available: {', '.join(available)}")
        config_module.set_value(cfg, "model.active", target)
        config_module.save(cfg)
        return f"  [omsh.success]Switched active model to {target}.[/omsh.success]"

    raise MetaCommandError(f"Usage: /model, /model list, or /model switch <name> (got {args[0]!r}).")


def _status_style(status: str) -> str:
    """Maps an audit-log entry's status string to a semantic omsh.* style,
    the same three-way success/failure/in-between distinction the rest of
    this app's UI already uses for risk levels and step outcomes."""
    if status in ("done",):
        return "omsh.success"
    if status in ("failed", "error"):
        return "omsh.danger"
    return "omsh.warning"  # cancelled, skipped, interrupted, or anything else


def _risk_style(risk: str) -> str:
    return {"low": "omsh.risk.low", "medium": "omsh.risk.medium", "high": "omsh.risk.high"}.get(risk, "omsh.muted")


def _usage_style(percent: float) -> str:
    """Same low/medium/high usage-color mapping as ui/thinking.py's own
    `_usage_style` (not imported directly -- this module has no other
    reason to import from ui/thinking.py, and the mapping is one line),
    used here for /system's CPU/RAM/GPU usage figures for the same
    at-a-glance "is this fine or not" reason."""
    if percent < 50:
        return "omsh.risk.low"
    if percent < 80:
        return "omsh.risk.medium"
    return "omsh.risk.high"


def _handle_history(session_start: float, base_dir=None) -> str:
    entries = audit_log_module.entries_since(session_start, base_dir=base_dir)
    if not entries:
        return "  [omsh.muted]No requests yet this session.[/omsh.muted]"
    # Bug fix (found during a wider real-terminal color audit): this header
    # line had no markup at all, unlike every other line this module
    # prints -- rendered as plain uncolored text alongside colored entries
    # right below it.
    lines = ["  [omsh.accent]This session's requests:[/omsh.accent]"]
    for i, entry in enumerate(entries, start=1):
        status_style = _status_style(entry.status)
        # `_escape_markup` on `entry.action` (post-Build-Order, found
        # during the same color-audit pass that added this markup):
        # `entry.action` is a real, previously-run shell command/AI
        # request string -- it can contain literal "[" / "]" characters
        # (e.g. a command with a bracket-glob argument), which rich's
        # markup parser would otherwise misinterpret as the start of a
        # (nonexistent) style tag. Escaping only the untrusted, freeform
        # value -- never the omsh.* tags this module writes itself -- is
        # the same rule Text-based command displays elsewhere in this app
        # already follow implicitly (Text.append() never markup-parses
        # its own text argument; a markup STRING like this one has no
        # such protection built in, so it has to be applied explicitly).
        lines.append(f"  {i}. {_escape_markup(entry.action)} [{status_style}]({entry.status})[/{status_style}]")
    return "\n".join(lines)


def _handle_undo(base_dir=None) -> str:
    try:
        restored = trash_module.undo_last_action(base_dir=base_dir)
    except trash_module.TrashError as exc:
        return f"  [omsh.warning]Nothing to undo: {exc}[/omsh.warning]"
    noun = "file" if len(restored) == 1 else "files"
    return f"  [omsh.success]Restored {len(restored)} {noun} from .trash/.[/omsh.success]"


def _handle_trash(args: list[str], cfg: dict, base_dir=None) -> str:
    subcommand = args[0] if args else "status"

    if subcommand == "status":
        entries = trash_module.list_trash(base_dir=base_dir)
        if not entries:
            return "  [omsh.muted].trash/ is empty.[/omsh.muted]"
        retention_days = config_module.get(cfg, "trash.retention_days")
        return f"  [omsh.accent]{len(entries)}[/omsh.accent] item(s) in .trash/ (retention: [omsh.accent]{retention_days}[/omsh.accent] days)."

    if subcommand == "clear":
        count = trash_module.clear_trash(base_dir=base_dir)
        return f"  [omsh.success]Permanently deleted {count} item(s) from .trash/.[/omsh.success]"

    if subcommand == "keep":
        # "Reset the retention timer" -- re-stamps every entry's trashed_at
        # to now via trash.keep_all() (implemented alongside this wiring;
        # see that function's own docstring for why re-stamping trashed_at
        # is the whole operation and no new on-disk shape was needed).
        count = trash_module.keep_all(base_dir=base_dir)
        if count == 0:
            return "  [omsh.muted].trash/ is empty -- nothing to keep.[/omsh.muted]"
        noun = "item" if count == 1 else "items"
        retention_days = config_module.get(cfg, "trash.retention_days")
        return f"  [omsh.success]Reset the retention timer for {count} {noun} -- kept for another {retention_days} days.[/omsh.success]"

    raise MetaCommandError(f"Usage: /trash status|keep|clear (got {subcommand!r}).")


def _handle_log(args: list[str], base_dir=None) -> str:
    try:
        entries = audit_log_module.read_entries(base_dir=base_dir)
    except audit_log_module.AuditLogError as exc:
        # Bug fix (found during a wider real-terminal color audit): this
        # was the one error message in the module with no [omsh.*] markup
        # at all -- every other error/status line here uses omsh.danger or
        # omsh.warning.
        return f"  [omsh.danger]Could not read the audit log: {exc}[/omsh.danger]"

    if not entries:
        return "  [omsh.muted]Audit log is empty.[/omsh.muted]"

    if args and args[0] == "export":
        import json

        from dataclasses import asdict

        return "\n".join(json.dumps(asdict(e)) for e in entries)

    # Bug fix (same color-audit pass): this header line had no markup at
    # all, unlike every entry line right below it.
    lines = ["  [omsh.accent]Recent audit log entries:[/omsh.accent]"]
    for entry in entries[-10:]:
        status_style = _status_style(entry.status)
        risk_style = _risk_style(entry.risk)
        lines.append(
            f"  {_escape_markup(entry.action)}  [{status_style}][{entry.status}][/{status_style}]"
            f"  risk=[{risk_style}]{entry.risk}[/{risk_style}]"
        )
    return "\n".join(lines)


def _handle_capabilities() -> str:
    """
    Rewritten for the open-ended architecture (see validation.py's module
    docstring): there is no fixed capability registry to list any more —
    the AI can generate any real shell command for any request. This now
    explains that plainly instead of enumerating a static action list.
    """
    return (
        "  [omsh.muted]Oh My Shell doesn't limit itself to a fixed list of actions.\n"
        "  Describe what you want in plain language and the AI will write\n"
        "  a real shell command for it — you'll always see the exact\n"
        "  command and its risk level before anything runs.\n\n"
        "  Raw shell syntax (ls, grep, pipes, ...) still runs directly,\n"
        "  no AI involved, just like a normal shell.[/omsh.muted]"
    )


def _handle_explain(base_dir=None) -> str:
    entry = audit_log_module.most_recent(base_dir=base_dir)
    if entry is None:
        return "  [omsh.muted]No AI decisions recorded yet this session.[/omsh.muted]"
    risk_style = _risk_style(entry.risk)
    status_style = _status_style(entry.status)
    return (
        f"  Last action: [omsh.accent]{_escape_markup(entry.action)}[/omsh.accent]"
        f"  (risk: [{risk_style}]{entry.risk}[/{risk_style}])\n"
        f"  Outcome: [{status_style}]{entry.status}[/{status_style}]"
    )


def _handle_stats(session_start: float, tokens_used: int | None = None, base_dir=None) -> str:
    summary = audit_log_module.summarize_session(session_start, base_dir=base_dir, tokens_used=tokens_used)
    return f"  [omsh.muted]{_escape_markup(audit_log_module.render_session_summary(summary))}[/omsh.muted]"


def _render_system_line_colored(snapshot: hardware_module.HardwareSnapshot) -> str:
    """
    Bug fix (found during a wider real-terminal color audit): /system's
    CPU/RAM/GPU line used to come straight from
    `hardware.render_system_line()` -- a plain string, rendered with no
    color at all, the one line in /system's whole output that wasn't.
    Rebuilt here (rather than adding rich markup inside hardware.py itself,
    which stays a plain data module on purpose -- see its own module
    docstring) with the same usage-based low/medium/high coloring
    ui/thinking.py's live indicator uses for the identical CPU/RAM/GPU
    percentages, via this module's own `_usage_style`.
    """
    cpu = snapshot.cpu
    parts = [
        f"CPU: {cpu.core_count} cores, [{_usage_style(cpu.usage_percent)}]{cpu.usage_percent:.0f}% used[/{_usage_style(cpu.usage_percent)}]"
    ]
    ram = snapshot.ram
    ram_percent = (ram.used_gb / ram.total_gb * 100) if ram.total_gb else 0.0
    parts.append(
        f"RAM: {ram.total_gb}GB total, "
        f"[{_usage_style(ram_percent)}]{ram.used_gb}GB used[/{_usage_style(ram_percent)}]"
    )
    if snapshot.gpu is not None:
        gpu = snapshot.gpu
        gpu_percent = (gpu.vram_used_mb / gpu.vram_total_mb * 100) if gpu.vram_total_mb else 0.0
        parts.append(
            f"GPU: {_escape_markup(gpu.name)}, "
            f"[{_usage_style(gpu_percent)}]{gpu.vram_used_mb:.0f}MB/{gpu.vram_total_mb:.0f}MB VRAM[/{_usage_style(gpu_percent)}]"
        )
    return "  ·  ".join(parts)


def _handle_system(cfg: dict, session_start: float, *, runner=subprocess.run, base_dir=None) -> str:
    snapshot = hardware_module.read_snapshot(runner=runner)
    active_model = config_module.get(cfg, "model.active")
    uptime_seconds = max(0.0, time.time() - session_start)
    uptime_minutes = int(uptime_seconds // 60)
    session_count = len(audit_log_module.entries_since(session_start, base_dir=base_dir))

    lines = [
        "  [bold][omsh.accent]System Info[/omsh.accent][/bold]",
        "  [omsh.muted]────────────────────────────────[/omsh.muted]",
        f"  {_render_system_line_colored(snapshot)}",
        "",
        "  [bold][omsh.accent]Oh My Shell[/omsh.accent][/bold]",
        "  [omsh.muted]────────────────────────────────[/omsh.muted]",
        f"  Active model: [omsh.accent]{active_model}[/omsh.accent]  ·  uptime [omsh.muted]{uptime_minutes}m[/omsh.muted]",
        f"  Session: [omsh.accent]{session_count}[/omsh.accent] requests",
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
        return f"  [omsh.success]Set {key} = {value!r}[/omsh.success]"

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