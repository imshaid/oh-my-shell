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

`ReplSession` wraps a `PromptSession` behind a small interface
(`prompt(text) -> str`) so tests can substitute a fake with the same shape
without importing prompt_toolkit at all. `PromptSession` is only
constructed lazily, inside `ReplSession.__init__`, specifically so
importing this module in a test file never requires a real terminal
(prompt_toolkit's default input/output backends probe the terminal at
PromptSession-construction time, which fails under pytest's captured
stdio unless a stub input/output is supplied).

--- Command palette ---

The palette is rendered via `bottom_toolbar` -- a plain text region
prompt_toolkit redraws live under the input line as the buffer changes,
with no menu chrome (an earlier `Completer`-based approach rendered as a
floating popup menu and is not used). `style=Style.from_dict({"bottom-
toolbar": "noreverse"})` is required: prompt_toolkit's built-in default
for this class is `"reverse"` (inverted fg/bg, a filled bar); `noreverse`
explicitly clears that flag so the toolbar renders as plain inline text
in the terminal's own ambient color, with this app's own per-fragment
`fg:` colors (ui/palette.render_toolbar_text) layered on top.

`_bottom_toolbar_text()` reads the *live* buffer via
`get_app().current_buffer`, since a callable passed to `bottom_toolbar` is
re-invoked by prompt_toolkit on every redraw -- it does not receive the
buffer as an argument. ui/palette.py's `render_toolbar_text()` is the pure
function that turns "current input text" into "what to show," kept there
so it's testable without any of this prompt_toolkit plumbing.
"""

from __future__ import annotations

from typing import Protocol


class PromptReader(Protocol):
    """What ReplSession needs from a prompt engine -- satisfied by both a
    real prompt_toolkit.PromptSession.prompt and any test fake."""

    def prompt(self, text: object = "") -> str: ...


def _read_single_keypress(bindings: dict[str, str], *, enter_result: str) -> str:
    """
    Shared machinery behind every single-keypress REPL menu (the plan
    choice, the sudo/permission choice): builds a real
    `prompt_toolkit.application.Application` with `bindings` mapped to
    their result strings, `Keys.Enter` mapped to `enter_result`, and any
    other key echoed back as its own single-character result -- then runs
    it and returns whatever key was pressed, the instant it's pressed (no
    second Enter needed).

    Pulled out of `read_plan_choice_keypress` (below) so
    `read_sudo_choice_keypress` doesn't duplicate the same
    Application/KeyBindings/Layout boilerplate for a second, differently-
    keyed menu -- both bugs (plan-choice Esc, sudo-choice repeated "s")
    have the identical root cause (a full `PromptSession.prompt()` line
    read compared against typed text instead of a real keypress binding),
    so both fixes share this one implementation.
    """
    from prompt_toolkit.application import Application
    from prompt_toolkit.key_binding import KeyBindings
    from prompt_toolkit.keys import Keys
    from prompt_toolkit.layout import Layout
    from prompt_toolkit.layout.containers import Window
    from prompt_toolkit.layout.controls import FormattedTextControl

    kb = KeyBindings()

    for key, result in bindings.items():
        # Bind by closing over `result` per-iteration (default arg avoids
        # the late-binding-in-a-loop trap where every handler would
        # otherwise see the same, final `result` value).
        @kb.add(key)
        def _bound(event, _result=result):
            event.app.exit(result=_result)

    @kb.add(Keys.Enter)
    def _confirm(event):
        event.app.exit(result=enter_result)

    @kb.add(Keys.ControlC)
    @kb.add(Keys.ControlD)
    def _interrupt(event):
        event.app.exit(exception=KeyboardInterrupt)

    @kb.add(Keys.Any)
    def _other(event):
        event.app.exit(result=event.data)

    app = Application(
        layout=Layout(Window(FormattedTextControl(text=""))),
        key_bindings=kb,
        full_screen=False,
    )
    result = app.run()
    if result is None:
        raise KeyboardInterrupt
    return result


def read_plan_choice_keypress() -> str:
    """
    Reads exactly one key for the Section 8.3.3 plan-choice prompt
    ([Enter] confirm / [e] edit / [c] chat / [Esc] cancel), returning as
    soon as that single key is pressed -- no Enter needed afterward for
    e/c/Esc, and Enter alone (nothing else typed) confirms.

    Binds `Keys.Escape` directly (via `prompt_toolkit.application.
    Application` + a small `KeyBindings` set, not `PromptSession.prompt()`)
    so this prompt reacts to the real Esc key event itself rather than
    comparing a submitted line against typed text like "esc".

    This is a true single-keypress menu (matching Section 8.3.3's own
    "[Enter] Confirm   [e] Edit   [c] Chat/adjust   [Esc] Cancel" mockup
    exactly) -- Enter/e/c/Esc/q each submit the instant they're pressed, no
    second Enter needed. Any other key is echoed back as its own
    single-character result (e.g. pressing "x" returns "x") so
    run_discussion's existing "Unrecognized choice %r" message still has
    something concrete to show, matching this function's previous
    line-read behavior for a typo -- there is no legitimate multi-
    character response at this specific prompt (discussion.py's own loop
    only ever recognizes the exact strings "confirm"/"edit"/"chat"/
    "cancel"; anything else always fell into the same "unrecognized
    choice" branch even under the old line-read version), so requiring a
    second Enter for a typo case that always re-prompts anyway would only
    slow down the common case this menu is actually used for.
    """
    from prompt_toolkit.keys import Keys

    return _read_single_keypress(
        {Keys.Escape: "cancel", "q": "cancel", "e": "edit", "c": "chat"},
        enter_result="",
    )


def read_sudo_choice_keypress() -> str:
    """
    Reads exactly one key for the Section 8.3.6 sudo/permission prompt
    ([Enter] Grant (sudo)   [s] Skip this step   [Esc/q] Abort), returning
    as soon as that key is pressed -- the sudo-prompt counterpart of
    `read_plan_choice_keypress`, same direct-keypress-binding approach and
    rationale (see that function's own docstring).
    """
    from prompt_toolkit.keys import Keys

    return _read_single_keypress(
        {Keys.Escape: "abort", "q": "abort", "s": "skip"},
        enter_result="grant",
    )


def _bottom_toolbar_text() -> str:
    """
    `bottom_toolbar` callable for the real PromptSession: renders the
    inline command list (ui/palette.render_toolbar_text) for whatever the
    buffer's current text is, or "" (nothing shown) otherwise.

    Reads the live buffer via `get_app().current_buffer` rather than
    taking `text` as a parameter -- prompt_toolkit calls a `bottom_toolbar`
    callable with no arguments and re-invokes it on every redraw, so this
    is how it sees what's currently typed.
    """
    from prompt_toolkit.application import get_app

    from ohmyshell.ui.palette import render_toolbar_text

    return render_toolbar_text(get_app().current_buffer.text)


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
            from prompt_toolkit.output.color_depth import ColorDepth
            from prompt_toolkit.styles import Style

            self._reader = PromptSession(
                # `PromptSession` negotiates its own color depth for
                # rendering, auto-detected from the terminal, independently
                # of whatever color codes are already baked into an
                # `ANSI()`-wrapped string -- an auto-detected depth below
                # true-color silently flattens a 24-bit color it doesn't
                # know how to downsample. Since ui/theme.py's palette is
                # specific truecolor hex (not one of the 16 standard
                # colors), the color negotiation is pinned to TRUE_COLOR
                # explicitly rather than left to autodetection.
                color_depth=ColorDepth.TRUE_COLOR,
                bottom_toolbar=_bottom_toolbar_text,
                # Cancels prompt_toolkit's own default bottom-toolbar style
                # ("reverse", i.e. inverted fg/bg -- a filled bar), so the
                # command list renders in the terminal's own ambient text
                # color, with this app's own per-fragment `fg:` colors
                # (ui/palette.render_toolbar_text) layered on top instead
                # of appearing inverted against a filled bar.
                style=Style.from_dict({"bottom-toolbar": "noreverse"}),
            )

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
        is_ansi = isinstance(formatted_text, str) and "\x1b[" in formatted_text
        if is_ansi:
            from prompt_toolkit.formatted_text import ANSI

            formatted_text = ANSI(formatted_text)

        # `color_depth` passed to the `PromptSession` constructor only sets
        # a default for calls that don't specify their own -- the actual
        # per-call color negotiation happens inside `.prompt()`/`.app.run()`
        # itself, which can re-derive a depth from the live output backend
        # rather than reliably falling back to the constructor's default.
        # Passing `color_depth=` explicitly on every `.prompt()` call too
        # removes that ambiguity.
        if is_ansi:
            from prompt_toolkit.output.color_depth import ColorDepth

            return self._reader.prompt(formatted_text, color_depth=ColorDepth.TRUE_COLOR)
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