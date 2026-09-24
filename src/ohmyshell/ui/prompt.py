"""
rich-styled REPL prompt string (Build Order Step 11).

Replaces main.py's plain-text `_render_prompt()` (Step 6) with a colorized
version, and adds an icon swap for when an AI request is active.

Baseline prompt format is `<folder> ❯ ` (Sections 8.3.5/8.3.8/8.3.9). The
prompt swaps its icon from the default `❯` to `✦` while a natural-language
request is being parsed/planned/executed — a raw shell command or slash
command never triggers it. This is an explicit `ai_active: bool` parameter
rather than global state, so main.py's REPL loop controls exactly when it's
true. main.py's current REPL loop doesn't set it (a single blocking
`.prompt()` call has no later moment to redraw mid-request); it stays here
for whenever the read loop grows a live-redraw mechanism.

Colors use ui/theme.py's fixed `"omsh.*"` style names (`omsh.path`,
`omsh.accent`, `omsh.active`) for a consistent, terminal-theme-independent
look, same as the rest of this app's chrome (see ui/theme.py).
"""

from __future__ import annotations

from pathlib import Path

from rich.style import Style
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
    icon_style = "omsh.active" if ai_active else "omsh.accent"

    text = Text()
    # A composite style string mixing a plain attribute with a theme-
    # registered "omsh.*" name (e.g. "bold omsh.path") silently renders
    # unstyled — rich's composite-string parser can't resolve a
    # theme-only name once it's seen a non-color token. Build a real
    # `rich.style.Style` by combining the attribute with the theme name
    # resolved through `OMSH_THEME` directly, bypassing that parser.
    from ohmyshell.ui.theme import OMSH_THEME

    folder_style = Style(bold=True) + OMSH_THEME.styles["omsh.path"]
    text.append(folder_name, style=folder_style)
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


def render_prompt_ansi(cfg: dict, *, ai_active: bool = False, cwd: Path | None = None) -> str:
    """
    ANSI-escaped string form of render_prompt(), for prompt_toolkit's
    `ANSI()` wrapper (see ui/session.py). rich turns a Text into raw ANSI
    escapes via a Console capture, so the color palette stays defined in
    exactly one place (render_prompt above) rather than a second, parallel
    prompt_toolkit style sheet.

    `color_system="truecolor"`: ui/theme.py's accent colors are specific
    truecolor hex values, not one of the 16 standard ANSI colors —
    "standard" would snap each one to its nearest 16-color approximation.
    """
    from ohmyshell.ui.theme import themed_console

    console = themed_console(force_terminal=True, color_system="truecolor", no_color=False)
    with console.capture() as capture:
        console.print(render_prompt(cfg, ai_active=ai_active, cwd=cwd), end="")
    return capture.get()