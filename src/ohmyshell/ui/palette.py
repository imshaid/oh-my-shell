"""
Command palette (Step 11 visual-polish follow-up).

meta_commands.py already is the full command reference (Section 8.4's
table, reproduced in HELP_TEXT); this module doesn't invent new commands,
it makes the existing slash-command set discoverable as you type "/"
instead of only via `/help`.

Design notes:

- The command list below is a small, explicit table, not a parser over
  meta_commands.HELP_TEXT's formatted text. HELP_TEXT is prose meant for a
  human to read top-to-bottom; meta_commands.py's `dispatch()` remains the
  single source of truth for what a command actually does. This table only
  carries what the palette needs (name, one-line description), kept next
  to each other so it's easy to keep in sync by eye.
- Only top-level command names are matched (e.g. "/model"), not their
  subcommands ("/model switch gemini-3.1-flash-lite"), since
  meta_commands.py's subcommand shapes vary too much (a fixed enum like
  /trash's status|keep|clear, vs. a free-form model name or config key) to
  offer generically useful completions for.
- The list only appears when the line starts with "/" (checked by
  visible_commands itself) -- typing "/" mid-sentence in ordinary natural
  language must not show a command list, mirroring router.py's own
  "starts with /" slash-command classification.

The palette is rendered via `prompt_toolkit.PromptSession`'s
`bottom_toolbar` -- a plain text region that redraws live under the input
line as you type, with no border or popup menu chrome.
`visible_commands(text)` is the pure function that decides what to show
(empty list unless `text` starts with "/"); `render_toolbar_text(text)`
turns that into the styled fragments ui/session.py's `bottom_toolbar`
callable returns -- separated so each is independently testable without
constructing a real PromptSession.

Colors use this app's own fixed Nord accent palette (ui/theme.py),
consistent with every other piece of chrome (panels, prompt, streaming,
hardware stats): raw command output stays system-theme-respecting
(LS_COLORS, a tool's own --color), but this app's own chrome is
deliberately fixed to its own truecolor palette. `render_toolbar_text`
returns a list of `(style, text)` tuples using `omsh.accent` (command
name) and `omsh.muted` (description).
"""

from __future__ import annotations

# (name, one-line description) -- name matches meta_commands.py's own
# `command` token (no leading "/"); description is a short paraphrase of
# HELP_TEXT's matching line, not a copy-paste, so the two can drift in
# wording without one having to mirror the other's exact formatting.
#
# Listed alphabetically by name (not HELP_TEXT's own frequency-of-use
# ordering), so a person scanning the list for a specific command can find
# it by letter. HELP_TEXT (`/help`'s own output) keeps its separate,
# curated ordering -- these serve two different audiences (skimming a live
# list while typing vs. reading a reference top to bottom).
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
        # "quit" is a full alias of "/exit" in meta_commands.EXIT_COMMANDS
        # and main.py's own EXIT_COMMANDS -- listed here too so typing
        # "/q" or "/quit" shows a suggestion.
        ("quit", "Quit Oh My Shell"),
        ("stats", "Session token usage & average latency"),
        ("system", "Full hardware & shell status"),
        ("trash", "View or manage .trash/"),
        ("undo", "Revert the last destructive action"),
        ("update", "Check for and install the latest version"),
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
    `AnyFormattedText` shape), not a bare `str`: the command name
    ("/help") is styled with this app's own `omsh.accent` hex
    (ui/theme.py's ACCENT), the description with `omsh.muted`, matching
    how `/help`'s own output (meta_commands.py's HELP_TEXT) colors the
    same two pieces. Styles are passed as the same hex strings ui/theme.py
    defines, not the `rich`-only "omsh.accent" theme name -- prompt_toolkit
    and rich are separate styling systems here, each fed the same
    underlying hex values, each already prefixed with the leading "#"
    `Style.from_dict`/prompt_toolkit hex syntax expects.
    A row break between entries is its own `("", "\\n")` tuple, kept
    separate and unstyled so every visible run of text is attributable to
    exactly one style.
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