"""
rich-styled REPL prompt string (Build Order Step 11).

Replaces main.py's plain-text `_render_prompt()` (Step 6) with a colorized
version, and adds the icon-swap-on-AI-request behavior Section 8.3.1
mentions in passing at the Module 4 responsibility line ("Prompt design
(folder-name display, model-tag, inline icon-change on AI-request)").

--- Disclosed gap (Section 16 Rule 5) ---
As documented in ui/panels.py's module docstring, Sections 8.3.1-8.3.2 of
the blueprint were never actually captured in this project's transcript
(a gap discovered and confirmed with the user before writing this module).
The *exact* mockup for the icon change -- which glyph it swaps to, and the
precise trigger moment -- is not available verbatim. What IS confirmed,
verbatim, elsewhere in the blueprint:
  - the baseline prompt format is `<folder> ❯ ` (seen repeatedly across
    Sections 8.3.5/8.3.8/8.3.9's examples, e.g. "downloads ❯ ls -la"),
  - main.py's own Step 6 docstring already documents this as "folder name
    display, model-tag" and defers "icon-change on AI-request" to this step.

Design choice made here (my own, adjustable once 8.3.1's real text is
available): the prompt swaps its icon from the default `❯` to `✦` while a
natural-language request is being parsed/planned/executed (i.e. anything
that goes through the Intent Parser, not a raw shell command or slash
command) -- a simple, visually distinct "AI is involved right now" signal,
matching the spirit of the Module 4 responsibility line without inventing
specific colors/characters the blueprint doesn't state. This is exposed as
an explicit `ai_active: bool` parameter rather than any global/implicit
state, so it stays trivially testable and main.py's REPL loop controls
exactly when it's true (only around the natural-language handling branch).
"""

from __future__ import annotations

from pathlib import Path

from rich.text import Text

from ohmyshell import config as config_module

DEFAULT_ICON = "❯"
AI_ACTIVE_ICON = "✦"


def render_prompt(cfg: dict, *, ai_active: bool = False, cwd: Path | None = None) -> Text:
    """
    Build the rich-styled prompt (folder name, optional model tag, icon).

    Mirrors main.py's plain-text `_render_prompt()` exactly in content
    (folder name only, not full path; model tag shown only when the active
    model differs from the default), adding color and the AI-active icon
    swap on top.
    """
    folder_name = (cwd if cwd is not None else Path.cwd()).name or "/"
    active_model = config_module.get(cfg, "model.active")
    default_model = config_module.default_config()["model"]["active"]

    icon = AI_ACTIVE_ICON if ai_active else DEFAULT_ICON
    icon_style = "magenta" if ai_active else "cyan"

    text = Text()
    text.append(folder_name, style="bold blue")
    if active_model != default_model:
        text.append(f" ({active_model})", style="dim")
    text.append(" ")
    text.append(icon, style=icon_style)
    text.append(" ")
    return text


def render_prompt_plain(cfg: dict, *, ai_active: bool = False, cwd: Path | None = None) -> str:
    """Plain-string form of render_prompt(), for callers that need `input()`'s
    prompt argument (which only accepts str, not a rich renderable) -- rich
    styling is applied by printing render_prompt() separately when a full
    Console-driven prompt (Step 11's real terminal) is available; this is
    the fallback used by input()-based call sites."""
    return render_prompt(cfg, ai_active=ai_active, cwd=cwd).plain