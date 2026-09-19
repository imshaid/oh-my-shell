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
that redraws live under the input line as you type, with no border, no
menu chrome, and (per the explicit `Style.from_dict({"bottom-toolbar": ""})`
override in ui/session.py) no forced background/foreground color at all,
so it renders in the terminal's own default text color exactly like any
other line this app prints via `rich`. `visible_commands(text)` below is
the pure function that decides what to show (empty list unless `text`
starts with "/"); `render_toolbar_text(text)` turns that into the actual
lines ui/session.py's `bottom_toolbar` callable returns -- separated so
each is independently testable without constructing a real PromptSession.
"""

from __future__ import annotations

# (name, one-line description) -- name matches meta_commands.py's own
# `command` token (no leading "/"); description is a short paraphrase of
# HELP_TEXT's matching line, not a copy-paste, so the two can drift in
# wording without one having to mirror the other's exact formatting.
COMMANDS: list[tuple[str, str]] = [
    ("help", "Show the command reference"),
    ("model", "Show or switch the active model"),
    ("history", "This session's earlier requests"),
    ("undo", "Revert the last destructive action"),
    ("trash", "View or manage .trash/"),
    ("log", "View the audit log"),
    ("capabilities", "List what Oh My Shell can do"),
    ("explain", "Why the AI chose its last action"),
    ("stats", "Session token usage & average latency"),
    ("system", "Full hardware & shell status"),
    ("config", "View or change settings"),
    ("clear", "Clear the screen"),
    ("exit", "Quit Oh My Shell"),
]

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


def render_toolbar_text(text: str) -> str:
    """
    Plain-text rendering of visible_commands(text), one command per line,
    name left-padded to a fixed column so descriptions line up -- the
    actual string ui/session.py's `bottom_toolbar` callable returns.
    Deliberately returns a bare `str`, not any rich/prompt_toolkit markup
    object: no color or style is attached here at all, so whatever the
    terminal's own default foreground/background is is what's shown (see
    ui/session.py's matching `Style.from_dict({"bottom-toolbar": ""})`
    override, which is what stops prompt_toolkit's own default reverse-
    video toolbar styling from applying here).
    """
    commands = visible_commands(text)
    if not commands:
        return ""
    lines = [f"  /{name:<{_NAME_COLUMN_WIDTH}}{desc}" for name, desc in commands]
    return "\n".join(lines)