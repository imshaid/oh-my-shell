"""Tests for ui/panels.py (Build Order Step 11)."""

from __future__ import annotations

import io

import pytest
from rich.console import Console
from rich.panel import Panel

from ohmyshell.danger_classifier import ClassificationResult, Destructive, Safe
from ohmyshell.plan_generator import Plan
from ohmyshell.sudo_layer import ElevatedStep
from ohmyshell.ui.panels import (
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

    def test_includes_all_steps(self):
        text = _render_to_text(render_plan_panel(_plan()))
        assert "Scan /tmp for files older than 7 days" in text
        assert "Move matched files to .trash/" in text

    def test_includes_risk_level(self):
        text = _render_to_text(render_plan_panel(_plan(risk="high")))
        assert "High" in text

    def test_includes_action_in_title(self):
        text = _render_to_text(render_plan_panel(_plan(action="organize_files")))
        assert "organize_files" in text

    def test_includes_confirm_edit_chat_cancel_options(self):
        text = _render_to_text(render_plan_panel(_plan()))
        assert "Confirm" in text
        assert "Edit" in text
        assert "Chat" in text or "chat" in text.lower()
        assert "Cancel" in text

    def test_low_risk_plan_renders_without_error(self):
        text = _render_to_text(render_plan_panel(_plan(risk="low")))
        assert "Low" in text


class TestRenderDestructiveCommandPanel:
    def _result(self, *, trash_alternative_possible: bool, explanation: str = "This deletes things permanently."):
        return ClassificationResult(
            verdict=Destructive(explanation=explanation, trash_alternative_possible=trash_alternative_possible),
            source="regex",
        )

    def test_returns_a_panel(self):
        assert isinstance(render_destructive_command_panel(self._result(trash_alternative_possible=True)), Panel)

    def test_includes_explanation_text(self):
        text = _render_to_text(
            render_destructive_command_panel(self._result(trash_alternative_possible=True, explanation="Deletes /var/log"))
        )
        assert "Deletes /var/log" in text

    def test_includes_trash_option_when_possible(self):
        text = _render_to_text(render_destructive_command_panel(self._result(trash_alternative_possible=True)))
        assert "Move to trash instead" in text

    def test_omits_trash_option_when_not_possible(self):
        text = _render_to_text(render_destructive_command_panel(self._result(trash_alternative_possible=False)))
        assert "Move to trash instead" not in text

    def test_always_includes_run_anyway_and_cancel(self):
        text = _render_to_text(render_destructive_command_panel(self._result(trash_alternative_possible=False)))
        assert "Run anyway" in text
        assert "Cancel" in text

    def test_raises_type_error_for_safe_verdict(self):
        safe_result = ClassificationResult(verdict=Safe(), source="regex")
        with pytest.raises(TypeError):
            render_destructive_command_panel(safe_result)


class TestRenderSudoPanel:
    def _step(self, **overrides) -> ElevatedStep:
        defaults = dict(
            step_number=3,
            total_steps=5,
            description="Clear system-level cache in /var/cache",
            reason="this directory is owned by root",
        )
        defaults.update(overrides)
        return ElevatedStep(**defaults)

    def test_returns_a_panel(self):
        assert isinstance(render_sudo_panel(self._step()), Panel)

    def test_includes_step_progress(self):
        text = _render_to_text(render_sudo_panel(self._step(step_number=3, total_steps=5)))
        assert "3/5" in text

    def test_includes_description_and_reason(self):
        text = _render_to_text(
            render_sudo_panel(self._step(description="Clear system-level cache in /var/cache", reason="this directory is owned by root"))
        )
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