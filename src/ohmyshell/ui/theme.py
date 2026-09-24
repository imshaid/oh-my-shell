"""
Fixed UI accent palette.

This app's own chrome (the plan panel's border, the prompt icon, the
banner, risk labels, status glyphs) uses a consistent, recognizable
"brand" look rather than blending into whatever 16-color palette the
terminal's theme happens to remap "cyan" or "yellow" to. Every fixed
color below is a specific truecolor hex value (not a named ANSI color),
applied only to oh-my-shell's own UI chrome -- raw command output
(`ui/streaming.py`'s `_decode_ansi_block`, turning a running command's
own captured ANSI escapes into styled `Text`) is untouched by this
module and continues to come from `LS_COLORS`/the tool's own choices,
staying system-theme-respecting.

Requires a terminal that supports truecolor (24-bit ANSI, `COLORTERM=
truecolor` or `24bit`) to render the exact hex shades; `rich.console.
Console` degrades a truecolor style to the nearest 256-color/16-color
approximation automatically on a terminal that doesn't advertise
truecolor support, so this is safe to use unconditionally.

The palette is mapped onto the Nord color palette
(https://www.nordtheme.com/docs/colors-and-palettes) -- Nord's own named
hex values for the semantic roles (errors/warnings/success/accent) this
app needs. Nord's "Aurora" accent colors are tuned to read well against
Nord's own "Polar Night" dark backgrounds, and stay reasonably legible on
other dark themes too since they're moderate/muted rather than neon.

No fixed background is set by this module -- oh-my-shell's own chrome
renders with these foreground colors directly against whatever
background the terminal already has, the same principle applied to raw
command output above.

MUTED/ACCENT_DIM uses Nord4 (`#D8DEE9`, "Snow Storm") rather than Nord3
(`#4C566A`): Nord3 is the shade Nord's own docs/mockups use for dim text
against Nord's light "Snow Storm" surfaces, not a dark background, where
it reads as too low-contrast. Nord4 stays visibly dimmer than the
saturated Aurora/Frost accents elsewhere in this palette while remaining
legible on a dark terminal.
"""

from __future__ import annotations

from rich.theme import Theme

# Brand accent -- oh-my-shell's own signature color, used for its normal
# (non-alert) chrome: the default prompt icon, plan-panel borders,
# neutral confirmation panels, the startup banner accent.
# Nord10 ("Frost", the deeper of Nord's two primary blues) -- distinct
# enough from nord8/nord9 (used elsewhere in this palette) to read as
# this app's own signature color.
ACCENT = "#5E81AC"
# Nord4 (`#D8DEE9`, "Snow Storm") -- dim/secondary accent; see module
# docstring for why Nord3 isn't used here (too low-contrast on dark
# backgrounds).
ACCENT_DIM = "#D8DEE9"

# AI-active state (prompt icon while a natural-language request is being
# parsed/planned) -- a second, distinct hue so it reads as "something is
# actively happening" separately from the resting accent color above.
# Nord15 ("Aurora" purple) -- Nord's own color for "numbers and uncommon
# functionality," repurposed here for the same role.
ACTIVE = "#B48EAD"

# Semantic status colors -- Nord's own Aurora accent colors, used for
# exactly the roles Nord itself defines them for.
SUCCESS = "#A3BE8C"      # Nord14 -- Nord's own success/string green
WARNING = "#EBCB8B"      # Nord13 -- Nord's own warning/escape-character yellow
DANGER = "#BF616A"       # Nord11 -- Nord's own error/deletion red
MUTED = "#D8DEE9"        # Nord4 -- dim/secondary text (footers, hints, timestamps)

# Folder name in the prompt -- kept distinct from the accent color so the
# two pieces of the prompt (location vs. the app's own icon) read as
# separate visual elements at a glance.
# Nord8 ("Frost", Nord's own primary accent blue -- function declarations
# in Nord's own syntax-highlighting role) -- brighter and more saturated
# than ACCENT's Nord10, giving the two blues visible separation.
PATH = "#88C0D0"


# A `rich.theme.Theme` mapping semantic style *names* (not raw colors) to
# the fixed hex values above. Every call site in ui/panels.py, ui/prompt.py,
# ui/streaming.py should use these names (e.g. style="omsh.accent") instead
# of a literal color string, so the whole app's palette lives in exactly
# one place and can be retuned by editing only this file.
OMSH_THEME = Theme(
    {
        "omsh.accent": ACCENT,
        "omsh.accent_dim": ACCENT_DIM,
        "omsh.active": ACTIVE,
        "omsh.success": SUCCESS,
        "omsh.warning": WARNING,
        "omsh.danger": DANGER,
        "omsh.muted": MUTED,
        "omsh.path": PATH,
        # Risk-level text (ui/panels.py's _risk_text) -- same three shades
        # as the semantic colors above, named separately so a future
        # change to "what green/red mean generally" doesn't silently also
        # change "what risk:low/high look like" if the two ever need to
        # diverge.
        "omsh.risk.low": SUCCESS,
        "omsh.risk.medium": WARNING,
        "omsh.risk.high": DANGER,
    }
)


def themed_console(*args, **kwargs):
    """
    `rich.console.Console` pre-loaded with `OMSH_THEME`, so every
    "omsh.*" style name used anywhere this Console prints resolves to the
    fixed hex values above regardless of the caller's own terminal theme.
    Every `Console()` construction in ui/panels.py, ui/prompt.py,
    ui/streaming.py, and main.py should go through this function instead
    of calling `rich.console.Console()` directly -- same call signature
    (accepts and forwards all the same *args/**kwargs Console itself
    does, e.g. `file=`, `force_terminal=`, `width=` for tests), so this is
    a drop-in replacement.

    No forced background is applied here (see this module's own top
    docstring). This app's chrome renders with `OMSH_THEME`'s foreground
    colors directly against whatever background the terminal already has.
    """
    from rich.console import Console

    kwargs.setdefault("theme", OMSH_THEME)
    return Console(*args, **kwargs)


# --- Per-turn left-border ("bordered turn") mode ---
#
# A true single alternate-screen-buffer full-screen app (what vim/htop do)
# would break scrollable history, since alternate-screen mode has its own
# separate buffer a terminal's normal scrollback/mouse-wheel can't reach.
# Instead, each REPL turn (one prompt + everything that turn produces)
# gets its own persistent left-accent bar down the terminal's scrollback,
# printed once per turn, never redrawn.
#
# Scope, deliberately narrower than "wrap literally everything": this
# bar wraps only ordinary (non-`rich.live.Live`) output -- the plan/
# destructive/sudo panels, the execution summary text, banners, plain
# status lines. It does not wrap `StreamingRenderer`'s or
# `run_with_thinking_indicator`'s `Live`-driven spinner/progress
# rendering, because `Live`'s own in-place redraw sequences use `\r`
# (carriage return) + ERASE_IN_LINE cursor-control codes mid-block, not
# just line-by-line `\n`-terminated writes -- a line-prefixing stream
# wrapper like this one cannot reliably tell "start of a new visual line"
# from "mid-redraw cursor repositioning" from raw bytes alone. This stays
# scoped to static output only, the safe, verified case.
class _BarWriter:
    """
    A file-like wrapper that prepends a styled left-accent bar to every
    line written through it, without altering the bytes/content
    otherwise -- used to give one REPL turn's worth of ordinary (non-Live)
    output a persistent visual left border in the terminal's own
    scrollback, the same way each git-diff hunk or blockquote gets a
    left-margin marker in many tools.

    Treats a bare `\\r` (carriage return, no following `\\n`) as a line
    start too, not just `\\n` -- needed for `rich`'s own cursor-control
    sequences elsewhere in the app that use `\\r` to return to column 0
    without starting a new terminal line. This wrapper is only ever used
    around static (non-Live) output in practice, but handling `\\r`
    correctly here keeps this class correct as a general-purpose
    primitive.
    """

    def __init__(self, real_file, *, bar: str) -> None:
        self._real_file = real_file
        self._bar = bar
        self._at_line_start = True

    def write(self, s: str) -> int:
        out: list[str] = []
        for ch in s:
            if ch == "\r":
                self._at_line_start = True
                out.append(ch)
                continue
            if self._at_line_start and ch != "\n":
                out.append(self._bar)
                self._at_line_start = False
            out.append(ch)
            if ch == "\n":
                self._at_line_start = True
        return self._real_file.write("".join(out))

    def flush(self) -> None:
        self._real_file.flush()

    def isatty(self) -> bool:
        return self._real_file.isatty()


def bordered_console(real_console, *, bar_style: str = "omsh.accent_dim") -> "Console":
    """
    Build a second `Console` that writes through `real_console`'s own
    output file, wrapped in `_BarWriter` so everything printed to IT
    (not to `real_console` directly) gets the left-accent-bar treatment.

    `real_console` is unchanged and still usable directly (e.g. for
    `Live`-driven rendering that must NOT get the bar -- see this
    module's own scope note above); this returns a SEPARATE Console
    object for call sites that want the bordered look, sharing the same
    underlying terminal file so output from both interleaves correctly
    in real time (no buffering mismatch between the two).

    `bar_style` resolves through `OMSH_THEME` (this Console carries the
    same theme as `themed_console()`), defaulting to the dimmer accent
    shade so the border reads as a quiet margin marker, not a second loud
    accent competing with the panels it's wrapping.

    No forced background here (see this module's own top docstring). This
    Console's panels render with `OMSH_THEME`'s foreground colors directly
    against the terminal's own background, same as `themed_console()`.
    """
    from rich.console import Console
    from rich.style import Style

    # Render the bar character's own ANSI escape once, directly (not
    # through markup), so `_BarWriter` can prepend a plain string to raw
    # write() calls without needing a second Console round-trip per line.
    style = OMSH_THEME.styles.get(bar_style, Style())
    probe = Console(file=None, force_terminal=True, color_system="truecolor", theme=OMSH_THEME)
    with probe.capture() as capture:
        probe.print("▏", style=style, end="")
    bar = capture.get() + " "

    # The bar itself ("▏ ") occupies 2 real terminal columns that aren't
    # available to whatever this Console renders -- subtracted from the
    # width handed to the new Console so wrapped text (long lines,
    # panels) stays within the REAL terminal's width once the bar prefix
    # is added back on each line, rather than wrapping as if the full
    # terminal width were available and then overflowing once the bar is
    # prepended.
    bar_visible_width = 2
    inner_width = None
    if real_console.is_terminal:
        inner_width = max(1, real_console.size.width - bar_visible_width)

    wrapped_file = _BarWriter(real_console.file, bar=bar)
    console = themed_console(
        file=wrapped_file,
        force_terminal=real_console.is_terminal,
        color_system="truecolor" if real_console.is_terminal else None,
        width=inner_width,
    )
    # `unbordered`: the plain `real_console` this bordered Console was
    # built from, stashed as a plain attribute so a caller holding only
    # the bordered Console (e.g. deep inside `_handle_natural_language`,
    # which is handed one `console=` parameter for the whole turn) can
    # still reach the real, un-prefixed Console for anything that must not
    # get the bar treatment -- `StreamingRenderer`/
    # `run_with_thinking_indicator`'s `Live`-driven rendering (see this
    # module's own scope note above). A plain `Console()` (not built via
    # this function) has no such attribute; callers use
    # `getattr(console, "unbordered", console)` to fall back to the
    # console itself in that case.
    console.unbordered = real_console
    return console