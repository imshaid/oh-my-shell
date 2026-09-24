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
User-requested, THIRD major revision of this palette: the original
indigo/violet "brand" accent (chosen to stay distinct from any real
terminal theme) turned out to read as visually "off" against a real
Nord-themed terminal (the person's own daily setup), and a separate
attempt to paint a fixed full-window background behind it (so the
accent palette would have a guaranteed-consistent backdrop) was itself
abandoned after several rounds of real-terminal testing showed no
reliable, scrollback-safe way to keep a painted background in sync with
a continuously scrolling terminal (see git history / prior session
notes for that full investigation). The person's explicit direction
after that: give up the fixed-background approach entirely, and instead
choose colors that work WITH their actual terminal theme (Nord) rather
than fighting it with a competing fixed backdrop.

So this palette is now mapped directly onto the Nord color palette
(https://www.nordtheme.com/docs/colors-and-palettes) -- the same named
hex values Nord itself defines for exactly the semantic roles
(errors/warnings/success/accent) this app already needed, rather than
an invented brand hue. Nord's own "Aurora" accent colors are
deliberately tuned by the Nord project to already read well against
Nord's own "Polar Night" dark backgrounds -- since the person's real
terminal IS Nord-themed, using Nord's own colors means this app's
chrome will look native to their setup instead of clashing with it,
and stays reasonably legible on any other dark theme too since Nord's
Aurora colors are moderate/muted rather than neon.

No fixed background is set by this module at all anymore -- oh-my-shell's
own chrome renders with these foreground colors directly against
whatever background the person's own terminal already has, the same
principle this file's own command-output rule (below) already applied
to raw command output; the app's chrome and raw command output are now
both consistently "respect the person's own terminal", just for
different reasons (raw output because it was always meant to be
system-theme-respecting; the app's own chrome because fighting a real
terminal theme with a competing painted background was tried and
explicitly abandoned).

--- Follow-up fix: dim/muted text contrast (same session) ---
The first pass of this Nord remap used Nord3 (`#4C566A`) for every
dim/secondary role (panel footers, key-hint lines, hardware stats,
`/help` separators). A real screenshot from the person's own dark-navy
Ptyxis window showed that text as almost unreadable -- too little
contrast against a DARK background specifically. Root cause: Nord3 is
the shade Nord's own docs/mockups use for dim text sitting on Nord's
LIGHT "Snow Storm" surfaces, not on a dark background -- it was picked
for the wrong side of Nord's own light/dark split. Fixed by using Nord4
(`#D8DEE9`, "Snow Storm") for MUTED/ACCENT_DIM instead: still visibly
dimmer than the saturated Aurora/Frost accents used elsewhere in this
palette, but light enough to actually read against a dark terminal.
"""

from __future__ import annotations

from rich.theme import Theme

# Brand accent -- oh-my-shell's own signature color, used for its normal
# (non-alert) chrome: the default prompt icon, plan-panel borders,
# neutral confirmation panels, the startup banner accent.
# Nord10 ("Frost", the deeper of Nord's two primary blues) -- Nord's own
# tertiary-accent blue, distinct enough from nord8/nord9 (used elsewhere
# in this palette) to read as this app's own signature color rather than
# blending into ordinary Nord-themed syntax highlighting.
ACCENT = "#5E81AC"
# Nord3 (`#4C566A`) was tried first here since Nord's own docs call it the
# "comments/subtle UI" shade -- but Nord picks that shade to sit against
# Nord's LIGHT "Snow Storm" panels/gutters (its own editor mockups use it
# on nord4/nord5/nord6 backgrounds), not against a dark terminal's own
# near-black background. Confirmed against a real screenshot (the user's
# own Ptyxis window, a dark navy background close to nord0): Nord3 text
# there reads as almost invisible -- too little contrast against a DARK
# background specifically, even though it's a perfectly readable "muted"
# tone against a light one. Nord4 (`#D8DEE9`, "Snow Storm") is used
# instead for every dim/secondary role below -- still visibly dimmer than
# the saturated Aurora/Frost accent colors, but light enough to actually
# read against a dark terminal background.
ACCENT_DIM = "#D8DEE9"   # Nord4 -- dim/secondary accent (see note above)

# AI-active state (prompt icon while a natural-language request is being
# parsed/planned) -- a second, distinct hue so it reads as "something is
# actively happening" separately from the resting accent color above.
# Nord15 ("Aurora" purple) -- Nord's own color for "numbers and uncommon
# functionality," repurposed here for the same "something unusual/active
# is happening" role.
ACTIVE = "#B48EAD"

# Semantic status colors -- Nord's own Aurora accent colors, used for
# exactly the roles Nord itself defines them for.
SUCCESS = "#A3BE8C"      # Nord14 -- Nord's own success/string green
WARNING = "#EBCB8B"      # Nord13 -- Nord's own warning/escape-character yellow
DANGER = "#BF616A"       # Nord11 -- Nord's own error/deletion red
MUTED = "#D8DEE9"        # Nord4 -- dim/secondary text (footers, hints, timestamps); see ACCENT_DIM's note above for why Nord3 was replaced

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
    docstring, "THIRD major revision" -- the fixed-full-window-background
    approach was tried and explicitly abandoned after it proved
    impossible to keep reliably in sync with a scrolling terminal). This
    app's chrome renders with `OMSH_THEME`'s foreground colors directly
    against whatever background the person's own terminal already has.
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

    No forced background here (see this module's own top docstring,
    "THIRD major revision" -- the fixed-background approach was tried and
    explicitly abandoned). This Console's panels render with `OMSH_THEME`'s
    foreground colors directly against the person's own terminal
    background, same as `themed_console()`.
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