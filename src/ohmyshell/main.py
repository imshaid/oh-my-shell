"""
Shell REPL loop, entrypoint (Build Order Step 6).

Per the Build Order's own scoping for this step ("basic REPL loop, raw
pass-through প্রথমে — সবচেয়ে সহজ path"), this is intentionally NOT the full
Section 4.1 pipeline yet:

- RAW_SHELL input executes immediately via subprocess, with no danger
  check — the Danger Classifier is Build Order Step 8. Wiring that in
  will be a small, additive change to `_handle_raw_shell` once it exists;
  nothing here should be read as a security boundary yet.
- NATURAL_LANGUAGE input is parsed (Step 5's intent_parser) and the
  resulting intent/risk is shown to the user, but NOT executed — there is
  no Plan Generator (Step 7), Confirmation/Discussion loop (Step 7), Sudo
  Layer (Step 9), or Executor (Step 10) yet. Showing the parsed result
  without acting on it is this step's whole point: prove the pipeline up
  through validated-intent works before building the confirm/execute
  machinery on top of it.
- SLASH_COMMAND input only understands "/exit" (and "/quit" as a synonym)
  in this step. The full Meta-Command Handler is Step 14.
- Prompt rendering here is plain text (folder name, model tag if
  non-default) — the `rich`-based visual polish from Section 8.3.1 is
  Step 11 (ui/prompt.py, ui/panels.py).

None of this needs a `--no-ai` style flag yet (Section 8.5) since there's
no AI-execution path to disable — that flag becomes meaningful once the
Plan Generator/Executor exist.
"""

from __future__ import annotations

import shlex
import subprocess
import sys
from pathlib import Path

from ohmyshell import config as config_module
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


def _handle_raw_shell(text: str) -> None:
    """
    Execute raw shell input directly, no AI involvement, no danger check
    yet (Step 8 adds that in front of this call). Uses shell=True so
    pipes/redirects/chaining the router already detected actually work;
    interactive full-screen programs (vim/top) are a pty hand-off concern
    for a later step — this is plain blocking subprocess execution.
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


def _handle_slash_command(text: str) -> bool:
    """
    Returns True if the REPL should exit.

    Only /exit and /quit are understood in this step; everything else is
    reported as not-yet-available rather than silently ignored, so it's
    clear this isn't the full Meta-Command Handler (Step 14) yet.
    """
    command = shlex.split(text)[0].lower() if text.strip() else text
    if command in EXIT_COMMANDS:
        return True
    print(f"  Slash commands aren't fully wired up yet ({text!r}). Try /exit to quit.")
    return False


def run() -> None:
    """Entrypoint (see pyproject.toml's [project.scripts] and bin/oh-my-shell)."""
    try:
        registry = load_registry()
    except RegistryError as exc:
        print(f"Fatal: could not load capability registry: {exc}", file=sys.stderr)
        sys.exit(1)

    cfg = config_module.load()

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
            if _handle_slash_command(routed.text):
                break
            continue
        if routed.kind == InputKind.RAW_SHELL:
            _handle_raw_shell(routed.text)
            continue
        if routed.kind == InputKind.NATURAL_LANGUAGE:
            _handle_natural_language(routed.text, registry, cfg)
            continue


if __name__ == "__main__":
    run()