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
  the two things a completion popup needs (name, one-line description),
  kept next to each other here so they're easy to keep in sync with
  meta_commands.py's command set by eye.
- Only top-level command names are completed (e.g. "/model"), not their
  subcommands ("/model switch qwen3:8b") -- matching how Claude Code's own
  "/" palette works (it completes the command, not every argument), and
  because meta_commands.py's subcommand shapes vary too much (some take a
  fixed enum like /trash's status|keep|clear, others take a free-form
  model name or config key) to offer generically useful completions for.
- Completion only triggers when the line starts with "/" (checked by
  OhMyShellCompleter.get_completions itself) -- typing "/" in the middle of
  an ordinary natural-language sentence (e.g. "what does a/b mean") must
  not pop up a command list. Router.py's own INPUT classification already
  treats "starts with /" as the slash-command signal (see router.py); this
  completer mirrors that exact rule rather than inventing a second one.

--- Round 2 fixes (post-Build-Order, after the first delivered version was
tested in a real terminal) ---

Two real problems came back from that test, both fixed here and in
ui/session.py rather than in this module's own completion logic (which was
already correct -- see test_ui_palette.py, unchanged):

1. "the ui too much ugly and not properly fit with the main shell ui... not
   additional popup". prompt_toolkit's own default completion-menu style is
   a flat grey box (`bg:#bbbbbb #000000` for the menu, `#999999` for the
   meta column -- prompt_toolkit/styles/defaults.py's own hardcoded
   defaults), which has nothing to do with this app's rich-driven cyan/dim
   palette (ui/prompt.py). PALETTE_STYLE below is a prompt_toolkit `Style`
   override for exactly the "completion-menu*" style classes, matched to
   this app's own colors (dark background, cyan for the command name,
   grey/dim for its description, reverse-cyan for the highlighted row) --
   passed into PromptSession as `style=` in ui/session.py. This does not
   change the menu's *position* (still an inline dropdown directly under
   the cursor, prompt_toolkit's COLUMN style, which is already the
   "attached, not floating" layout the person compared to Claude Code/
   Hermes Agent) -- only its colors, which is what actually read as "ugly."

2. "when I remove o from /mo then terminal not show any commands". This one
   is a genuine prompt_toolkit behavior gap, not a bug in this module:
   `Buffer.insert_text` is the ONLY place that fires `complete_while_typing`
   autocompletion (see its own source -- the async completer task is
   scheduled from inside `insert_text`, right after `on_text_insert.fire()`).
   Backspace goes through `delete_before_cursor`, which never calls
   `insert_text` and never schedules that completer task -- so
   `complete_while_typing=True` alone silently does nothing on backspace,
   independent of anything OhMyShellCompleter itself does (this was
   confirmed by reading prompt_toolkit's own buffer.py, not guessed at).
   The fix is in ui/session.py: an `on_text_changed` handler (which DOES
   fire on every edit, insert or delete alike) that explicitly calls
   `buffer.start_completion()` again whenever the line still starts with
   "/", so deleting back into a shorter command prefix re-opens the list
   instead of leaving it stuck closed.
"""

from __future__ import annotations

from prompt_toolkit.completion import Completer, Completion
from prompt_toolkit.styles import Style

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


# Matches ui/prompt.py's own palette (folder=bold blue, icon=cyan) instead
# of prompt_toolkit's stock grey completion-menu colors -- see this
# module's docstring, fix #1. Only the "completion-menu*" style classes are
# set here; everything else (the input line itself, etc.) is left alone so
# this can't accidentally override unrelated prompt styling.
PALETTE_STYLE = Style.from_dict(
    {
        "completion-menu": "bg:#1c1c1c #d0d0d0",
        "completion-menu.completion": "bg:#1c1c1c #00d7ff",
        "completion-menu.completion.current": "bg:#00d7ff #1c1c1c bold",
        "completion-menu.meta.completion": "bg:#1c1c1c #808080",
        "completion-menu.meta.completion.current": "bg:#00d7ff #1c1c1c",
    }
)


class OhMyShellCompleter(Completer):
    """
    Offers slash-command completions while the current line starts with
    "/" and has no space yet (i.e. the user is still typing the command
    name itself, not its arguments -- see module docstring for why
    subcommand args aren't completed here).
    """

    def get_completions(self, document, complete_event):
        text = document.text_before_cursor
        if not text.startswith("/"):
            return
        if " " in text:
            # Already past the command name (typing args) -- nothing to
            # complete; see module docstring.
            return

        typed = text[1:].lower()
        for name, description in COMMANDS:
            if name.startswith(typed):
                yield Completion(
                    name,
                    start_position=-len(typed),
                    display=f"/{name}",
                    display_meta=description,
                )