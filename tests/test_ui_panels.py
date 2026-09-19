"""Tests for ui/panels.py (Build Order Step 11)."""

from __future__ import annotations

import io

import pytest
from rich.console import Console
from rich.panel import Panel

from ohmyshell.danger_classifier import ClassificationResult, Destructive, Safe
from ohmyshell.plan_generator import Plan
from ohmyshell.sudo_layer import ElevatedStep, SudoDecision
from ohmyshell.ui.panels import (
    RichSudoPrompt,
    print_panel,
    render_destructive_command_panel,
    render_plan_panel,
    render_sudo_panel,
    render_undo_confirm_panel,
)


def _render_to_text(panel) -> str:
    buffer = io.StringIO()
    console = Console(file=buffer, width=100, force_terminal=False)
    console.print(panel)
    return buffer.getvalue()


def _plan(**overrides) -> Plan:
    defaults = dict(
        action="clean_temp_files",
        params={"days": 7, "paths": ["/tmp"]},
        risk="medium",
        steps=["Scan /tmp for files older than 7 days", "Move matched files to .trash/"],
    )
    defaults.update(overrides)
    return Plan(**defaults)


class TestRenderPlanPanel:
    def test_returns_a_panel(self):
        assert isinstance(render_plan_panel(_plan()), Panel)

    def test_includes_every_step(self):
        text = _render_to_text(render_plan_panel(_plan()))
        assert "Scan /tmp for files older than 7 days" in text
        assert "Move matched files to .trash/" in text

    def test_includes_risk_level(self):
        text = _render_to_text(render_plan_panel(_plan(risk="high")))
        assert "High" in text

    def test_includes_action_in_title(self):
        panel = render_plan_panel(_plan(action="organize_files"))
        assert "organize_files" in str(panel.title)

    def test_includes_confirm_edit_chat_cancel_options(self):
        text = _render_to_text(render_plan_panel(_plan()))
        assert "Confirm" in text
        assert "Edit" in text
        assert "Chat/adjust" in text
        assert "Cancel" in text


class TestRenderDestructiveCommandPanel:
    def _result(self, *, trash_alternative_possible: bool) -> ClassificationResult:
        return ClassificationResult(
            verdict=Destructive(
                explanation="this deletes everything in /tmp",
                trash_alternative_possible=trash_alternative_possible,
            ),
            source="regex",
        )

    def test_raises_for_safe_verdict(self):
        safe_result = ClassificationResult(verdict=Safe(), source="regex")
        with pytest.raises(TypeError):
            render_destructive_command_panel(safe_result)

    def test_includes_explanation(self):
        text = _render_to_text(render_destructive_command_panel(self._result(trash_alternative_possible=True)))
        assert "this deletes everything in /tmp" in text

    def test_includes_trash_option_when_possible(self):
        text = _render_to_text(render_destructive_command_panel(self._result(trash_alternative_possible=True)))
        assert "[t] Move to trash instead" in text

    def test_omits_trash_option_when_not_possible(self):
        text = _render_to_text(render_destructive_command_panel(self._result(trash_alternative_possible=False)))
        assert "[t]" not in text

    def test_includes_run_anyway_and_cancel_always(self):
        text = _render_to_text(render_destructive_command_panel(self._result(trash_alternative_possible=False)))
        assert "[y] Run anyway" in text
        assert "[n] Cancel" in text


class TestRenderSudoPanel:
    def _step(self) -> ElevatedStep:
        return ElevatedStep(
            step_number=3,
            total_steps=5,
            description="Clear system-level cache in /var/cache",
            reason="this directory is owned by root",
        )

    def test_includes_step_number_and_total(self):
        text = _render_to_text(render_sudo_panel(self._step()))
        assert "Step 3/5" in text

    def test_includes_description_and_reason(self):
        text = _render_to_text(render_sudo_panel(self._step()))
        assert "Clear system-level cache in /var/cache" in text
        assert "this directory is owned by root" in text

    def test_includes_grant_skip_abort_options(self):
        text = _render_to_text(render_sudo_panel(self._step()))
        assert "Grant" in text
        assert "Skip" in text
        assert "Abort" in text


class TestRenderUndoConfirmPanel:
    def test_returns_a_panel(self):
        assert isinstance(render_undo_confirm_panel(count=340), Panel)

    def test_includes_count_and_files_plural(self):
        text = _render_to_text(render_undo_confirm_panel(count=340))
        assert "340" in text
        assert "files" in text

    def test_singular_file_for_count_one(self):
        text = _render_to_text(render_undo_confirm_panel(count=1))
        assert "1 file" in text
        assert "1 files" not in text

    def test_includes_confirm_and_cancel_options(self):
        text = _render_to_text(render_undo_confirm_panel(count=5))
        assert "Confirm undo" in text
        assert "Cancel" in text


class TestPrintPanel:
    def test_writes_to_injected_console(self):
        buffer = io.StringIO()
        console = Console(file=buffer, width=100, force_terminal=False)
        print_panel(render_undo_confirm_panel(count=1), console=console)
        assert "Undo" in buffer.getvalue()

    def test_works_without_injected_console(self, capsys):
        # Should not raise even with rich's default Console (stdout).
        print_panel(render_undo_confirm_panel(count=1))


class TestRichSudoPrompt:
    """
    RichSudoPrompt is the Step 11 SudoPrompt implementation -- same
    Grant/Skip/Abort decision contract as sudo_layer.InputPrompt, just
    rendered via render_sudo_panel() instead of plain text. These tests
    mirror sudo_layer.py's own InputPrompt tests but assert against a
    rich-rendered buffer instead of print() calls.
    """

    @staticmethod
    def _step() -> ElevatedStep:
        return ElevatedStep(
            step_number=3,
            total_steps=5,
            description="Clear system-level cache in /var/cache",
            reason="this directory is owned by root",
        )

    def test_bare_enter_grants(self):
        buffer = io.StringIO()
        console = Console(file=buffer, width=100, force_terminal=False)
        prompt = RichSudoPrompt(input_fn=lambda _: "", console=console)
        assert prompt.ask(self._step()) is SudoDecision.GRANT

    def test_s_skips(self):
        buffer = io.StringIO()
        console = Console(file=buffer, width=100, force_terminal=False)
        prompt = RichSudoPrompt(input_fn=lambda _: "s", console=console)
        assert prompt.ask(self._step()) is SudoDecision.SKIP

    def test_esc_aborts(self):
        buffer = io.StringIO()
        console = Console(file=buffer, width=100, force_terminal=False)
        prompt = RichSudoPrompt(input_fn=lambda _: "esc", console=console)
        assert prompt.ask(self._step()) is SudoDecision.ABORT

    def test_q_aborts(self):
        buffer = io.StringIO()
        console = Console(file=buffer, width=100, force_terminal=False)
        prompt = RichSudoPrompt(input_fn=lambda _: "q", console=console)
        assert prompt.ask(self._step()) is SudoDecision.ABORT

    def test_invalid_choice_reprompts_then_grants(self):
        buffer = io.StringIO()
        console = Console(file=buffer, width=100, force_terminal=False)
        responses = iter(["garbage", ""])
        prompt = RichSudoPrompt(input_fn=lambda _: next(responses), console=console)
        assert prompt.ask(self._step()) is SudoDecision.GRANT

    def test_renders_sudo_panel_to_console(self):
        buffer = io.StringIO()
        console = Console(file=buffer, width=100, force_terminal=False)
        prompt = RichSudoPrompt(input_fn=lambda _: "", console=console)
        prompt.ask(self._step())
        output = buffer.getvalue()
        assert "elevated permission" in output
        assert "Clear system-level cache in /var/cache" in output

    def test_default_construction_uses_the_real_single_keypress_reader(self, monkeypatch):
        """
        Esc/repeated-"s" bug fix regression test: with no `input_fn`
        override (the real construction path every actual REPL session
        hits), `ask()` must go through
        `ui.session.read_sudo_choice_keypress` -- a real single-keypress
        reader -- rather than falling back to a line-editing `read()` call
        that only submits on Enter (main.py's `RichSudoPrompt(input_fn=read,
        ...)` was the previous, buggy wiring; see this class's own
        docstring for the full history).
        """
        from ohmyshell.ui import session as session_module

        calls = []

        def _fake_keypress():
            calls.append(True)
            return "skip"

        monkeypatch.setattr(session_module, "read_sudo_choice_keypress", _fake_keypress)

        buffer = io.StringIO()
        console = Console(file=buffer, width=100, force_terminal=False)
        prompt = RichSudoPrompt(console=console)  # no input_fn override
        assert prompt.ask(self._step()) is SudoDecision.SKIP
        assert calls == [True]