"""
Full rich prompt engine (Build Order Step 11).

Replaces main.py's plain `input(_render_prompt(cfg))` call with a
`prompt_toolkit.PromptSession`-driven read, so the REPL prompt line itself
is colored (via ui/prompt.py's render_prompt_ansi) rather than the plain
"<folder> (<model>) <icon> " string Step 6 originally produced.

Why prompt_toolkit and not a plain `input()` + ANSI-string prompt (the
simpler alternative): `input()` accepts only a `str` prompt argument, and a
raw ANSI-escaped string passed there types-and-displays correctly on most
terminals but breaks line-editing (backspace/arrow-key redraw) on some,
because the terminal's own readline layer doesn't know the escape
sequences aren't part of the editable text. `prompt_toolkit.PromptSession`
is explicitly designed to accept a styled prompt (as its own `ANSI`/
`HTML`/`FormattedText` types) and keep line-editing correct regardless --
this is the actual reason Step 11 reaches for a real prompt library instead
of hand-rolling ANSI codes into `input()`.

Testability (Section 16 Rule 5 -- this module's own design, since
prompt_toolkit has no drop-in equivalent to a fake `input()` callable):
`ReplSession` wraps a `PromptSession` behind a small interface
(`prompt(text) -> str`) so tests can substitute a fake with the same shape
without importing prompt_toolkit at all. `PromptSession` is only
constructed lazily, inside `ReplSession.__init__`, specifically so
importing this module in a test file never requires a real terminal
(prompt_toolkit's default input/output backends probe the terminal at
PromptSession-construction time, which fails under pytest's captured
stdio unless a stub input/output is supplied).

Command palette (Step 11 follow-up, post-Build-Order): a real
`PromptSession` is now constructed with `completer=OhMyShellCompleter()`
and `complete_while_typing=True`, so typing "/" shows the slash-command
list (ui/palette.py) live, matching the person's explicit ask ("when press
/ then automatically show all the commands ... like claude code or hermes
agent"). This only affects the real, lazily-constructed `PromptSession` --
a test-injected `reader` never sees a completer at all, since fakes used
in tests don't implement prompt_toolkit's completion protocol and have no
reason to.

Command palette, round 2 (post-Build-Order, after real-terminal testing of
round 1 found two problems -- see ui/palette.py's own docstring for the
full root-cause writeup of both):

  - `style=ui.palette.PALETTE_STYLE` is now also passed to PromptSession,
    overriding prompt_toolkit's stock grey completion-menu colors with
    this app's own cyan/dim palette.
  - `_retrigger_completion_on_edit`, attached below as an
    `on_text_changed` handler on the real PromptSession's buffer, re-opens
    the completion menu on backspace/delete too. `complete_while_typing`
    by itself only fires from `Buffer.insert_text` (confirmed by reading
    prompt_toolkit's own buffer.py) -- deleting a character never re-runs
    the completer through that mechanism, which is why backspacing "/mo"
    down to "/m" made the list vanish instead of updating. `on_text_changed`
    fires on every edit either direction, so this handler is what actually
    keeps the menu in sync while backspacing.
"""

from __future__ import annotations

from typing import Protocol


class PromptReader(Protocol):
    """What ReplSession needs from a prompt engine -- satisfied by both a
    real prompt_toolkit.PromptSession.prompt and any test fake."""

    def prompt(self, text: object = "") -> str: ...


def _retrigger_completion_on_edit(buffer) -> None:
    """
    `on_text_changed` handler: keeps the command-palette menu in sync on
    backspace/delete, which `complete_while_typing` alone does not cover
    (see this module's docstring, "Command palette, round 2," for why).

    Re-runs completion whenever the line still starts with "/" (so typing
    forward keeps working exactly as before -- this handler doesn't change
    that path, it just ALSO fires on deletes), and explicitly cancels any
    open menu once the line no longer starts with "/" (e.g. the user
    backspaced all the way past the leading "/", or pasted over the whole
    line) -- otherwise a stale menu from a moment ago could linger onscreen
    for text it no longer applies to.
    """
    if buffer.text.startswith("/"):
        buffer.start_completion(select_first=False)
    else:
        buffer.cancel_completion()


class ReplSession:
    """
    Thin wrapper around prompt_toolkit.PromptSession, used as the REPL's
    `read` callable everywhere main.py previously passed the `input`
    builtin directly.

    Args:
        reader: override for testing -- any object with a `.prompt(text)`
            method. Defaults to a real `prompt_toolkit.PromptSession`
            (constructed here, not at import time, so importing this
            module never touches the terminal).
    """

    def __init__(self, *, reader: PromptReader | None = None) -> None:
        if reader is not None:
            self._reader = reader
        else:
            from prompt_toolkit import PromptSession

            from ohmyshell.ui.palette import PALETTE_STYLE, OhMyShellCompleter

            self._reader = PromptSession(
                completer=OhMyShellCompleter(),
                complete_while_typing=True,
                style=PALETTE_STYLE,
            )
            # See _retrigger_completion_on_edit's own docstring -- closes
            # the "backspace makes the menu vanish" gap that
            # complete_while_typing alone leaves open.
            self._reader.default_buffer.on_text_changed += _retrigger_completion_on_edit

    def prompt(self, formatted_text: object = "") -> str:
        """
        Read one line.

        `formatted_text` is either a plain `str` (an ordinary sub-prompt
        like "Edit which param?") or a `str` already containing raw ANSI
        escape codes (ui/prompt.py's `render_prompt_ansi` output, used for
        the REPL's own colored main prompt). A raw-ANSI string is wrapped
        in prompt_toolkit's own `ANSI(...)` here before being handed to the
        underlying reader -- prompt_toolkit does not parse escape codes out
        of a bare `str` (it would render the literal escape bytes), and
        `ANSI(...)` is what actually turns them into styled terminal
        output while keeping prompt_toolkit's line-editing correct. Callers
        (main.py) don't need to know this distinction -- passing either
        kind of string here just works.

        Raises EOFError/KeyboardInterrupt exactly as `input()` would
        (prompt_toolkit's own contract), so main.py's existing
        `except (EOFError, KeyboardInterrupt)` handling is unchanged.
        """
        if isinstance(formatted_text, str) and "\x1b[" in formatted_text:
            from prompt_toolkit.formatted_text import ANSI

            formatted_text = ANSI(formatted_text)
        return self._reader.prompt(formatted_text)

    def __call__(self, formatted_text: object = "") -> str:
        """
        Makes a ReplSession instance itself usable anywhere a plain
        `read: callable` was expected (main.py's existing REPL-callback
        signatures throughout discussion.py/sudo_layer.py wiring) --
        `read("some prompt: ")` and `session("some prompt: ")` behave
        identically, so passing `session` in place of `input` requires no
        signature changes downstream.
        """
        return self.prompt(formatted_text)