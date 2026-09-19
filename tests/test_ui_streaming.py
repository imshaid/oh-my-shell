"""Tests for ui/streaming.py (Build Order Step 11)."""

from __future__ import annotations

import io

from rich.console import Console
from rich.spinner import Spinner

from ohmyshell.executor import ExecutionResult, StepEvent, StepResult, StepStatus
from ohmyshell.ui.streaming import (
    StreamingRenderer,
    render_execution_summary,
    render_result_line,
    render_running_line,
    streaming,
)


def _render_to_text(renderable, width: int = 100) -> str:
    buffer = io.StringIO()
    console = Console(file=buffer, width=width, force_terminal=False)
    console.print(renderable)
    return buffer.getvalue()


class TestRenderRunningLine:
    def test_returns_a_spinner(self):
        event = StepEvent(step_number=1, total_steps=1, status=StepStatus.RUNNING, detail="find /tmp ...")
        assert isinstance(render_running_line(event), Spinner)

    def test_defaults_label_when_no_detail(self):
        event = StepEvent(step_number=1, total_steps=1, status=StepStatus.RUNNING, detail="")
        spinner = render_running_line(event)
        assert spinner is not None  # smoke test — Spinner text isn't trivially introspectable


class TestRenderResultLine:
    def test_done_status_shows_checkmark_and_done(self):
        event = StepEvent(step_number=1, total_steps=1, status=StepStatus.DONE, detail="340 files removed")
        text = _render_to_text(render_result_line(event))
        assert "✓" in text
        assert "Done" in text
        assert "340 files removed" in text

    def test_failed_status_shows_x_and_failed(self):
        event = StepEvent(step_number=1, total_steps=1, status=StepStatus.FAILED, detail="permission denied")
        text = _render_to_text(render_result_line(event))
        assert "✗" in text
        assert "Failed" in text

    def test_interrupted_status_shows_warning_and_interrupted(self):
        event = StepEvent(step_number=1, total_steps=1, status=StepStatus.INTERRUPTED, detail="")
        text = _render_to_text(render_result_line(event))
        assert "⚠" in text
        assert "Interrupted" in text

    def test_skipped_status_shows_skipped(self):
        event = StepEvent(step_number=1, total_steps=1, status=StepStatus.SKIPPED, detail="")
        text = _render_to_text(render_result_line(event))
        assert "Skipped" in text

    def test_no_detail_suffix_when_detail_empty(self):
        event = StepEvent(step_number=1, total_steps=1, status=StepStatus.DONE, detail="")
        text = _render_to_text(render_result_line(event))
        assert "—" not in text


class TestRenderExecutionSummary:
    def _result(self, status: StepStatus, **overrides) -> ExecutionResult:
        defaults = dict(interrupted=False, aborted_for_sudo=False)
        defaults.update(overrides)
        step = StepResult(step_number=1, description="Clean temp files", status=status)
        return ExecutionResult(action="clean_temp_files", step_results=[step], **defaults)

    def test_all_done_shows_undo_hint(self):
        result = self._result(StepStatus.DONE)
        text = _render_to_text(render_execution_summary(result))
        assert "Undo this action" in text

    def test_interrupted_shows_stopped_message(self):
        result = self._result(StepStatus.INTERRUPTED, interrupted=True)
        text = _render_to_text(render_execution_summary(result))
        assert "Stopped early" in text
        assert "Undo this action" not in text

    def test_aborted_for_sudo_shows_aborted_message(self):
        result = self._result(StepStatus.INTERRUPTED, aborted_for_sudo=True)
        text = _render_to_text(render_execution_summary(result))
        assert "Aborted" in text

    def test_failed_step_does_not_show_undo_hint(self):
        result = self._result(StepStatus.FAILED)
        text = _render_to_text(render_execution_summary(result))
        assert "Undo this action" not in text

    def test_includes_step_description(self):
        result = self._result(StepStatus.DONE)
        text = _render_to_text(render_execution_summary(result))
        assert "Clean temp files" in text


class TestStreamingRenderer:
    def test_on_event_updates_live_display_without_raising(self):
        buffer = io.StringIO()
        console = Console(file=buffer, width=100, force_terminal=False)
        with StreamingRenderer(console=console) as renderer:
            renderer.on_event(StepEvent(step_number=1, total_steps=1, status=StepStatus.RUNNING, detail="working..."))
            renderer.on_event(StepEvent(step_number=1, total_steps=1, status=StepStatus.DONE, detail="done"))
        # No exception means success; Live's transient output isn't asserted line-by-line.

    def test_on_event_before_enter_is_a_noop(self):
        buffer = io.StringIO()
        console = Console(file=buffer, width=100, force_terminal=False)
        renderer = StreamingRenderer(console=console)
        renderer.on_event(StepEvent(step_number=1, total_steps=1, status=StepStatus.RUNNING))  # should not raise

    def test_print_summary_writes_to_console(self):
        buffer = io.StringIO()
        console = Console(file=buffer, width=100, force_terminal=False)
        renderer = StreamingRenderer(console=console)
        step = StepResult(step_number=1, description="Clean temp files", status=StepStatus.DONE)
        result = ExecutionResult(action="clean_temp_files", step_results=[step])
        renderer.print_summary(result)
        assert "Clean temp files" in buffer.getvalue()

    def test_context_manager_form(self):
        buffer = io.StringIO()
        console = Console(file=buffer, width=100, force_terminal=False)
        with streaming(console=console) as renderer:
            assert isinstance(renderer, StreamingRenderer)