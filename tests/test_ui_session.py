"""
Tests for ui/session.py (Build Order Step 11's prompt_toolkit REPL engine).

A fake `PromptReader` stands in for prompt_toolkit.PromptSession in every
test here, so importing/instantiating prompt_toolkit's own terminal-probing
machinery never happens under pytest -- see ReplSession's own docstring for
why that matters (its lazy-construction design is exactly for this).
"""

from __future__ import annotations

import pytest

from ohmyshell.ui.session import ReplSession


class FakeReader:
    """Scripted responses, one per .prompt() call, plus a record of what
    each call was asked to show -- lets tests assert both the return value
    and that ReplSession passed the right prompt text through unmodified."""

    def __init__(self, responses: list[str]):
        self._responses = list(responses)
        self.calls: list[object] = []

    def prompt(self, text: object = "") -> str:
        self.calls.append(text)
        return self._responses.pop(0)


class TestReplSessionWithFakeReader:
    def test_prompt_returns_readers_response(self):
        session = ReplSession(reader=FakeReader(["hello"]))
        assert session.prompt("> ") == "hello"

    def test_prompt_passes_formatted_text_through_unchanged(self):
        reader = FakeReader(["ok"])
        session = ReplSession(reader=reader)
        sentinel = object()  # stands in for an ANSI(...)-wrapped prompt
        session.prompt(sentinel)
        assert reader.calls == [sentinel]

    def test_call_is_equivalent_to_prompt(self):
        """session(...) must work everywhere main.py passes `read` as a
        plain callable (discussion.py/sudo_layer.py's existing contracts)."""
        reader = FakeReader(["typed value"])
        session = ReplSession(reader=reader)
        assert session("Edit which param? ") == "typed value"

    def test_multiple_calls_consume_responses_in_order(self):
        reader = FakeReader(["first", "second", "third"])
        session = ReplSession(reader=reader)
        assert [session(), session(), session()] == ["first", "second", "third"]

    def test_eof_error_propagates(self):
        class _RaisingReader:
            def prompt(self, text=""):
                raise EOFError

        session = ReplSession(reader=_RaisingReader())
        with pytest.raises(EOFError):
            session.prompt("> ")

    def test_keyboard_interrupt_propagates(self):
        class _RaisingReader:
            def prompt(self, text=""):
                raise KeyboardInterrupt

        session = ReplSession(reader=_RaisingReader())
        with pytest.raises(KeyboardInterrupt):
            session.prompt("> ")


class TestReplSessionAnsiWrapping:
    """
    A raw-ANSI-escaped prompt string (ui/prompt.py's render_prompt_ansi
    output) must be wrapped in prompt_toolkit's ANSI() before reaching the
    underlying reader, or prompt_toolkit renders the literal escape bytes
    instead of styled text -- see ReplSession.prompt's own docstring.
    """

    def test_ansi_string_is_wrapped_before_reaching_reader(self):
        from prompt_toolkit.formatted_text import ANSI

        reader = FakeReader(["typed"])
        session = ReplSession(reader=reader)
        session.prompt("\x1b[36mfolder\x1b[0m ❯ ")
        assert len(reader.calls) == 1
        assert isinstance(reader.calls[0], ANSI)

    def test_plain_string_without_ansi_passes_through_unwrapped(self):
        reader = FakeReader(["typed"])
        session = ReplSession(reader=reader)
        session.prompt("Edit which param? ")
        assert reader.calls == ["Edit which param? "]

    def test_default_empty_prompt_passes_through_unwrapped(self):
        reader = FakeReader(["typed"])
        session = ReplSession(reader=reader)
        session.prompt()
        assert reader.calls == [""]


class TestReplSessionConstructionIsLazy:
    def test_default_construction_does_not_require_reader_kwarg_to_import(self):
        """Just importing ohmyshell.ui.session (done at module load, above)
        must never touch prompt_toolkit's terminal-probing machinery --
        this test passing at all (no error during collection) is the
        actual assertion; this body only re-confirms the class is usable
        with an explicit reader, which is the supported test-time path."""
        session = ReplSession(reader=FakeReader(["x"]))
        assert session.prompt() == "x"

class TestReplSessionRealConstructionWiresInlineToolbar:
    """
    Command-palette, round 3: the real (non-fake) construction path must
    wire prompt_toolkit's `bottom_toolbar` (an inline text region, not a
    popup menu) to ui/session._bottom_toolbar_text, and blank out
    prompt_toolkit's own default reverse-video toolbar styling so the list
    renders in the terminal's own ambient colors -- see ui/session.py's own
    docstring, "Round 3," for the full history of why rounds 1 and 2 (a
    Completer-based popup, then a recolored popup) were replaced. This
    constructs a real PromptSession (no `reader=` override) -- safe under
    pytest because PromptSession's own terminal-probing is about picking an
    input/output backend, not about requiring an interactive TTY to merely
    construct one.
    """

    def test_default_construction_sets_bottom_toolbar(self):
        from ohmyshell.ui.session import _bottom_toolbar_text

        session = ReplSession()
        assert session._reader.bottom_toolbar is _bottom_toolbar_text

    def test_default_construction_blanks_the_toolbars_default_style(self):
        """
        prompt_toolkit's own built-in default for the "bottom-toolbar"
        style class is "reverse" (confirmed by reading
        prompt_toolkit/styles/defaults.py directly) -- an inverted-color
        bar, which is exactly the kind of forced styling that clashes with
        an arbitrary terminal theme. The style passed here must override it
        to an empty rule (no color forced either way), not to some other
        specific color -- picking a *different* hardcoded color was round
        2's mistake.
        """
        session = ReplSession()
        rules = dict(session._reader.style.style_rules)
        assert rules.get("bottom-toolbar") == ""

    def test_default_construction_does_not_set_a_completer(self):
        """Round 1/2's popup-menu approach (a prompt_toolkit Completer) is
        gone entirely in round 3 -- nothing here should still be wiring one
        up, since a completer is what produced the floating popup box the
        person explicitly said they didn't want."""
        session = ReplSession()
        assert session._reader.completer is None


class TestBottomToolbarText:
    """
    Unit tests for the bottom_toolbar callable itself, via a fake
    prompt_toolkit "current app" so the current buffer's text can be
    controlled directly without a real PromptSession event loop.
    """

    class _FakeBuffer:
        def __init__(self, text: str):
            self.text = text

    class _FakeApp:
        def __init__(self, text: str):
            self.current_buffer = TestBottomToolbarText._FakeBuffer(text)

    def _call_with_text(self, text: str, monkeypatch) -> str:
        from ohmyshell.ui import session as session_module

        monkeypatch.setattr(
            session_module, "_bottom_toolbar_text", session_module._bottom_toolbar_text
        )
        import prompt_toolkit.application as application_module

        monkeypatch.setattr(application_module, "get_app", lambda: self._FakeApp(text))
        return session_module._bottom_toolbar_text()

    def test_renders_matching_commands_for_current_buffer_text(self, monkeypatch):
        result = self._call_with_text("/mo", monkeypatch)
        assert "/model" in result

    def test_renders_empty_string_when_buffer_has_no_slash(self, monkeypatch):
        result = self._call_with_text("clean up temp files", monkeypatch)
        assert result == ""