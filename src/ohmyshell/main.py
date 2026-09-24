"""
Shell REPL loop, entrypoint (Build Order Step 6, danger-check integrated
Step 8, full Meta-Command Handler wired in Step 14, full Section 4.1
pipeline wired post-Build-Order, full `rich`/`prompt_toolkit` visual
polish wired post-Build-Order — Step 11).

Per the Build Order's own scoping for Step 6 ("basic REPL loop, raw
pass-through প্রথমে — সবচেয়ে সহজ path"), this was initially NOT the full
Section 4.1 pipeline. Every later step filled in one piece; this module now
wires all of them together end to end:

- RAW_SHELL input is classified (danger_classifier.py, Step 8) before
  running. A Destructive verdict shows Section 8.3.5's confirmation prompt
  with all three options: [y] Run anyway / [n] Cancel / [t] Move to trash
  instead (only offered when the verdict says trash_alternative_possible;
  [t] moves the command's target into .trash/ via trash.py, Step 10,
  instead of running the original destructive command at all).
- NATURAL_LANGUAGE input now runs the full pipeline (Section 4.1): Intent
  Parser (Step 5) -> Plan Generator (Step 7) -> Confirmation + Discussion
  Loop (Step 7/11) -> Sudo Layer (Step 9, only if a step needs elevated
  permission) -> Streaming Executor (Step 10) -> Audit Log (Step 12). See
  _handle_natural_language's own docstring for the exact wiring.
- SLASH_COMMAND input goes through the full Meta-Command Handler
  (meta_commands.py, Step 14) — /help, /model, /history, /undo, /trash,
  /log, /capabilities, /explain, /stats, /system, /config, /clear, /exit,
  /quit are all recognized.

--- Step 11 visual-polish wiring ---
Every render in this module goes through ui/panels.py (boxed rich.Panel
confirmation/warning prompts) and ui/streaming.py (rich.Live execution
progress) rather than plain `print`.

What each module contributes:
  - ui/prompt.py: `_render_prompt` (plain "<folder> (<model>) ❯ " string)
    is replaced by ui/prompt.py's `render_prompt_ansi`, which reuses the
    same content/logic but colors the folder name and the icon (cyan ❯).
    ui/prompt.py's `ai_active` parameter (icon swap to magenta ✦) is not
    driven from here: this REPL's `session.prompt()` call blocks until the
    *next* line is typed, so there is no later moment in the same prompt
    render to redraw it "while" a request runs — the earliest an
    `ai_active=True` render could show is the *next* prompt after the
    request already finished, which would misrepresent what's currently
    happening rather than reflect it. Actually redrawing the live prompt
    mid-request would need a different input mechanism (a background
    thread refreshing a prompt_toolkit Application, not a single blocking
    `.prompt()` call) — left as a real Step-11-completing follow-up rather
    than faked here with a parameter that would always render `False`.
  - ui/session.py: the bare `input()` REPL read is replaced by a
    `ReplSession` (prompt_toolkit-backed) so the colored prompt actually
    renders with working line-editing (`input()` cannot safely take a
    pre-ANSI-escaped string — see ui/session.py's own docstring for why
    prompt_toolkit specifically, not just wrapping input() in more rich
    markup). `ReplSession` is also used as the `read` callable everywhere
    a raw `input` was passed before (edit-param sub-prompts, chat-adjust
    text, sudo-decision reads) — its `__call__` makes it a drop-in
    replacement for every existing `read: callable = input` parameter
    throughout this file, discussion.py, and sudo_layer.py.
  - ui/panels.py: the plan (confirm/edit/chat/cancel), destructive-command
    warning, and sudo-escalation prompts are now rendered as boxed
    rich.Panel output (render_plan_panel / render_destructive_command_panel
    / render_sudo_panel) instead of plain print() lines. `RichSudoPrompt`
    (new, alongside these) replaces `sudo_layer.InputPrompt` as the
    `SudoPrompt` passed into `run_plan` — same Grant/Skip/Abort decision
    contract, boxed rendering.
  - ui/streaming.py: execution progress now goes through
    `StreamingRenderer` (a `rich.Live` spinner that collapses to a
    ✓/✗/⚠/⊘ summary line per step) instead of one flat print per StepEvent.

Every function below takes `console: Console | None = None` (defaulting
to a fresh rich Console, matching ui/panels.py's own `print_panel`
convention) — tests inject a `Console(file=io.StringIO(),
force_terminal=False)` and assert against the buffer's rendered text, the
same pattern ui/panels.py's and ui/streaming.py's own test suites use.
`read: callable` parameters stay "a thing callable with an optional
prompt string that returns str" — a `ReplSession` or a plain test fake
both satisfy that shape identically, so discussion.py and sudo_layer.py
need no contract changes.

`--yes`/`-y`/`--dry-run`/`--verbose`/`--quiet` (Section 8.5) are still not
implemented — no inline-modifier parsing exists on either the raw-shell or
natural-language input paths yet. The one hard constraint that DOES apply
today — `--yes`/`-y` must never bypass a destructive-command confirmation —
remains satisfied by construction: neither _handle_raw_shell nor the NL
pipeline inspects command/request text for these flags at all.
"""

from __future__ import annotations

import dataclasses
import os
import shlex
import shutil
import subprocess
import sys
import threading
import time
from pathlib import Path

from rich.console import Console
from rich.style import Style
from rich.text import Text

from ohmyshell import audit_log as audit_log_module
from ohmyshell import config as config_module
from ohmyshell import meta_commands
from ohmyshell import trash as trash_module
from ohmyshell import update_check as update_check_module
from ohmyshell import wizard as wizard_module
from ohmyshell.danger_classifier import Destructive, DangerClassifierError, classify, override_risk
from ohmyshell.discussion import Cancelled, Confirmed, edit_command, run_discussion
from ohmyshell.executor import StepStatus, run_plan
from ohmyshell.intent_parser import IntentParseError, parse_intent
from ohmyshell.plan_generator import Plan, generate_plan
from ohmyshell.router import InputKind, route
from ohmyshell.ui.panels import (
    RichSudoPrompt,
    render_destructive_command_panel,
    render_plan_panel,
    render_update_available_panel,
)
from ohmyshell.ui.prompt import render_prompt_ansi
from ohmyshell.ui.session import ReplSession
from ohmyshell.ui.streaming import StreamingRenderer
from ohmyshell.ui.theme import OMSH_THEME, bordered_console, themed_console
from ohmyshell.ui.thinking import run_with_thinking_indicator

EXIT_COMMANDS = {"/exit", "/quit"}

# How long the first REPL loop iteration waits for the background GitHub
# Releases check (update_check_module.check_for_update_async) to finish
# before giving up on showing the "update available" notice this session.
# Bounded on purpose -- a slow or offline network must never delay the
# first prompt by more than this -- but long enough to actually catch a
# normal GitHub API response, which was measured (on a real machine) to
# take noticeably longer than the near-zero head start the wizard/config
# steps between thread-start and this join used to provide on their own.
UPDATE_CHECK_JOIN_TIMEOUT_SECONDS = 2.5

# One consistent banner shown once at startup — gives the app a signature
# look on launch rather than dropping straight into a bare prompt, kept
# intentionally small/quiet so it doesn't compete with /help's own
# reference text.
#
# This banner is oh-my-shell's own signature look, so it uses
# ui/theme.py's fixed hex accent rather than a terminal-theme-relative
# named color. See ui/theme.py's own module docstring.
#
# Built by hand with a real `Style` object instead of `Text.from_markup()`
# for the composite span: a composite markup tag mixing a plain attribute
# with a ui/theme.py "omsh.*" theme name (e.g. "[bold omsh.accent]...
# [/bold omsh.accent]") renders completely unstyled in rich rather than
# falling back to the color alone. Building the Style directly sidesteps
# rich's markup/style-string parser for this composite case.
_BANNER = Text()
_BANNER.append("✦ Oh My Shell", style=Style(bold=True) + OMSH_THEME.styles["omsh.accent"])
_BANNER.append(" — natural-language Linux shell\n", style="omsh.muted")
_BANNER.append("Type naturally, or /help for commands.", style="omsh.muted")


def _extract_trash_target(text: str) -> str | None:
    """
    Best-effort guess at "the path this raw command would have deleted",
    for the [t] move-to-trash-instead option on a raw shell command.

    A raw command is free-form shell syntax (e.g. "rm -rf /tmp/build",
    "rm -rf ./old_logs/*"), not a structured plan with a named `path`
    param. There is no reliable general way to know "the target" of an
    arbitrary shell command. The heuristic used here: take the last
    whitespace-
    separated token that isn't a flag (doesn't start with "-") -- this
    covers the common `rm [-flags...] <path>` shape (including the
    trailing-glob case `rm -rf ./old_logs/*`, since glob expansion is the
    shell's job and `shlex.split` still returns the literal glob token
    here). This is deliberately conservative: if no such token is found
    (e.g. a fork-bomb pattern, or a `dd`/`mkfs` device-write with no plain
    trailing path), [t] is simply not offered -- matching
    `trash_alternative_possible`'s own purpose of gating this option to
    the cases it can plausibly apply to. Multi-path commands
    (`rm -rf a b c`) only recover the LAST path with this heuristic; that
    limitation is accepted here rather than guessed around, since a fuller
    shell-argument parser is out of scope for a CEP-level raw-shell escape
    hatch that already has [n] Cancel as a safe fallback. The command word
    itself (the first token, e.g. "rm") is never returned as a target -- a
    command with no path argument at all (just flags, e.g. "rm -rf") has no
    real target to recover, and returning the command name in that case
    would silently trash a file that happens to be named "rm" if one
    exists in the current directory, which is worse than just not offering
    [t] at all.
    """
    try:
        tokens = shlex.split(text)
    except ValueError:
        return None
    if not tokens:
        return None
    for token in reversed(tokens[1:]):
        if token and not token.startswith("-"):
            return token
    return None


def _handle_raw_shell(
    text: str,
    cfg: dict,
    *,
    confirm: callable = input,
    console: Console | None = None,
    base_dir=None,
    nl_read: callable = input,
    nl_choice_read: callable | None = None,
    nl_sudo_input_fn: callable | None = None,
) -> None:
    """
    Classify, then execute raw shell input directly (Section 8.3.5).

    Confirmation prompt (Section 8.3.5): a Destructive verdict shows a
    boxed rich.Panel (ui/panels.render_destructive_command_panel) with
    [y] Run anyway / [n] Cancel, and -- only when the classifier says
    `trash_alternative_possible` -- also [t] Move to trash instead, which
    moves the command's apparent target (see _extract_trash_target's own
    docstring for how that's determined) into .trash/ via trash.py instead
    of running the original destructive command at all.

    Hard constraint (Section 8.4, line 853): nothing here inspects the
    command text for --yes/-y and skips the prompt on a destructive
    verdict — that flag has no special handling anywhere in this function,
    which is what makes it unable to bypass this confirmation.

    Fail-safe policy: if the LLM fallback itself fails, the command is
    treated as unclassifiable and not run automatically — the user is
    told why and asked to re-run manually, rather than silently executing
    something that couldn't be checked.

    Every outcome (run / cancelled / trashed) is recorded to the audit log
    (source="raw_shell") so /history, /log, and /stats see raw-shell
    activity too, not just natural-language actions.

    --- AI-fallback-on-misroute ---
    router.py's own docstring already admits its heuristic has margin
    cases: a natural-language phrase whose first word happens to be a
    real PATH executable (e.g. "open firefox", "open setting" -- `open`
    is a real xdg-open/gio wrapper on most systems) gets classified
    RAW_SHELL and lands here instead of going to the AI. Exit code alone
    can't reliably catch this: a genuinely-missing command gives the
    classic shell 127, but a misrouted phrase whose first word is a real,
    existing command (like `open`) fails with that command's own ordinary
    argument-parsing error instead ("gio: ... No such file or directory",
    "xdg-open: unexpected option ...") -- typically exit code 1 or 2,
    indistinguishable by number alone from an ordinary command failure.
    So detection here is stderr-text-based
    (`_looks_like_command_not_understood`, a sibling of executor.py's own
    `_looks_like_permission_denied`), checked whenever the exit code is
    simply non-zero at all -- see `_run_shell_command`'s own docstring for
    how that text is captured (a teed stderr pipe) without disturbing
    stdout/stdin, which is what keeps interactive full-screen programs
    (vim/top) working despite this capture.

    Whenever `_looks_like_command_not_understood` matches, this falls
    straight through to `_handle_natural_language` with no confirmation
    prompt -- a real typo against an existing command (e.g. "gerp foo")
    is reinterpreted as natural language exactly the same as a genuine
    misroute, trading a rare wrong reinterpretation for never
    interrupting the user here.

    Falls through to the exact same `_handle_natural_language` pipeline a
    NATURAL_LANGUAGE-routed input would have used, with the *original*
    text (not re-routed) -- this function's own audit-log entry above is
    skipped in that case, since `_handle_natural_language` logs its own
    outcome (source="natural_language"), and logging both would
    double-count one user action across two different sources.
    """
    active_console = console if console is not None else themed_console()
    try:
        result = classify(text, model=config_module.get(cfg, "model.active"))
    except DangerClassifierError as exc:
        active_console.print(
            f"  [omsh.warning]⚠ Could not classify this command ({exc}) — not running it automatically.[/omsh.warning]"
        )
        active_console.print(f"  [omsh.muted]Re-run manually if you're sure: {text}[/omsh.muted]")
        return

    if isinstance(result.verdict, Destructive):
        active_console.print(render_destructive_command_panel(result))
        choice = confirm("  > ").strip().lower()

        if choice == "t" and result.verdict.trash_alternative_possible:
            target = _extract_trash_target(text)
            if target is None:
                active_console.print("  [omsh.warning]Couldn't tell what to move to trash — cancelled. Nothing changed.[/omsh.warning]")
                audit_log_module.record_action(
                    action=text, source="raw_shell", status="cancelled",
                    detail="trash target not determinable", base_dir=base_dir,
                )
                return
            try:
                trash_module.move_to_trash(target, base_dir=base_dir)
            except trash_module.TrashError as exc:
                active_console.print(f"  [omsh.danger]Could not move {target!r} to trash: {exc}[/omsh.danger]")
                audit_log_module.record_action(
                    action=text, source="raw_shell", status="failed",
                    error=str(exc), detail=f"target={target}", base_dir=base_dir,
                )
                return
            active_console.print(f"  [omsh.success]✓[/omsh.success] Moved {target!r} to .trash/ instead of running the original command.")
            audit_log_module.record_action(
                action=text, source="raw_shell", status="done",
                detail=f"moved {target!r} to trash instead of running command", base_dir=base_dir,
            )
            return

        if choice != "y":
            active_console.print("  [omsh.warning]Cancelled.[/omsh.warning]")
            audit_log_module.record_action(action=text, source="raw_shell", status="cancelled", base_dir=base_dir)
            return

    exit_code, stderr_text = _run_shell_command(text)

    if exit_code is not None and exit_code != 0 and _looks_like_command_not_understood(stderr_text):
        audit_log_module.record_action(
            action=text, source="raw_shell", status="cancelled",
            detail="command not understood — auto-reinterpreted as natural language", base_dir=base_dir,
        )
        _handle_natural_language(
            text, cfg,
            read=nl_read, choice_read=nl_choice_read, sudo_input_fn=nl_sudo_input_fn,
            console=active_console, base_dir=base_dir,
        )
        return

    audit_log_module.record_action(action=text, source="raw_shell", status="done", base_dir=base_dir)


_COMMAND_NOT_UNDERSTOOD_SIGNATURES = (
    "command not found",
    "not found",  # covers fish's "Unknown command", bash's "<cmd>: command not found"
    "unexpected argument",
    "unexpected option",
    "unrecognized option",
    "unrecognized argument",
    "invalid option",
    "no such file or directory",
)


def _looks_like_command_not_understood(stderr: str) -> bool:
    """
    Sibling to executor.py's own `_looks_like_permission_denied` (same
    pattern: lowercase substring match against known shell/tool error
    phrasing, not a real parser) -- used by `_handle_raw_shell` to decide
    whether to offer the AI-fallback prompt. See that function's own
    docstring for why this exists and why it's a confirmation, not an
    automatic reinterpretation.
    """
    lowered = stderr.lower()
    return any(sig in lowered for sig in _COMMAND_NOT_UNDERSTOOD_SIGNATURES)


def _raw_shell_popen_args(text: str) -> list[str] | str:
    """
    Build the actual argv (or shell=True string) `_run_shell_command`
    spawns, preferring the person's own real login shell over Python's
    hardcoded `shell=True` default.

    Plain `subprocess.run(text, shell=True)` always uses `/bin/sh` (dash
    on most distros), spawned non-login/non-interactive -- not the
    person's actual shell. That means every environment variable their
    real shell's own config sets up (most visibly `LS_COLORS`) is never
    present for a raw command run this way -- so `ls -la --color=always`
    would have color forced on but nothing to color WITH, falling back to
    a generic built-in dircolors database instead of the person's own
    configured LS_COLORS.

    Detection: `$SHELL` (the standard POSIX "this is my login shell"
    variable, set by the OS/login manager -- works the same way for
    fish, bash, or zsh) is used when it points at a real, existing
    executable; `["$SHELL", "-c", text]` is real argv (no shell
    metacharacter quoting/injection risk from wrapping `text` in another
    shell=True string), and this exact shell binary does its own full
    normal startup (reading its own config/env setup) before running
    `text` -- exactly the environment the person sees when they type the
    same raw command directly into their own terminal.

    Falls back to `shell=True` when `$SHELL` is unset or doesn't point at
    a real file -- keeps this project running in any environment that has
    no shell entry set up, rather than hard-failing.
    """
    shell_path = os.environ.get("SHELL")
    if shell_path and shutil.which(shell_path) is not None:
        return [shell_path, "-c", text]
    return text


# --- fish "greeting noise on every raw command" fix ---
#
# `_raw_shell_popen_args` above makes every raw command run through the
# person's own real login shell so their own config (LS_COLORS, aliases,
# etc.) is present. For a fish user, that means `fish -c "text"` runs
# fish's own `config.fish` in full before `text` -- and if that config.fish
# calls something like `fastfetch`/`neofetch`/a banner script with no
# `status is-interactive` guard around it (a common real-world fish
# config), that banner reprints before every single raw command, not just
# once at real interactive shell startup.
#
# This is a gap in the person's own config, not in oh-my-shell -- fish
# itself already has the right primitive (`status is-interactive`) for a
# config to guard against exactly this. But different users' config.fish
# files call different unguarded things, so there's no fixed string
# oh-my-shell could detect and patch around at the source-analysis level,
# and shipping `fish --no-config` instead would silently drop the very env
# setup (LS_COLORS etc.) the shell-preference fix above exists to preserve.
#
# Defining a fish FUNCTION named `status` to make `status is-interactive`
# report false during raw exec does not work: `status` is one of fish's
# reserved keywords, not an ordinary builtin a function can shadow the way
# `ls`/`grep`/etc. can. There is no way to override what
# `status is-interactive` returns from outside fish's own implementation.
#
# The actual fix rewrites the noisy command's own line directly --
# wrapping known-noisy startup commands (fastfetch, neofetch; others can
# be added the same way if ever needed) in their own
# `if not set -q OMSH_RAW_EXEC ... end` guard, in place, the first time
# oh-my-shell finds one bare/unguarded. Every other line in the file
# (aliases, LS_COLORS, PATH exports, zoxide/starship/fzf init, ...) is
# left untouched -- only the specific noisy line itself gets wrapped, so
# raw commands keep every bit of real shell setup that makes `ls`/`grep`/
# etc. colorize and behave the way the person's own interactive shell
# does.
_FISH_GUARD_ENV = "OMSH_RAW_EXEC"
_FISH_GUARD_MARKER = "# oh-my-shell: greeting-noise guard installed"
# Command names known to print a startup greeting/banner in a real-world
# fish config -- each bare/unguarded invocation of one of these (a line
# that is exactly the command name, optionally with arguments, at column
# 0 -- not already inside an `if`/piped/commented out) gets wrapped.
_FISH_NOISY_COMMANDS = ("fastfetch", "neofetch")
# Fragments unique to two earlier, broken guard approaches -- used by
# `ensure_fish_guard_installed`'s migration path to detect and remove an
# already-installed old guard before applying the current, working fix.
_FISH_GUARD_OLD_STATUS_FUNCTION_LINE = "function status --wraps status"
_FISH_GUARD_OLD_EXIT_LINE = f"if set -q {_FISH_GUARD_ENV}\n    exit\nend"


def _fish_config_path() -> Path:
    """
    Fish's own standard config location -- `$XDG_CONFIG_HOME/fish/config.fish`
    if that's set (fish itself honors this), else `~/.config/fish/config.fish`.
    """
    xdg_config = os.environ.get("XDG_CONFIG_HOME")
    base = Path(xdg_config) if xdg_config else Path.home() / ".config"
    return base / "fish" / "config.fish"


_FISH_BLOCK_OPENERS = ("if", "for", "while", "function", "begin", "switch")


def _wrap_noisy_command_line(line: str) -> list[str] | None:
    """
    If `line` (one physical line of config.fish, no trailing newline) is a
    bare invocation of one of `_FISH_NOISY_COMMANDS` -- the command name is
    the first whitespace-separated token, ignoring leading indentation --
    returns the replacement lines to wrap it in an
    `if not set -q OMSH_RAW_EXEC ... end` guard (indented one level deeper
    than the original line, matching the original line's own indentation
    as the `if`/`end` lines' indentation). Returns None for anything else
    (blank lines, comments, unrelated commands). Whether this specific
    occurrence is already nested inside some OTHER conditional (e.g. the
    person's own `if status is-interactive ... fastfetch ... end`, which
    needs no further wrapping at all) is a property of the surrounding
    lines, not of this one line alone -- `ensure_fish_guard_installed`'s
    own scan tracks that nesting depth and simply never calls this
    function for a line it already knows is nested.
    """
    stripped = line.strip()
    if not stripped or stripped.startswith("#"):
        return None
    first_token = stripped.split(None, 1)[0]
    if first_token not in _FISH_NOISY_COMMANDS:
        return None
    indent = line[: len(line) - len(line.lstrip(" \t"))]
    return [
        f"{indent}if not set -q {_FISH_GUARD_ENV}",
        f"{indent}    {stripped}",
        f"{indent}end",
    ]


def ensure_fish_guard_installed() -> None:
    """
    Idempotently rewrite the person's own fish config.fish so any bare/
    unguarded call to a known-noisy startup command (`_FISH_NOISY_COMMANDS`
    -- fastfetch, neofetch) only runs for a real interactive fish session,
    not for oh-my-shell's own non-interactive raw-command invocations (see
    this module's own comment above `_FISH_GUARD_ENV`). Does nothing unless
    fish is the person's real shell (`$SHELL`) and config.fish already
    exists (never creates one from scratch). Every other line in the file
    -- aliases, LS_COLORS, PATH exports, tool init lines, comments, blank
    lines, anything not an exact bare noisy-command invocation -- is left
    untouched, byte-for-byte. Failures (permission error, unreadable file,
    etc.) are swallowed -- this is a quality-of-life fix, not something
    that should ever block the REPL from starting.

    Idempotent via `_FISH_GUARD_MARKER`, a comment line inserted once at
    the top of the file the first time any wrapping happens -- its
    presence alone means "this file has already been scanned/wrapped", so
    re-running this on every startup after the first is a fast no-op (the
    file is still opened and read every time, to catch a noisy command
    the person might add later, but scanning stops immediately once the
    marker is found).

    --- Migration for two earlier, broken guard attempts ---
    Anyone who ran an earlier version of oh-my-shell may have one of two
    broken guards already installed at the top of their config.fish: an
    `exit`-based guard that silently broke every raw command's LS_COLORS/
    alias setup (see `_FISH_GUARD_OLD_EXIT_LINE`), and a `function status`
    redefinition that fails to source at all since `status` is a fish
    reserved keyword. Both are detected via their own distinctive fragment
    and their whole old guard block is stripped out before this function's
    normal line-wrapping logic runs on what's left.
    """
    shell_path = os.environ.get("SHELL", "")
    if "fish" not in Path(shell_path).name:
        return

    config_path = _fish_config_path()
    try:
        if not config_path.is_file():
            return
        existing = config_path.read_text()

        if _FISH_GUARD_OLD_EXIT_LINE in existing or _FISH_GUARD_OLD_STATUS_FUNCTION_LINE in existing:
            existing = _strip_old_fish_guard_block(existing)

        if _FISH_GUARD_MARKER in existing:
            if existing != config_path.read_text():
                config_path.write_text(existing)
            return

        lines = existing.splitlines()
        new_lines: list[str] = []
        wrapped_anything = False
        # Tracks how many `if`/`for`/`function`/etc. blocks the scan is
        # currently nested inside -- a noisy-command line only gets
        # wrapped at depth 0 (a true top-level, unguarded call). A line
        # already inside the person's own `if status is-interactive ...
        # fastfetch ... end` (depth 1+ at that point) is left completely
        # alone: it's already conditional, wrapping it again would just
        # nest a redundant, harmless-but-pointless second `if` around it.
        depth = 0
        for line in lines:
            stripped = line.strip()
            first_token = stripped.split(None, 1)[0] if stripped and not stripped.startswith("#") else ""
            if depth == 0:
                replacement = _wrap_noisy_command_line(line)
            else:
                replacement = None
            if replacement is None:
                new_lines.append(line)
            else:
                new_lines.extend(replacement)
                wrapped_anything = True
            if first_token in _FISH_BLOCK_OPENERS:
                depth += 1
            elif first_token == "end":
                depth = max(0, depth - 1)

        rebuilt = "\n".join(new_lines)
        if existing.endswith("\n"):
            rebuilt += "\n"
        if wrapped_anything:
            rebuilt = f"{_FISH_GUARD_MARKER}\n" + rebuilt
        if rebuilt != config_path.read_text():
            config_path.write_text(rebuilt)
    except OSError:
        pass


def _strip_old_fish_guard_block(existing: str) -> str:
    """
    Removes either of the two earlier, broken guard blocks (see
    `ensure_fish_guard_installed`'s own "Migration" docstring section) from
    `existing`, whichever is present, leaving every other line untouched.
    Both old blocks start with the same first line (`# oh-my-shell: skip
    rest of config.fish for non-interactive raw exec`) and end with the
    first bare `end` line that closes their own single top-level
    `if set -q OMSH_RAW_EXEC` -- found here by counting `if`/`end` nesting
    from that first line, so this works for either old block's own
    (different) body without needing two separate hardcoded copies of it.
    """
    old_first_line = "# oh-my-shell: skip rest of config.fish for non-interactive raw exec"
    lines = existing.splitlines(keepends=True)
    start = None
    for i, line in enumerate(lines):
        if line.strip() == old_first_line:
            start = i
            break
    if start is None:
        return existing

    # `start + 1` is the block's own opening `if set -q OMSH_RAW_EXEC` line
    # (both old blocks' second line, always) -- that line has already
    # opened one level of if/end nesting before this loop's first
    # iteration even begins, so `depth` starts at 1 (not 0) to account for
    # it; the loop itself starts scanning at `start + 2`, the line right
    # after that opening `if`. Each further `if`/`function`/etc. opens
    # another level, each `end` closes one -- the `end` that brings `depth`
    # back down to 0 is the one that closes the block's own OUTERMOST `if`,
    # which is exactly the end of the whole guard block, regardless of how
    # much nested if/function/end structure the block's own body has
    # (verified directly against both old blocks' real, different bodies).
    depth = 1
    end = None
    for i in range(start + 2, len(lines)):
        stripped = lines[i].strip()
        first_token = stripped.split(None, 1)[0] if stripped else ""
        if first_token in ("if", "for", "while", "function", "begin", "switch"):
            depth += 1
        elif first_token == "end":
            depth -= 1
            if depth == 0:
                end = i
                break
    if end is None:
        return existing
    # Also swallow one blank separator line right after the old block, if
    # present (both old blocks were always installed with a trailing blank
    # line before the person's own original content) -- keeps the
    # stripped file from picking up an extra blank line at the top that
    # wasn't part of the person's own original config.fish.
    after = end + 1
    if after < len(lines) and lines[after].strip() == "":
        after += 1
    return "".join(lines[:start] + lines[after:])


def _run_shell_command(text: str) -> tuple[int | None, str]:
    """
    Actually execute a raw shell command. Prefers the person's own real
    login shell (see `_raw_shell_popen_args`'s own docstring for why --
    short version: their shell's own config sets up things like
    `LS_COLORS`, which a hardcoded `/bin/sh` never would), falling back to
    `shell=True` (Python's own `/bin/sh`) only when no real shell can be
    found -- either way, pipes/redirects/chaining the router already
    detected keep working, since both paths still hand the whole original
    command string to a real shell to interpret, never split/re-parsed by
    this function itself.

    stdin/stdout stay fully inherited from the real terminal, unchanged --
    interactive full-screen programs (vim/top) keep reading/writing the
    real tty directly, exactly as before. Only stderr is redirected to a
    pipe, then "teed": every chunk read from that pipe is written straight
    back out to the real stderr live (so the user still sees error output
    exactly as it streams, nothing is held back or delayed) while also
    being collected into a buffer this function returns alongside the exit
    code. This is what lets `_handle_raw_shell` inspect stderr for a
    "command not understood" signature (see `_looks_like_command_not_understood`)
    without capturing (and therefore risking breaking) stdout/stdin at all
    -- the one stream that actually matters for interactive programs.

    Returns (exit_code, captured_stderr). exit_code is None if the command
    couldn't even be spawned (OSError), in which case captured_stderr is
    always "".
    """
    args = _raw_shell_popen_args(text)
    # `_FISH_GUARD_ENV=1` in the child's own env -- read by the guard
    # `ensure_fish_guard_installed()` wraps around noisy commands in the
    # person's config.fish (fish-only; harmless/unused for any other
    # shell), so this invocation skips the person's full interactive
    # startup output (fastfetch, etc.) on every single raw command.
    # `os.environ` itself is left untouched -- only this one child process
    # sees the var.
    child_env = dict(os.environ, **{_FISH_GUARD_ENV: "1"})
    try:
        if isinstance(args, list):
            proc = subprocess.Popen(args, stderr=subprocess.PIPE, env=child_env)
        else:
            proc = subprocess.Popen(args, shell=True, stderr=subprocess.PIPE, env=child_env)
    except OSError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return None, ""

    captured: list[bytes] = []

    def _tee_stderr() -> None:
        assert proc.stderr is not None
        for chunk in iter(lambda: proc.stderr.read(4096), b""):
            sys.stderr.buffer.write(chunk)
            sys.stderr.buffer.flush()
            captured.append(chunk)

    tee_thread = threading.Thread(target=_tee_stderr, daemon=True)
    tee_thread.start()
    proc.wait()
    tee_thread.join()
    return proc.returncode, b"".join(captured).decode(errors="replace")


def _repl_get_user_choice(
    plan: Plan,
    *,
    read: callable | None = None,
    console: Console | None = None,
    telemetry=None,
    attempts: int | None = None,
) -> str:
    """
    REPL-level [Enter] confirm / [e] edit / [c] chat / [Esc] cancel prompt
    (Section 8.3.3), mapped onto discussion.run_discussion's expected
    "confirm"/"edit"/"chat"/"cancel" return values.

    Design note: discussion.py's own docstring says the raw-keypress-to-
    choice mapping belongs to the REPL layer (main.py), not to
    run_discussion itself -- this is that mapping, rendering the plan via
    ui/panels.render_plan_panel (boxed) instead of a plain text renderer.

    `telemetry`/`attempts` (intent_parser.ParseTelemetry / ParseResult.attempts,
    both optional) are passed straight through to render_plan_panel so the
    boxed panel this function prints carries the real tokens-in/out,
    duration, tok/s, model, and retry-count footer (Section 8.3.3's mockup)
    -- see _handle_natural_language's own wiring for where these come from.

    `read`, when not overridden, defaults to
    `ui.session.read_plan_choice_keypress` -- a real single-keypress reader
    bound to the actual Esc key event rather than a line-editing read
    compared against the literal typed text "esc" (see that function's own
    docstring). `read_plan_choice_keypress` returns already-lowercase/short
    tokens ("cancel"/"edit"/"chat"/<typed text>) directly, not a raw line
    needing the same "esc"/"q"/"cancel" string comparison below -- that
    comparison is kept for backward compatibility with callers/tests that
    still inject a plain `read: callable` returning ordinary typed lines.
    """
    from ohmyshell.ui.session import read_plan_choice_keypress

    active_console = console if console is not None else themed_console()
    active_console.print(render_plan_panel(plan, telemetry=telemetry, attempts=attempts))
    if read is None:
        raw = read_plan_choice_keypress().strip().lower()
    else:
        raw = read("> ").strip().lower()
    if raw == "":
        return "confirm"
    if raw == "e":
        return "edit"
    if raw == "c":
        return "chat"
    if raw in ("esc", "q", "cancel"):
        return "cancel"
    return raw  # let run_discussion's own "unrecognized choice" branch handle it


def _repl_edit_flow(plan: Plan, *, read: callable = input, console: Console | None = None) -> Plan:
    """
    Drives the [e] direct-edit sub-flow (rewritten for the open-ended
    architecture — see discussion.edit_command's own docstring): shows the
    current command text and lets the user type a replacement outright.

    Design note: discussion.run_discussion's own "edit" branch just
    continues the loop and expects the caller to have already applied the
    edit before the next get_user_choice call (see its docstring) -- this
    function is that caller-side piece. An empty replacement cancels the
    edit rather than leaving the plan with a blank command.
    """
    active_console = console if console is not None else themed_console()
    active_console.print(f"  [omsh.muted]Current command:[/omsh.muted] [omsh.accent]{plan.command}[/omsh.accent]")
    new_command = read("  New command (blank to cancel): ").strip()
    if not new_command:
        active_console.print("  [omsh.warning]Cancelled edit — plan unchanged.[/omsh.warning]")
        return plan
    try:
        return edit_command(plan, new_command)
    except ValueError:
        active_console.print("  [omsh.danger]Command cannot be empty — plan unchanged.[/omsh.danger]")
        return plan


def _handle_natural_language(
    text: str,
    cfg: dict,
    *,
    read: callable = input,
    choice_read: callable | None = None,
    sudo_input_fn: callable | None = None,
    console: Console | None = None,
    base_dir=None,
) -> None:
    """
    Full Section 4.1 pipeline (open-ended architecture — see
    validation.py's module docstring): Intent Parser -> Independent Risk
    Override -> Plan Generator -> Confirmation + Discussion Loop -> (Sudo
    Layer, only if a step needs it) -> Streaming Executor -> Audit Log.

    No Capability Registry is involved — the Intent Parser produces one
    real, directly-runnable command itself instead of action/params for a
    fixed set of registered capabilities (see intent_parser.py's own
    docstring).

    Wiring decisions:

    1. Intent Parser failure (backend unreachable) and "unmapped" both end
       the request here with a message -- nothing to plan or execute yet.
    2. Plan Generator runs once up front, then the Discussion Loop
       (discussion.run_discussion) drives confirm/edit/chat/cancel using
       the REPL callbacks above. "chat" reparses via parse_intent + a
       fresh generate_plan (the `reparse` callback below) -- the adjustment
       text is sent to the Intent Parser as a fresh request rather than
       appended to the original text, since intent_parser.parse_intent's
       contract only takes one user_message string; there is no
       "conversation history" concept in this module, so each chat-turn is
       its own independent parse. A multi-turn contextual adjustment (e.g.
       "no wait, just the Downloads folder") only works as well as the
       Intent Parser can infer from that fragment alone.
    3. On Cancelled(): the request ends, logged as status="cancelled" (no
       execution happened).
    4. On Confirmed(plan): run_plan() executes it, streaming StepEvents
       through ui/streaming.StreamingRenderer (a rich.Live spinner that
       collapses to a summary line per step) instead of a flat print per
       event. run_plan() is given prompt=RichSudoPrompt(console=...) so
       any step that hits a permission-denied retry shows the Section
       8.3.6 sudo box as a boxed rich.Panel via sudo_layer's own decision
       contract.
    5. Exactly one audit_log.record_action() call per request outcome,
       mapping ExecutionResult onto the audit schema:
         - all_done                      -> status="done"
         - interrupted                   -> status="interrupted"
         - aborted_for_sudo              -> status="cancelled" (abort at the
           sudo prompt is the user declining the rest of the plan -- the
           closest existing status; "aborted" isn't its own enum value per
           audit_log.py's VALID_STATUSES, confirmed in Step 12)
         - a SKIPPED step with otherwise no failures -> status="skipped"
         - anything else (a FAILED step)  -> status="failed"
       `used_sudo` is True if any step result used it; `duration_seconds`
       is wall-clock time around run_plan(); `detail` carries the last
       step's stderr/detail for a failed/interrupted outcome so /explain
       and /log have something concrete to show.
    """
    active_console = console if console is not None else themed_console()
    # Bordered-turn mode (see ui/theme.py's own module-level note): when
    # `console` is a bordered Console (built by `ui.theme.bordered_console`,
    # normally by run()'s own REPL loop), `Live`-driven rendering below
    # (run_with_thinking_indicator, StreamingRenderer) must use the real,
    # un-prefixed Console it carries as `.unbordered` instead -- printing
    # a Live display through the bar-prefixing wrapper produces visual
    # glitches (see that module's docstring). A plain Console (e.g. every
    # existing test's injected `console=`) has no such attribute, so
    # `getattr(..., active_console)` falls back to using it directly.
    live_console = getattr(active_console, "unbordered", active_console)

    # `run_with_thinking_indicator` (ui/thinking.py) drives the Section
    # 8.3.3 / Core Feature #14 live CPU/RAM/GPU indicator, with live token
    # counts and the raw JSON text streaming underneath it. `token_box` is
    # written by parse_intent()'s own on_token callback (one dict update
    # per streamed chunk, from intent_parser.GoogleAIStudioBackend) and
    # read every UI frame by the Live polling loop below -- see
    # run_with_thinking_indicator's own `on_token_box` docstring for why a
    # plain dict needs no lock here.
    token_box: dict[str, object] = {}

    def _on_token(progress) -> None:
        token_box["tokens_out"] = progress.tokens_out
        if progress.tokens_in is not None:
            token_box["tokens_in"] = progress.tokens_in
        token_box["text"] = progress.text_so_far

    try:
        result = run_with_thinking_indicator(
            lambda: parse_intent(text, on_token=_on_token),
            console=live_console,
            on_token_box=token_box,
        )
    except IntentParseError as exc:
        active_console.print(f"  [omsh.danger]⚠ Could not reach the model: {exc}[/omsh.danger]")
        return

    if result.intent is None:
        active_console.print("  [omsh.muted]I couldn't turn that into a command. Try rephrasing, or `/help`.[/omsh.muted]")
        return

    # Independent risk override (see danger_classifier.py's own docstring):
    # this session's own testing found both tested Gemini models reliably
    # under-risking port-opening and passwordless-user-creation -- never
    # trust the model's own risk field alone for an AI-generated command,
    # exactly as classify() already double-checks every raw-shell command.
    effective_risk = override_risk(result.intent.command, result.intent.risk)
    if effective_risk != result.intent.risk:
        result = dataclasses.replace(
            result, intent=result.intent.model_copy(update={"risk": effective_risk})
        )

    plan = generate_plan(result.intent)
    # Tracks the most recent parse's telemetry/attempts so the plan panel
    # (rendered by _repl_get_user_choice, below) always shows the numbers
    # for whichever parse actually produced the plan currently on screen --
    # the original parse, or the latest chat-adjust reparse once one has
    # happened.
    last_telemetry = result.telemetry
    last_attempts = result.attempts

    def _reparse(adjustment_text: str, current_plan: Plan) -> Plan | None:
        nonlocal last_telemetry, last_attempts
        reparse_token_box: dict[str, object] = {}

        def _on_reparse_token(progress) -> None:
            reparse_token_box["tokens_out"] = progress.tokens_out
            if progress.tokens_in is not None:
                reparse_token_box["tokens_in"] = progress.tokens_in
            reparse_token_box["text"] = progress.text_so_far

        try:
            adjusted = run_with_thinking_indicator(
                lambda: parse_intent(adjustment_text, on_token=_on_reparse_token),
                console=live_console,
                on_token_box=reparse_token_box,
            )
        except IntentParseError:
            return None
        if adjusted.intent is None:
            return None
        adjusted_risk = override_risk(adjusted.intent.command, adjusted.intent.risk)
        if adjusted_risk != adjusted.intent.risk:
            adjusted = dataclasses.replace(
                adjusted, intent=adjusted.intent.model_copy(update={"risk": adjusted_risk})
            )
        last_telemetry = adjusted.telemetry
        last_attempts = adjusted.attempts
        return generate_plan(adjusted.intent)

    def _get_user_choice(current_plan: Plan) -> tuple[str, Plan]:
        # `choice_read` defaults to None, which makes _repl_get_user_choice
        # use its own real single-keypress reader (see that function's
        # "Esc bug fix" docstring) instead of the REPL's ordinary line-read
        # `read` -- only that path actually binds the real Esc key. Tests
        # (and any other caller that needs to script this specific choice)
        # can still override via `choice_read=`. `_repl_edit_flow`'s own
        # sub-prompts keep using `read` normally, since editing a param's
        # value is a genuine typed-line read, not a single-keypress choice.
        choice = _repl_get_user_choice(
            current_plan,
            read=choice_read,
            console=active_console,
            telemetry=last_telemetry,
            attempts=last_attempts,
        )
        if choice == "edit":
            current_plan = _repl_edit_flow(current_plan, read=read, console=active_console)
        return choice, current_plan

    def _discussion_print(line: str) -> None:
        # run_discussion's own print_fn calls (diff-notes, soft-limit nudge,
        # "couldn't apply that adjustment", "unrecognized choice") are short
        # single lines -- rendered dim rather than boxed, so they read as
        # secondary/system text next to the boxed plan panel above them.
        active_console.print(f"[omsh.muted]{line}[/omsh.muted]" if line.strip() else line)

    outcome = run_discussion(
        plan,
        get_user_choice=_get_user_choice,
        get_adjustment_text=lambda: read("  What would you like to change? "),
        reparse=_reparse,
        print_fn=_discussion_print,
    )

    if isinstance(outcome, Cancelled):
        active_console.print("  [omsh.warning]Cancelled.[/omsh.warning]")
        audit_log_module.record_action(
            action=result.intent.command, source="natural_language", status="cancelled",
            risk=result.intent.risk, base_dir=base_dir,
        )
        return

    assert isinstance(outcome, Confirmed)
    confirmed_plan = outcome.plan

    start = time.time()
    with StreamingRenderer(console=live_console) as renderer:
        execution = run_plan(
            confirmed_plan,
            # `sudo_input_fn` defaults to None, which makes RichSudoPrompt
            # use its own real single-keypress reader (see that class's
            # "Esc/repeated-s bug fix" docstring) instead of the REPL's
            # ordinary line-read `read` -- only that path actually binds
            # the real Esc/s/q keys as immediate-submit. Tests can still
            # override via `sudo_input_fn=`.
            prompt=RichSudoPrompt(input_fn=sudo_input_fn, console=active_console),
            on_event=renderer.on_event,
            # Sudo password-prompt garbling bug fix: pause the Live spinner
            # for exactly the sudo-prefixed subprocess call (used_sudo=True
            # only) so its own repaint loop doesn't collide with `sudo`'s
            # real-terminal password prompt -- see StreamingRenderer's own
            # "Sudo password-prompt garbling bug fix" docstring comment for
            # the full explanation of the bug this fixes.
            on_before_execute=renderer.pause_for_sudo,
            on_after_execute=renderer.resume_after_sudo,
            # Real per-file live progress (added post-Build-Order, per the
            # user's explicit "make the whole shell feel alive, every
            # operation" request): streams the running command's stdout
            # live into the same spinner line, one real line at a time --
            # see ui/streaming.py's own module docstring and executor.py's
            # `_run_streaming` docstring for how this works without a
            # worker thread and without touching Ctrl+C handling.
            on_output_line=renderer.on_output_line,
        )
    # last_telemetry: the same ParseTelemetry already threaded into the
    # plan panel's own footer above -- the "AI: N tokens · Ns reasoning
    # time" execution-summary line (Section 8.3.4's mockup) uses the exact
    # same real numbers, not a second, separately-tracked figure.
    renderer.print_summary(execution, telemetry=last_telemetry)
    duration = time.time() - start

    used_sudo = any(r.used_sudo for r in execution.step_results)
    last_detail = execution.step_results[-1].stderr or execution.step_results[-1].description if execution.step_results else ""

    if execution.all_done:
        status = "done"
    elif execution.interrupted:
        status = "interrupted"
    elif execution.aborted_for_sudo:
        status = "cancelled"
    elif any(r.status is StepStatus.SKIPPED for r in execution.step_results):
        status = "skipped"
    else:
        status = "failed"

    audit_log_module.record_action(
        action=confirmed_plan.command,
        source="natural_language",
        risk=confirmed_plan.risk,
        status=status,
        duration_seconds=duration,
        used_sudo=used_sudo,
        detail=last_detail,
        base_dir=base_dir,
    )


def _handle_slash_command(
    text: str, cfg: dict, session_start: float, *, console: Console | None = None
) -> bool:
    """
    Dispatch a slash command through the full Meta-Command Handler
    (meta_commands.py, Step 14). Returns True if the REPL should exit.

    A MetaCommandError (recognized command, bad usage, or genuinely
    unrecognized command) is caught and printed rather than crashing the
    REPL — the same fail-soft spirit used throughout this codebase (e.g.
    the Danger Classifier's own LLM-failure handling). The print call is
    routed through the shared Console so its output lands in the same
    stream/buffer as everything else this module renders (important for
    tests that capture one Console's buffer).
    """
    active_console = console if console is not None else themed_console()

    # /update shells out to git/pip (update_check.run_self_update), which is
    # outside the thin-wrapper scope meta_commands.py's own module docstring
    # describes for every other command, so it's intercepted here rather
    # than added to that dispatch table.
    command = text.strip().lstrip("/").split()[0].lower() if text.strip() else ""
    if command == "update":
        # __file__ -> src/ohmyshell/main.py, so three parents up is the
        # repo root. Relies on install.sh's own `pip install -e` (editable
        # install), which keeps __file__ pointing at the real checkout
        # instead of a copied site-packages tree.
        repo_root = Path(__file__).resolve().parent.parent.parent
        active_console.print("  [omsh.muted]Checking for updates...[/omsh.muted]")
        succeeded, message = update_check_module.run_self_update(repo_root)
        style = "omsh.success" if succeeded else "omsh.danger"
        active_console.print(f"  [{style}]{message}[/{style}]")
        return False

    try:
        outcome = meta_commands.dispatch(text, cfg=cfg, session_start=session_start)
    except meta_commands.MetaCommandError as exc:
        active_console.print(f"  [omsh.danger]{exc}[/omsh.danger]")
        return False

    if outcome.text:
        active_console.print(outcome.text, highlight=False)
    return outcome.should_exit


def run() -> None:
    """Entrypoint (see pyproject.toml's [project.scripts] and bin/oh-my-shell)."""
    # `--version`/`-v` short-circuits before anything else (wizard, config
    # load, the REPL itself) -- found missing via manual testing: without
    # this, `oh-my-shell --version` was silently swallowed as a natural-
    # language command by the REPL instead of doing what every other CLI
    # (git, node, python3) does with this flag. Printed with a bare
    # print() rather than through themed_console(), since this needs to
    # work (and exit fast) even if config/theme setup would itself fail.
    if len(sys.argv) > 1 and sys.argv[1] in ("--version", "-v"):
        print(f"oh-my-shell {update_check_module.current_version()}")
        return

    console = themed_console()

    # First run: collect and verify the Google AI Studio API key before
    # config.load() would otherwise silently create a default config.json.
    if wizard_module.should_run_wizard():
        wizard_module.run_wizard(print_fn=console.print)

    # Fish-greeting-noise fix (see `ensure_fish_guard_installed`'s own
    # docstring for the full story) -- idempotent, so this runs on every
    # startup but only actually edits the person's config.fish once, ever.
    ensure_fish_guard_installed()

    cfg = config_module.load()
    session_start = time.time()
    session = ReplSession()

    # Started as early as possible so the GitHub Releases lookup has the
    # most time to finish before pending_update() is read a few lines down
    # -- it's still a best-effort check either way (see update_check.py's
    # own module docstring: any failure or a not-yet-finished check just
    # means the notice is skipped for this session, never a delay here).
    update_check_thread = update_check_module.check_for_update_async()

    console.print(_BANNER)

    first_loop = True
    while True:
        # A blank line before every prompt: without it, one command's
        # output runs directly into the next "<folder> ❯ " line with no
        # visual separation. One seam here, in the one place every turn
        # passes through, is enough to separate turns without scattering
        # spacing concerns across every handler.
        console.print()

        # Checked right before the very first prompt rather than
        # immediately after check_for_update_async() up above -- the
        # wizard/fish-guard/config-load steps between that call and here
        # give the background GitHub Releases lookup a head start, but
        # that head start alone was measured (on a real machine) to be
        # only a few milliseconds -- nowhere near enough for a ~1-2s
        # network call. So the check here waits up to
        # UPDATE_CHECK_JOIN_TIMEOUT_SECONDS for the thread to finish
        # before reading its result: bounded, so a slow/offline network
        # still can't hang the prompt, but long enough that a normal
        # GitHub API response is actually caught most of the time.
        if first_loop:
            first_loop = False
            update_check_thread.join(timeout=UPDATE_CHECK_JOIN_TIMEOUT_SECONDS)
            latest_version = update_check_module.pending_update()
            if latest_version is not None:
                # A boxed, warning-bordered panel rather than a single
                # muted line -- the plain-text version was easy to miss
                # right next to the banner and the first prompt (found via
                # real-terminal testing), so this now uses the same
                # warning-severity panel style as render_sudo_panel's own
                # "notice, not an error" elevated-permission box.
                console.print(render_update_available_panel(latest_version=latest_version))
                console.print()

        try:
            raw_input_text = session.prompt(render_prompt_ansi(cfg))
        except (EOFError, KeyboardInterrupt):
            console.print()  # keep the terminal cursor on a clean line
            break

        routed = route(raw_input_text)

        if routed.kind == InputKind.EMPTY:
            continue

        # Bordered-turn mode (see ui/theme.py's own module-level note for
        # the full design and why a true alternate-screen full-screen app
        # was ruled out in favor of this): every dispatched turn's static
        # output gets a fresh `bordered_console()` built per turn from the
        # REPL's own real `console`, so each turn's own left-accent bar
        # starts and ends cleanly around just that turn's output in the
        # scrollback, the same visual seam the blank-line spacing above
        # this loop already gives each turn's start.
        turn_console = bordered_console(console)

        # Only the top-level `session.prompt()` read above is wrapped for
        # KeyboardInterrupt automatically -- every mid-command confirmation
        # read a dispatched handler does itself (the destructive-command
        # [y/n/t] prompt in _handle_raw_shell, the plan discussion loop's
        # [Enter/e/c/Esc] and its own edit sub-prompts, the sudo
        # [Enter/s/Esc] prompt) needs its own handling. Pressing Ctrl+C at
        # any of those prompts is a natural "actually, never mind" reflex,
        # and without catching it here it would propagate out of this loop
        # as an unhandled exception, killing the whole interactive session
        # rather than just cancelling the one pending action. Catching it
        # here, around each dispatched handler individually (not by
        # widening the top-level prompt's own except, which has different
        # EOF/exit semantics -- Ctrl+D there legitimately means "end the
        # session"), cancels just that action and returns to the ordinary
        # REPL prompt instead.
        try:
            if routed.kind == InputKind.SLASH_COMMAND:
                if _handle_slash_command(routed.text, cfg, session_start, console=turn_console):
                    break
                continue
            if routed.kind == InputKind.RAW_SHELL:
                _handle_raw_shell(routed.text, cfg, confirm=session, console=turn_console, nl_read=session)
                continue
            if routed.kind == InputKind.NATURAL_LANGUAGE:
                _handle_natural_language(routed.text, cfg, read=session, console=turn_console)
                continue
        except KeyboardInterrupt:
            console.print()
            console.print("  [omsh.warning]Cancelled.[/omsh.warning]")
            continue


if __name__ == "__main__":
    run()