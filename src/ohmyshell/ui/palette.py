"""
Command palette (Step 11 visual-polish follow-up, post-Build-Order).

The person asked, after comparing a real session against Claude Code and
"Hermes Agent"'s own REPLs: "when press / then automatically show all the
commands with their short description like claude code or hermes agent."
Nothing in the blueprint specifies this exact interaction -- meta_commands.py
already IS the full command reference (Section 8.4's table, reproduced in
HELP_TEXT), so this module doesn't invent new commands, it only makes the
existing slash-command set discoverable as you type "/" instead of only via
`/help`.

Design notes (Section 16 Rule 5 -- this glue is this file's own decision):

- The command list below is a small, explicit table, not a parser over
  meta_commands.HELP_TEXT's formatted text. HELP_TEXT is prose meant for a
  human to read top-to-bottom; scraping short descriptions back out of it
  would be fragile (breaks the moment its wording changes) for no real
  gain -- meta_commands.py's own `dispatch()` function is still the single
  source of truth for what a command actually DOES, this table only carries
  the two things the palette needs (name, one-line description), kept next
  to each other here so they're easy to keep in sync with meta_commands.py's
  command set by eye.
- Only top-level command names are matched (e.g. "/model"), not their
  subcommands ("/model switch qwen3:8b") -- matching how Claude Code's own
  "/" palette works (it lists the command, not every argument), and because
  meta_commands.py's subcommand shapes vary too much (some take a fixed
  enum like /trash's status|keep|clear, others take a free-form model name
  or config key) to offer generically useful completions for.
- The list only appears when the line starts with "/" (checked by
  visible_commands itself) -- typing "/" in the middle of an ordinary
  natural-language sentence (e.g. "what does a/b mean") must not show a
  command list. Router.py's own INPUT classification already treats
  "starts with /" as the slash-command signal (see router.py); this module
  mirrors that exact rule rather than inventing a second one.

--- Round 3 (post-Build-Order, after two earlier real-terminal test rounds
found this still wasn't what was asked for) ---

Round 1 built this as a prompt_toolkit `Completer`, which renders as a
separate floating completion-menu widget -- a bordered box that pops up
below the cursor. Round 2 tried to reskin that box to match this app's own
colors, but that missed the actual ask twice over: (a) the person explicitly
said "not any additional popup or else" -- a floating menu widget is a
popup no matter what colors it uses, and Claude Code's own "/" list is not
a popup, it's plain lines printed inline under the input, using the
terminal's own ambient colors; (b) hardcoding *any* specific colors (even
ones picked to "match" this app) is wrong for a different reason the person
also raised directly: a real terminal's color scheme is chosen by the user
(their own theme, light or dark), not by this app, so baking in specific
hex colors will clash with whatever theme a given person is actually
running, exactly the nordic/dark-cyan scheme in the person's own screenshot
being one example, not the universal case.

This round replaces the whole `Completer`-based approach with
`prompt_toolkit.PromptSession`'s `bottom_toolbar` -- a plain text region
that redraws live under the input line as you type, with no border and
no menu chrome. `visible_commands(text)` below is the pure function that
decides what to show (empty list unless `text` starts with "/");
`render_toolbar_text(text)` turns that into the actual lines
ui/session.py's `bottom_toolbar` callable returns -- separated so each is
independently testable without constructing a real PromptSession.

--- Round 4 (post-Build-Order, "I want to add color in everywhere") ---
Round 3 above deliberately used NO color at all here -- the reverse-video
bar prompt_toolkit paints behind an unstyled bottom_toolbar by default
was mistaken, during that round, for "this app forcing a color," so the
fix at the time was to strip color entirely (`noreverse`, plain text).
Revisited now that the person has asked, explicitly and repeatedly, for
color everywhere in this app's own chrome ("I want to add color in
everywhere every types of operation") -- and clarified directly, when
asked whether this palette should use this app's own fixed Nord accent
palette (ui/theme.py) or bare ANSI attributes, that it should use this
app's own theme, consistent with every other piece of chrome (panels,
prompt, streaming, hardware stats). ui/theme.py's own module docstring
already draws exactly this line: raw *command output* stays
system-theme-respecting (LS_COLORS, a tool's own --color), but this
app's OWN chrome (which this palette is -- it lists oh-my-shell's own
slash commands, not any external program's output) is deliberately fixed
to this app's own Nord-mapped truecolor palette, the same as the prompt
icon, panel borders, and risk labels. So `render_toolbar_text` below now
returns a small list of `(style, text)` tuples instead of a bare `str`,
using `omsh.accent` (command name) and `omsh.muted` (description) --
`noreverse` in ui/session.py's own `Style.from_dict` override is kept
disabling the *forced reverse-video* default, which is a genuinely
separate concern (visual glitch, not this app's own accent color) from
whether this app *adds its own* color on top; the follow-up note in that
module records how the two combine.
"""

from __future__ import annotations

# (name, one-line description) -- name matches meta_commands.py's own
# `command` token (no leading "/"); description is a short paraphrase of
# HELP_TEXT's matching line, not a copy-paste, so the two can drift in
# wording without one having to mirror the other's exact formatting.
#
# Listed alphabetically by name (not HELP_TEXT's own frequency-of-use
# ordering) -- the person asked for the palette itself to be lexicographic,
# so a person scanning the list for a specific command can find it by
# letter rather than having to read the whole thing. HELP_TEXT (`/help`'s
# own output) is unaffected and keeps its separate, curated ordering --
# these are two different audiences (skimming a live list while typing vs.
# reading a reference top to bottom).
COMMANDS: list[tuple[str, str]] = sorted(
    [
        ("capabilities", "List what Oh My Shell can do"),
        ("clear", "Clear the screen"),
        ("config", "View or change settings"),
        ("exit", "Quit Oh My Shell"),
        ("explain", "Why the AI chose its last action"),
        ("help", "Show the command reference"),
        ("history", "This session's earlier requests"),
        ("log", "View the audit log"),
        ("model", "Show or switch the active model"),
        # "quit" (found missing via manual end-to-end testing): meta_commands.
        # EXIT_COMMANDS and main.py's own EXIT_COMMANDS both accept "/quit" as
        # a full alias of "/exit" -- this table just hadn't been kept in sync,
        # exactly the drift risk this module's own docstring already flags
        # about maintaining this list "by eye". Typing "/q" or "/quit"
        # previously showed no suggestion at all despite it being a real,
        # working command.
        ("quit", "Quit Oh My Shell"),
        ("stats", "Session token usage & average latency"),
        ("system", "Full hardware & shell status"),
        ("trash", "View or manage .trash/"),
        ("undo", "Revert the last destructive action"),
    ]
)

_NAME_COLUMN_WIDTH = max(len(name) for name, _ in COMMANDS) + 3  # +3: "/" + 2 gap


def visible_commands(text: str) -> list[tuple[str, str]]:
    """
    Which (name, description) pairs should be listed for the current input
    line -- empty unless `text` starts with "/" and has no space yet (still
    typing the command name itself, not its arguments; see module
    docstring). Filters by prefix once at least one character after "/" has
    been typed; a bare "/" lists everything.
    """
    if not text.startswith("/"):
        return []
    if " " in text:
        return []
    typed = text[1:].lower()
    return [(name, desc) for name, desc in COMMANDS if name.startswith(typed)]


def render_toolbar_text(text: str) -> list[tuple[str, str]]:
    """
    Styled rendering of visible_commands(text), one command per line, name
    left-padded to a fixed column so descriptions line up -- the actual
    value ui/session.py's `bottom_toolbar` callable returns.

    Returns a list of `(style, text)` tuples (prompt_toolkit's own
    `AnyFormattedText` shape), not a bare `str` -- see this module's
    "Round 4" docstring note for why color was added here: the command
    name ("/help") is styled with this app's own `omsh.accent` hex
    (ui/theme.py's ACCENT), the description with `omsh.muted` (ACCENT_DIM
    / MUTED), matching how `/help`'s own output (meta_commands.py's
    HELP_TEXT) already colors the exact same two pieces. Styles are
    passed as the SAME hex strings ui/theme.py defines (not the
    `rich`-only "omsh.accent" theme name, which prompt_toolkit's own
    `Style` object doesn't know how to resolve -- prompt_toolkit and rich
    are two separate styling systems here, each fed the same underlying
    hex values from ui/theme.py rather than one trying to reuse the
    other's theme object directly), each prefixed with the required
    leading "#" `Style.from_dict`/prompt_toolkit hex syntax expects
    (ui/theme.py's own constants already include it).
    A row break between entries is its own `("", "\\n")` tuple -- a plain
    literal newline embedded inside one of the styled strings would work
    for rendering, but keeping it as a separate, unstyled tuple keeps
    every visible run of text attributable to exactly one style, matching
    how the rest of this tuple list is built.
    """
    from ohmyshell.ui.theme import ACCENT, MUTED

    commands = visible_commands(text)
    if not commands:
        return []
    fragments: list[tuple[str, str]] = []
    for index, (name, desc) in enumerate(commands):
        if index:
            fragments.append(("", "\n"))
        fragments.append((f"fg:{ACCENT} bold", f"  /{name:<{_NAME_COLUMN_WIDTH}}"))
        fragments.append((f"fg:{MUTED}", desc))
    return fragments