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

Note (post-Build-Order, Step 11 wiring pass): main.py's real REPL loop
does NOT currently drive `ai_active=True` at any point -- see main.py's own
module docstring for why (a single blocking `.prompt()` call has no later
moment to redraw mid-request). `ai_active` stays here, tested and ready,
for whenever the REPL read loop grows a live-redraw mechanism.

--- Fixed accent palette (post-Build-Order, user-requested) ---
The folder name, default icon, and AI-active icon used to be `rich` NAMED
colors ("bold blue", "cyan", "magenta") -- replaced with ui/theme.py's
fixed `"omsh.*"` style names (`omsh.path`, `omsh.accent`, `omsh.active`)
for the same reason panels.py's border colors were: a consistent,
terminal-theme-independent "brand" look for this app's own chrome. See
ui/theme.py's module docstring for the full rationale.
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
    # Bug fix (post-Build-Order, found via real-terminal testing): a
    # composite style STRING that mixes a plain attribute with a
    # ui/theme.py "omsh.*" theme-registered name -- "bold omsh.path" --
    # silently renders completely unstyled (no color, no bold, no escape
    # codes at all) instead of raising or falling back to just the color.
    # Root cause, confirmed directly against rich's own Console.get_style:
    # a single theme name ("omsh.path" alone) resolves fine (rich looks it
    # up in the Console's Theme), but the multi-token parser rich uses for
    # a composite style string tries to parse the WHOLE remaining token
    # ("omsh.path") as a literal color name once it's seen "bold" isn't a
    # color -- and a theme-registered name that isn't also a valid raw
    # color string (rich/CSS color keyword or hex) makes that parse fail.
    # Text's own render path catches that failure and silently drops the
    # whole style rather than crashing (found by reproducing the same
    # composite string directly through console.get_style(), which DOES
    # raise rich.errors.MissingStyle) -- so this bug never crashed
    # anything, it just silently rendered plain text, which is why it
    # wasn't caught by the test suite until compared against a real
    # terminal by eye. Fix: build a real `rich.style.Style` object by
    # combining the plain attribute with the theme name resolved through
    # `OMSH_THEME` directly, instead of asking rich to parse a mixed
    # string -- this sidesteps rich's parser entirely for the composite
    # case. `ui/thinking.py`'s "omsh.accent dim" and main.py's banner
    # markup `[bold omsh.accent]...[/bold omsh.accent]` had the exact same
    # bug, fixed the same way.
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
    `ANSI()` wrapper (Step 11's full rich prompt engine -- see
    ui/session.py). rich already knows how to turn a Text into raw ANSI
    escapes via a Console capture; reusing that here means the color
    palette (folder=omsh.path, default icon=omsh.accent, AI-active
    icon=omsh.active -- ui/theme.py's fixed hex values) stays defined in
    exactly one place (render_prompt above, via ui.theme.themed_console)
    rather than being re-encoded as a second, parallel prompt_toolkit
    style sheet.

    `color_system="truecolor"` (changed from "standard", post-Build-Order):
    ui/theme.py's fixed accent colors are specific truecolor hex values,
    not one of the 16 standard ANSI colors -- "standard" would have
    silently snapped each one to its nearest 16-color approximation
    (losing the whole point of a specific, consistent brand hue) every
    time this function builds its own throwaway Console instead of
    reusing the real terminal's actual negotiated capability.
    """
    from ohmyshell.ui.theme import themed_console

    console = themed_console(force_terminal=True, color_system="truecolor", no_color=False)
    with console.capture() as capture:
        console.print(render_prompt(cfg, ai_active=ai_active, cwd=cwd), end="")
    return capture.get()