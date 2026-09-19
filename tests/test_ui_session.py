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