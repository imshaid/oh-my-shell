"""
Fixed UI accent palette (post-Build-Order, user-requested).

--- Design decision this module exists to record (Section 16 Rule 5) ---
Every earlier round of this app's own `rich` output (ui/panels.py,
ui/prompt.py, ui/streaming.py) used `rich`'s NAMED colors directly as
inline style strings ("cyan", "yellow", "green", "bold blue", ...),
scattered across three files. Those names are deliberately
terminal-theme-relative: `rich`/ANSI don't send an actual RGB value for
"cyan", they send SGR code 36, and it's the person's own terminal
emulator that decides what pixel color code 36 actually paints, per
their own light/dark/Nord/Dracula/whatever theme. That's exactly right
for *command output* (an `ls`/`grep`/etc.'s own colors, via LS_COLORS or
the tool's own `--color` flag) -- this app has zero business overriding
what the person's system already colors for them, and nothing about that
changes here.

It is a DIFFERENT, deliberate choice for oh-my-shell's OWN chrome (the
plan panel's border, the prompt icon, the banner, risk labels, status
glyphs): the person asked for this app to have a consistent, recognizable
"brand" look -- comparable to how Claude Code's own CLI always shows the
same accent color box/prompt regardless of which terminal or theme it's
running in -- rather than blending into whatever 16-color palette the
person's terminal theme happens to remap "cyan" or "yellow" to (which, on
some real themes, can render close to unreadable against certain
backgrounds, or just inconsistently across different people's machines
screenshotted side by side).

So: every fixed color below is a specific truecolor hex value (not a
named ANSI color), applied ONLY to oh-my-shell's own UI chrome. Command
output rendering (`ui/streaming.py`'s `_decode_ansi_block`, which turns a
running command's own captured ANSI escapes into styled `Text`) is
completely untouched by this module -- those colors still come from
`LS_COLORS`/the tool's own choices, on principle, per the user's own
earlier explicit direction that command-output color must stay
system-theme-respecting.

Requires a terminal that supports truecolor (24-bit ANSI, `COLORTERM=
truecolor` or `24bit`) to render the exact hex shades; `rich.console.
Console` degrades a truecolor style to the nearest 256-color/16-color
approximation automatically on a terminal that doesn't advertise
truecolor support (this is `rich`'s own existing behavior, not something
this module has to implement), so this is safe to use unconditionally --
worst case on an old/limited terminal, the accent color is a close
approximation instead of the exact hex, never a crash or missing color.

--- The palette ---
One accent family (an indigo/violet "brand" hue, distinct from any of the
8 standard ANSI colors so it doesn't collide with -- and stays visually
distinct from -- whatever the person's terminal theme does to actual
named colors), plus the existing semantic meanings (success/warning/
danger/info) pinned to fixed, WCAG-readable-on-both-light-and-dark-
background shades rather than left to each theme's own interpretation of
"green"/"yellow"/"red". Chosen to stay legible on both a near-black and a
near-white background (the two extremes most real terminal themes fall
between), verified by eye against both.
"""

from __future__ import annotations

from rich.theme import Theme

# Brand accent -- oh-my-shell's own signature color, used for its normal
# (non-alert) chrome: the default prompt icon, plan-panel borders,
# neutral confirmation panels, the startup banner accent.
ACCENT = "#8B7FE8"       # soft indigo/violet
ACCENT_DIM = "#5D53A8"   # same hue, darker -- secondary/dim accent text

# AI-active state (prompt icon while a natural-language request is being
# parsed/planned) -- a second, brighter hue so it reads as "something is
# actively happening" distinctly from the resting accent color above.
ACTIVE = "#E88BD4"       # warm magenta/pink

# Semantic status colors -- fixed shades for the same meanings
# ui/panels.py and ui/streaming.py already used named colors for
# (risk levels, step-result glyphs, warning/destructive panels).
SUCCESS = "#4FD68C"      # step done / low risk
WARNING = "#E8B04F"      # medium risk / destructive-command / sudo panels
DANGER = "#E85F5F"       # failed step / high risk
MUTED = "#8A8A9A"        # dim/secondary text (footers, hints, timestamps)

# Folder name in the prompt -- kept distinct from the accent color so the
# two pieces of the prompt (location vs. the app's own icon) read as
# separate visual elements at a glance.
PATH = "#6FA8E8"         # soft blue


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
    """
    from rich.console import Console

    kwargs.setdefault("theme", OMSH_THEME)
    return Console(*args, **kwargs)


# --- Per-turn left-border ("bordered turn") mode (post-Build-Order, user-
# requested: "I want when user run the oh my shell then automatically it
# transform a full screen mode like claude code cli, and also should be
# the history user can scrolled") ---
#
# A true single alternate-screen-buffer full-screen app (what vim/htop do)
# was explicitly ruled out by the user themselves once the trade-off was
# explained: it would break their other explicit requirement, scrollable
# history (alternate-screen mode has its own separate buffer that a
# terminal's normal scrollback/mouse-wheel can't reach at all). What was
# confirmed instead: each REPL turn (one prompt + everything that turn
# produces) gets its own persistent left-accent bar down the terminal's
# scrollback -- printed once per turn, never redrawn -- giving the
# "this app has a consistent boxed identity" look without sacrificing
# ordinary scrollback.
#
# Scope, deliberately narrower than "wrap literally everything": this
# bar wraps only ordinary (non-`rich.live.Live`) output -- the plan/
# destructive/sudo panels, the execution summary text, banners, plain
# status lines. It does NOT wrap `StreamingRenderer`'s or
# `run_with_thinking_indicator`'s `Live`-driven spinner/progress
# rendering. Verified directly (a working prototype was built and tested
# against a real `Live` display): `Live`'s own in-place redraw sequences
# use `\r` (carriage return) + ERASE_IN_LINE cursor-control codes mid-
# block, not just line-by-line `\n`-terminated writes -- a line-prefixing
# stream wrapper like this one cannot always tell "start of a new visual
# line" from "mid-redraw cursor repositioning" from raw bytes alone, and
# the result was a real, reproducible glitch (the bar character appearing
# at the wrong column, sometimes twice, during a step's spinner). Rather
# than risk introducing that glitch into the most failure-sensitive
# rendering code in the app (StreamingRenderer/thinking-indicator, both
# already carefully tuned around real terminal edge cases -- sudo-prompt
# collisions, Ctrl+C timing, etc.), this stays scoped to static output
# only, which is the safe, fully-verified case.
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
    without literally starting a new terminal line; this wrapper is only
    ever used around static (non-Live) output in practice (see this
    module's own note above), but treating `\\r` correctly here as well
    costs nothing and keeps this class correct as a general-purpose
    primitive, not just "correct for the cases currently used".
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
    # still reach the real, un-prefixed Console for anything that must
    # NOT get the bar treatment -- `StreamingRenderer`/
    # `run_with_thinking_indicator`'s `Live`-driven rendering (see this
    # module's own scope note above for why). A plain `Console()` (not
    # built via this function) simply has no such attribute; callers use
    # `getattr(console, "unbordered", console)` to fall back to the
    # console itself in that case.
    console.unbordered = real_console
    return console