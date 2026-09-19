"""
Shell REPL loop, entrypoint (Build Order Step 6, danger-check integrated
Step 8, full Meta-Command Handler wired in Step 14 — the final Build Order step).

Per the Build Order's own scoping for Step 6 ("basic REPL loop, raw
pass-through প্রথমে — সবচেয়ে সহজ path"), this was initially NOT the full
Section 4.1 pipeline. Later steps filled in what was missing:

- RAW_SHELL input is classified (danger_classifier.py, Step 8) before
  running. A Destructive verdict shows Section 8.3.5's confirmation prompt
  ([y]/[n] only — [t] "move to trash instead" still isn't wired into this
  prompt even though trash.py exists as of Step 10; wiring executor.py/
  trash.py's [t] option and the Plan Generator/Sudo Layer/Executor into the
  NATURAL_LANGUAGE path is Section 4.1's full pipeline and remains future
  work beyond Step 14's own scope, which is specifically "slash-commands,
  thin wrapper over existing core logic" per its Section 10.1 line — not a
  general invitation to wire up the whole pipeline).
- NATURAL_LANGUAGE input is parsed (Step 5's intent_parser) and the
  resulting intent/risk is shown to the user, but still NOT executed —
  unchanged from Step 6/8's behavior; see the note above.
- SLASH_COMMAND input now goes through the full Meta-Command Handler
  (meta_commands.py, Step 14) — /help, /model, /history, /undo, /trash,
  /log, /capabilities, /explain, /stats, /system, /config, /clear, /exit,
  /quit are all recognized. meta_commands.dispatch() is a thin wrapper
  over already-built modules per its own docstring; main.py's job here is
  just calling it and printing/acting on the result.
- Prompt rendering here is still plain text (folder name, model tag if
  non-default) — the `rich`-based visual polish from ui/prompt.py (Step 11)
  exists but isn't wired into this REPL's `input()` call, since `input()`
  only accepts a plain string prompt; adopting ui/prompt.py fully would
  need replacing `input()` with a `rich`/`prompt_toolkit`-driven read loop,
  which is a UI-framework change beyond what Step 14's "thin wrapper" scope
  covers.

`--yes`/`-y`/`--dry-run`/`--verbose`/`--quiet` (Section 8.5) are not
implemented anywhere yet — they're inline modifiers on a natural-language
request, which isn't executed at all yet (see above), so there's nothing
for them to modify. The one hard constraint that DOES apply today --
`--yes`/`-y` must never bypass a destructive-command confirmation -- is
already satisfied by construction in _handle_raw_shell, which has no
special-case for any flag in the command text.
"""

from __future__ import annotations

import shlex
import subprocess
import sys
import time
from pathlib import Path

from ohmyshell import config as config_module
from ohmyshell import meta_commands
from ohmyshell.danger_classifier import Destructive, DangerClassifierError, classify
from ohmyshell.intent_parser import IntentParseError, parse_intent
from ohmyshell.registry import Registry, RegistryError
from ohmyshell.registry import load as load_registry
from ohmyshell.router import InputKind, route

EXIT_COMMANDS = {"/exit", "/quit"}


def _render_prompt(cfg: dict) -> str:
    """
    Plain-text prompt: current folder name only (not full path), plus a
    model tag if the active model isn't the default (Section 8.3.1).
    The icon-swap-on-AI-request behavior from 8.3.1 needs live in-place
    terminal redraw, which belongs to ui/streaming.py (Step 11) — this
    step always shows the raw-command icon "❯".
    """
    folder_name = Path.cwd().name or "/"
    active_model = config_module.get(cfg, "model.active")
    default_model = config_module.default_config()["model"]["active"]
    tag = f" ({active_model})" if active_model != default_model else ""
    return f"{folder_name}{tag} ❯ "


def _handle_raw_shell(text: str, cfg: dict, *, confirm: callable = input) -> None:
    """
    Classify, then execute raw shell input directly (Section 8.3.5).

    Scope note (implementation decision): Section 8.3.5's confirmation
    prompt offers [y] Run anyway / [n] Cancel / [t] Move to trash instead.
    [t] is not offered yet — it needs the Trash/Undo Manager (Step 10),
    which doesn't exist yet, so this step only offers [y]/[n]. A
    Destructive verdict with trash_alternative_possible=True is exactly
    the case Step 10 will extend this confirmation prompt to also offer
    [t] for.

    Hard constraint (Section 8.4, line 853): nothing here inspects the
    command text for --yes/-y and skips the prompt on a destructive
    verdict — that flag has no special handling anywhere in this function,
    which is what makes it unable to bypass this confirmation.

    Fail-safe policy (this function's own choice, not the classifier's):
    if the LLM fallback itself fails (Ollama unreachable, etc.), the
    command is treated as unclassifiable and NOT run automatically — the
    user is told why and asked to re-run manually, rather than silently
    executing something that couldn't be checked.
    """
    try:
        result = classify(text, model=config_module.get(cfg, "model.active"))
    except DangerClassifierError as exc:
        print(f"  ⚠ Could not classify this command ({exc}) — not running it automatically.")
        print(f"  Re-run manually if you're sure: {text}")
        return

    if isinstance(result.verdict, Destructive):
        print("\n  ⚠ Potentially destructive command detected\n")
        print(f"  {result.verdict.explanation}\n")
        choice = confirm("  [y] Run anyway   [n] Cancel   ").strip().lower()
        if choice != "y":
            print("  Cancelled.")
            return

    _run_shell_command(text)


def _run_shell_command(text: str) -> None:
    """
    Actually execute a raw shell command. Uses shell=True so pipes/redirects/
    chaining the router already detected actually work; interactive
    full-screen programs (vim/top) are a pty hand-off concern for a later
    step — this is plain blocking subprocess execution.
    """
    try:
        subprocess.run(text, shell=True)
    except OSError as exc:
        print(f"error: {exc}", file=sys.stderr)


def _handle_natural_language(text: str, registry: Registry, cfg: dict) -> None:
    """
    Parse the request and show what would happen — does not execute
    anything. Plan Generator/Confirmation loop/Executor land in later
    steps; this is deliberately just a visibility checkpoint on the
    Step 5 pipeline.
    """
    model = config_module.get(cfg, "model.active")
    try:
        result = parse_intent(text, registry, model=model)
    except IntentParseError as exc:
        print(f"  ⚠ Could not reach the model: {exc}")
        return

    if result.intent is None:
        print("  I couldn't map that to anything I can do yet. Try `/help`.")
        return

    print(f"  → {result.intent.action}  (risk: {result.intent.risk})")
    if result.intent.params:
        for key, value in result.intent.params.items():
            print(f"      {key}: {value}")
    print("  (Execution isn't wired up yet — this is a preview of the parsed intent.)")


def _handle_slash_command(text: str, registry: Registry, cfg: dict, session_start: float) -> bool:
    """
    Dispatch a slash command through the full Meta-Command Handler
    (meta_commands.py, Step 14). Returns True if the REPL should exit.

    A MetaCommandError (recognized command, bad usage, or genuinely
    unrecognized command) is caught and printed rather than crashing the
    REPL — the same fail-soft spirit used throughout this codebase (e.g.
    the Danger Classifier's own LLM-failure handling).
    """
    try:
        outcome = meta_commands.dispatch(text, cfg=cfg, registry=registry, session_start=session_start)
    except meta_commands.MetaCommandError as exc:
        print(f"  {exc}")
        return False

    if outcome.text:
        print(outcome.text)
    return outcome.should_exit


def run() -> None:
    """Entrypoint (see pyproject.toml's [project.scripts] and bin/oh-my-shell)."""
    try:
        registry = load_registry()
    except RegistryError as exc:
        print(f"Fatal: could not load capability registry: {exc}", file=sys.stderr)
        sys.exit(1)

    cfg = config_module.load()
    session_start = time.time()

    while True:
        try:
            raw_input_text = input(_render_prompt(cfg))
        except (EOFError, KeyboardInterrupt):
            print()  # keep the terminal cursor on a clean line
            break

        routed = route(raw_input_text)

        if routed.kind == InputKind.EMPTY:
            continue
        if routed.kind == InputKind.SLASH_COMMAND:
            if _handle_slash_command(routed.text, registry, cfg, session_start):
                break
            continue
        if routed.kind == InputKind.RAW_SHELL:
            _handle_raw_shell(routed.text, cfg)
            continue
        if routed.kind == InputKind.NATURAL_LANGUAGE:
            _handle_natural_language(routed.text, registry, cfg)
            continue


if __name__ == "__main__":
    run()