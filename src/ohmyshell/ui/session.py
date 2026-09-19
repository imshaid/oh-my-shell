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

--- Command palette: three rounds (post-Build-Order) ---

Round 1 wired a prompt_toolkit `Completer` (ui/palette.OhMyShellCompleter)
via `completer=`/`complete_while_typing=True`. Real-terminal testing found
two problems: prompt_toolkit's own default completion-menu colors (flat
grey) didn't match this app's look, and backspacing inside a "/..." line
made the menu vanish instead of updating (a genuine prompt_toolkit gap:
`complete_while_typing` only fires from `Buffer.insert_text`, never from
delete -- confirmed by reading prompt_toolkit's own buffer.py).

Round 2 fixed the backspace gap (an `on_text_changed` handler that
re-triggered completion) and reskinned the menu with a custom `Style` to
match this app's colors. Testing found this still wasn't right, for a
reason round 2 missed: a floating completion-menu widget is a popup
regardless of what colors it uses, and the person had explicitly asked for
an *inline* list ("not any additional popup or else"), like Claude Code's
own "/" list. Round 2's custom colors were also wrong on their own terms --
hardcoding specific hex colors bakes in an assumption about the person's
terminal theme that doesn't hold for every user.

Round 3 (current): the `Completer`/`complete_while_typing`/`on_text_changed`
machinery from rounds 1/2 is gone entirely. `bottom_toolbar` is used
instead -- a plain text region prompt_toolkit redraws live under the input
line as the buffer changes, with no menu chrome. Getting this to actually
render as *plain inline text*, with no forced color and no filled bar, took
two attempts within this round (both worth recording, since the first
attempt looked correct in the source but was still wrong on screen):

  - First attempt: `style=Style.from_dict({"bottom-toolbar": ""})`. This
    still rendered as a solid filled bar in a real terminal. Root cause,
    confirmed by directly inspecting prompt_toolkit's own resolved style
    attributes (`merge_styles([default_ui_style(), Style.from_dict({...})])
    .get_attrs_for_style_str("class:bottom-toolbar")`): prompt_toolkit's
    built-in default for this class is `"reverse"` (inverted fg/bg), and an
    *empty* string in `Style.from_dict` does not reset an already-set
    boolean flag like `reverse` -- there's nothing in `""` to override it
    with, so the inherited `reverse=True` survived the merge untouched.
    `Window`'s own `style="class:bottom-toolbar"` (prompt_toolkit's
    shortcuts/prompt.py, not something this app controls) then painted
    that reversed style across the toolbar's full width, which is what
    actually produced the "filled purple bar" look, regardless of what
    hex value (or lack of one) this app supplied.
  - Fix: `style=Style.from_dict({"bottom-toolbar": "noreverse"})`.
    `noreverse` is an explicit instruction to clear the reverse flag
    (confirmed the same way -- the merged attrs now show `reverse=False`),
    not "no color specified." With reverse actually off, the toolbar
    region no longer forces any fg/bg of its own, so the command list
    renders in the terminal's own ambient text color, matching Claude
    Code's own "/" list.

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


def read_plan_choice_keypress() -> str:
    """
    Reads exactly one key for the Section 8.3.3 plan-choice prompt
    ([Enter] confirm / [e] edit / [c] chat / [Esc] cancel), returning as
    soon as that single key is pressed -- no Enter needed afterward for
    e/c/Esc, and Enter alone (nothing else typed) confirms.

    --- Bug this fixes ---
    main.py's `_repl_get_user_choice` previously read this prompt with
    `ReplSession.prompt()` (a full `PromptSession.prompt()` line-edit
    call), then compared the *typed line* against `("esc", "q", "cancel")`.
    That only ever matched if someone typed the literal four letters "esc"
    and pressed Enter -- a raw Esc keypress produces no printable
    characters at all in `PromptSession.prompt()`'s default key bindings
    (it isn't bound to submit the buffer, so hitting it did nothing
    visible), which is exactly why only "q" + Enter appeared to work in
    real-terminal testing. Directly binding the `Keys.Escape` key here
    (via `prompt_toolkit.application.Application` + a small `KeyBindings`
    set, not `PromptSession.prompt()`) is the actual fix -- it lets this
    one prompt react to the real Esc key event itself, not to typed text
    that happens to spell out its name.

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
    from prompt_toolkit.application import Application
    from prompt_toolkit.key_binding import KeyBindings
    from prompt_toolkit.keys import Keys
    from prompt_toolkit.layout import Layout
    from prompt_toolkit.layout.containers import Window
    from prompt_toolkit.layout.controls import FormattedTextControl

    kb = KeyBindings()

    @kb.add(Keys.Escape)
    @kb.add("q")
    def _cancel(event):
        event.app.exit(result="cancel")

    @kb.add("e")
    def _edit(event):
        event.app.exit(result="edit")

    @kb.add("c")
    def _chat(event):
        event.app.exit(result="chat")

    @kb.add(Keys.Enter)
    def _confirm(event):
        event.app.exit(result="")

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
            from prompt_toolkit.styles import Style

            self._reader = PromptSession(
                bottom_toolbar=_bottom_toolbar_text,
                # Cancels prompt_toolkit's own default bottom-toolbar style
                # ("reverse", i.e. inverted fg/bg -- a filled bar), so the
                # command list renders in the terminal's own ambient text
                # color instead. See this module's docstring, "Round 3" --
                # and its own follow-up fix, below.
                #
                # Round 3 follow-up (still under this same round -- the
                # first attempt at this line used `"bottom-toolbar": ""`
                # instead of `"...": "noreverse"`, which real-terminal
                # testing showed still rendered as a solid filled bar, not
                # plain inline text). The reason: an empty style string in
                # `Style.from_dict` does not reset an already-set boolean
                # flag like `reverse` -- it has nothing to override with,
                # so prompt_toolkit's own built-in default
                # (`("bottom-toolbar", "reverse")`, confirmed by reading
                # prompt_toolkit/styles/defaults.py) stayed in effect after
                # merging with this style (confirmed directly: computing
                # `merge_styles([default_ui_style(), Style.from_dict({...})]
                # ).get_attrs_for_style_str("class:bottom-toolbar")` with
                # `""` still showed `reverse=True`; only the explicit
                # `"noreverse"` token actually clears it, verified the same
                # way). `noreverse` is the fix -- an explicit instruction to
                # turn reverse-video off, not merely "no color specified."
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