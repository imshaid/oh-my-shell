"""Tests for ui/streaming.py (Build Order Step 11)."""

from __future__ import annotations

import io
import time

from rich.console import Console

from ohmyshell.executor import ExecutionResult, StepEvent, StepResult, StepStatus
from ohmyshell.intent_parser import ParseTelemetry
from ohmyshell.ui.streaming import (
    StreamingRenderer,
    _LiveRunningLine,
    _progress_for_output_line,
    _render_activity_bar,
    _running_label_for_output_line,
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
    """
    Regression coverage for the live-ticking elapsed-time running line
    (added post-Build-Order, per the user's explicit "make the whole shell
    feel alive" request -- see ui/streaming.py's own docstring for why this
    replaced the previously-static Spinner).
    """

    def test_returns_a_live_running_line(self):
        event = StepEvent(step_number=1, total_steps=1, status=StepStatus.RUNNING, detail="find /tmp ...")
        assert isinstance(render_running_line(event), _LiveRunningLine)

    def test_defaults_label_when_no_detail(self):
        event = StepEvent(step_number=1, total_steps=1, status=StepStatus.RUNNING, detail="")
        line = render_running_line(event)
        text = _render_to_text(line)
        assert "Executing..." in text

    def test_shows_the_event_detail_as_label(self):
        event = StepEvent(step_number=1, total_steps=1, status=StepStatus.RUNNING, detail="moving files")
        text = _render_to_text(render_running_line(event))
        assert "moving files" in text

    def test_elapsed_time_genuinely_ticks_between_renders(self):
        """
        The whole point of this class: re-rendering the SAME instance
        later must show a larger elapsed number -- not a value frozen at
        construction time. This is what makes the spinner line "live"
        rather than a static string that merely looks like a timer.
        """
        line = _LiveRunningLine("Executing...")
        first_text = _render_to_text(line)
        time.sleep(0.25)
        second_text = _render_to_text(line)

        def _extract_seconds(text: str) -> float:
            # "⠸ Executing...  ·  0.3s" is the FIRST rendered line; later
            # lines (the activity bar, an optional detail line) also
            # contain "·" separators, so only the head line is parsed.
            head_line = text.splitlines()[0]
            marker = "·  "
            fragment = head_line[head_line.index(marker) + len(marker) :]
            return float(fragment.strip().rstrip("s"))

        assert _extract_seconds(second_text) > _extract_seconds(first_text)

    def test_update_label_changes_what_renders(self):
        line = _LiveRunningLine("Executing...")
        line.update_label("3 files moved  ·  report.pdf")
        text = _render_to_text(line)
        assert "3 files moved" in text
        assert "report.pdf" in text
        assert "Executing..." not in text

    def test_update_label_truncates_a_very_long_label_to_the_tail(self):
        line = _LiveRunningLine("Executing...")
        long_label = "x" * 200 + "TAIL_MARKER"
        line.update_label(long_label)
        text = _render_to_text(line)
        assert "TAIL_MARKER" in text
        assert "…" in text
        assert "x" * 200 not in text


class TestRunningLabelForOutputLine:
    """
    Regression coverage for real per-file live progress (added post-Build-
    Order, per the user's explicit "make the whole shell feel alive, every
    operation" request): parses `mv -v`'s real "renamed 'X' -> 'Y'" output
    (capabilities.json's clean_temp_files/organize_files templates were
    given `-v` specifically so there's genuine per-file output to parse)
    into a running file count + latest filename, and falls back to the raw
    line verbatim for any other capability's output.
    """

    def test_first_mv_verbose_line_counts_as_one_file(self):
        label, count = _running_label_for_output_line(
            "renamed '/tmp/old.log' -> '/home/user/.oh-my-shell/.trash/old.log'",
            files_moved_so_far=0,
        )
        assert count == 1
        assert "1 file moved" in label
        assert "old.log" in label

    def test_count_accumulates_across_successive_lines(self):
        label1, count1 = _running_label_for_output_line(
            "renamed '/tmp/a.log' -> '/trash/a.log'", files_moved_so_far=0
        )
        label2, count2 = _running_label_for_output_line(
            "renamed '/tmp/b.log' -> '/trash/b.log'", files_moved_so_far=count1
        )
        assert count1 == 1
        assert count2 == 2
        assert "2 files moved" in label2  # plural once count > 1

    def test_non_mv_output_is_shown_verbatim(self):
        """
        A capability whose output isn't `mv -v` (e.g. list_processes' `ps`
        output) must still be shown, not silently swallowed just because
        this parser doesn't recognize its shape.
        """
        label, count = _running_label_for_output_line(
            "1234  0.5%  chrome --type=renderer", files_moved_so_far=0
        )
        assert label == "1234  0.5%  chrome --type=renderer"
        assert count == 0  # unrecognized shape never increments the file counter


class TestStreamingRendererOnOutputLine:
    def test_updates_the_active_running_line(self):
        buffer = io.StringIO()
        console = Console(file=buffer, width=100, force_terminal=False)
        with StreamingRenderer(console=console) as renderer:
            renderer.on_event(StepEvent(step_number=1, total_steps=1, status=StepStatus.RUNNING, detail="Executing..."))
            renderer.on_output_line("renamed '/tmp/a.log' -> '/trash/a.log'")
            text = _render_to_text(renderer._running_line)
        assert "1 file" in text
        assert "a.log" in text

    def test_file_count_accumulates_across_multiple_output_lines(self):
        buffer = io.StringIO()
        console = Console(file=buffer, width=100, force_terminal=False)
        with StreamingRenderer(console=console) as renderer:
            renderer.on_event(StepEvent(step_number=1, total_steps=1, status=StepStatus.RUNNING, detail="Executing..."))
            renderer.on_output_line("renamed '/tmp/a.log' -> '/trash/a.log'")
            renderer.on_output_line("renamed '/tmp/b.log' -> '/trash/b.log'")
            renderer.on_output_line("renamed '/tmp/c.log' -> '/trash/c.log'")
            text = _render_to_text(renderer._running_line)
        assert "3 files" in text
        assert "c.log" in text

    def test_file_count_resets_for_a_new_running_step(self):
        """
        A fresh RUNNING event (a new command) must start its own file
        count from zero, not continue accumulating from a previous step's
        count -- each command's progress is independent.
        """
        buffer = io.StringIO()
        console = Console(file=buffer, width=100, force_terminal=False)
        with StreamingRenderer(console=console) as renderer:
            renderer.on_event(StepEvent(step_number=1, total_steps=1, status=StepStatus.RUNNING, detail="first"))
            renderer.on_output_line("renamed '/tmp/a.log' -> '/trash/a.log'")
            renderer.on_event(StepEvent(step_number=1, total_steps=1, status=StepStatus.DONE, detail="done"))
            renderer.on_event(StepEvent(step_number=1, total_steps=1, status=StepStatus.RUNNING, detail="second"))
            renderer.on_output_line("renamed '/tmp/x.log' -> '/trash/x.log'")
            text = _render_to_text(renderer._running_line)
        assert "1 file" in text  # not "2 files"
        assert "x.log" in text

    def test_before_running_event_is_a_noop(self):
        buffer = io.StringIO()
        console = Console(file=buffer, width=100, force_terminal=False)
        renderer = StreamingRenderer(console=console)
        renderer.on_output_line("renamed '/tmp/a.log' -> '/trash/a.log'")  # must not raise

    def test_after_terminal_event_is_a_noop(self):
        buffer = io.StringIO()
        console = Console(file=buffer, width=100, force_terminal=False)
        with StreamingRenderer(console=console) as renderer:
            renderer.on_event(StepEvent(step_number=1, total_steps=1, status=StepStatus.RUNNING, detail="x"))
            renderer.on_event(StepEvent(step_number=1, total_steps=1, status=StepStatus.DONE, detail="done"))
            renderer.on_output_line("renamed '/tmp/a.log' -> '/trash/a.log'")  # must not raise
            assert renderer._running_line is None


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

    def test_interrupted_also_shows_undo_hint(self):
        """
        Regression test (found via manual end-to-end testing, in a real
        terminal session): a single-Ctrl+C graceful stop used to show no
        undo option at all, even though Section 8.3.4's own mockup shows
        "[u] Undo what was moved" right alongside an interrupted stop, and
        whatever completed before the interrupt (files already moved into
        .trash/) is exactly as undoable as a fully-finished run's files --
        the audit log records status="interrupted" with the same action_id
        trash.py's undo looks up regardless of how the run ended.
        """
        result = self._result(StepStatus.INTERRUPTED, interrupted=True)
        text = _render_to_text(render_execution_summary(result))
        assert "Undo this action" in text

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


class TestExecutionSummaryAiTelemetryLine:
    """
    Regression coverage (added post-Build-Order, found via manual
    end-to-end testing): the "AI: N tokens total · Ns reasoning time" line
    from Section 8.3.4's mockup used to be entirely unrendered, even though
    intent_parser.OllamaBackend already reads real token/timing numbers off
    every ollama.chat() response into ParseTelemetry -- the data existed,
    it just never reached this renderer.
    """

    def _result(self, status: StepStatus = StepStatus.DONE, **overrides) -> ExecutionResult:
        defaults = dict(interrupted=False, aborted_for_sudo=False)
        defaults.update(overrides)
        step = StepResult(step_number=1, description="Clean temp files", status=status)
        return ExecutionResult(action="clean_temp_files", step_results=[step], **defaults)

    def test_no_telemetry_omits_the_line_entirely(self):
        text = _render_to_text(render_execution_summary(self._result()))
        assert "AI:" not in text

    def test_full_telemetry_shows_total_tokens_and_reasoning_time(self):
        telemetry = ParseTelemetry(tokens_in=94, tokens_out=62, duration_seconds=0.8, model="qwen3:8b")
        text = _render_to_text(render_execution_summary(self._result(), telemetry=telemetry))
        assert "AI: 156 tokens total · 0.8s reasoning time" in text

    def test_partial_telemetry_shows_only_available_fields(self):
        telemetry = ParseTelemetry(tokens_in=None, tokens_out=None, duration_seconds=1.2, model=None)
        text = _render_to_text(render_execution_summary(self._result(), telemetry=telemetry))
        assert "AI: 1.2s reasoning time" in text
        assert "tokens total" not in text

    def test_empty_telemetry_object_omits_the_line(self):
        telemetry = ParseTelemetry()
        text = _render_to_text(render_execution_summary(self._result(), telemetry=telemetry))
        assert "AI:" not in text

    def test_telemetry_line_also_appears_on_an_interrupted_result(self):
        telemetry = ParseTelemetry(tokens_in=10, tokens_out=5, duration_seconds=0.3)
        result = self._result(StepStatus.INTERRUPTED, interrupted=True)
        text = _render_to_text(render_execution_summary(result, telemetry=telemetry))
        assert "AI: 15 tokens total · 0.3s reasoning time" in text
        assert "Stopped early" in text


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

class TestStreamingRendererSudoPause:
    """
    Regression tests for the sudo password-prompt garbling bug fix (see
    this module's own `pause_for_sudo`/`resume_after_sudo` docstring
    comments): a real `sudo <command>` process reads/writes its password
    prompt directly on the controlling terminal, which collided with
    `Live`'s own repaint loop still running throughout that blocking call.
    These hooks must stop/start the underlying `Live` display, and only
    when `used_sudo` is True -- an ordinary command's spinner must be
    completely unaffected.
    """

    def test_pause_for_sudo_stops_live_when_used_sudo_true(self):
        buffer = io.StringIO()
        console = Console(file=buffer, width=100, force_terminal=False)
        with StreamingRenderer(console=console) as renderer:
            assert renderer._live is not None
            assert renderer._live.is_started
            renderer.pause_for_sudo(True)
            assert not renderer._live.is_started

    def test_resume_after_sudo_restarts_live_when_used_sudo_true(self):
        buffer = io.StringIO()
        console = Console(file=buffer, width=100, force_terminal=False)
        with StreamingRenderer(console=console) as renderer:
            renderer.pause_for_sudo(True)
            assert not renderer._live.is_started
            renderer.resume_after_sudo(True)
            assert renderer._live.is_started

    def test_pause_for_sudo_is_a_noop_when_used_sudo_false(self):
        buffer = io.StringIO()
        console = Console(file=buffer, width=100, force_terminal=False)
        with StreamingRenderer(console=console) as renderer:
            renderer.pause_for_sudo(False)
            assert renderer._live.is_started  # untouched -- not a sudo call

    def test_resume_after_sudo_is_a_noop_when_used_sudo_false(self):
        buffer = io.StringIO()
        console = Console(file=buffer, width=100, force_terminal=False)
        with StreamingRenderer(console=console) as renderer:
            renderer.pause_for_sudo(True)
            renderer.resume_after_sudo(False)  # wrong flag -- must not resume
            assert not renderer._live.is_started

    def test_pause_before_enter_does_not_raise(self):
        buffer = io.StringIO()
        console = Console(file=buffer, width=100, force_terminal=False)
        renderer = StreamingRenderer(console=console)
        renderer.pause_for_sudo(True)  # no Live yet -- must not crash
        renderer.resume_after_sudo(True)

class TestRenderActivityBar:
    """
    The animated, indeterminate progress bar (added post-Build-Order, per
    the user's explicit "professional, animated and rich, not a toy"
    request) -- a highlighted segment sweeps across the bar purely as a
    function of elapsed time, same "no stored/advanced state" trick
    Spinner.render(t) already uses (see _LiveRunningLine's docstring).
    """

    def test_bar_has_fixed_visual_width_regardless_of_elapsed_time(self):
        for elapsed in (0.0, 0.7, 1.6, 3.3, 100.0):
            text = _render_activity_bar(elapsed).plain
            # Fixed cell count between the two border characters.
            assert len(text) == 30  # 28 cells + 2 border chars

    def test_bar_position_changes_as_elapsed_time_advances(self):
        early = _render_activity_bar(0.0).plain
        later = _render_activity_bar(0.8).plain
        assert early != later

    def test_bar_sweep_is_periodic(self):
        # A full there-and-back sweep takes 2 * _BAR_CYCLE_SECONDS (3.2s);
        # the same elapsed time modulo that period must render identically.
        first = _render_activity_bar(0.5).plain
        one_cycle_later = _render_activity_bar(0.5 + 3.2).plain
        assert first == one_cycle_later


class TestProgressForOutputLine:
    """
    `_progress_for_output_line` -- the pure parsing step behind
    `StreamingRenderer.on_output_line`, returning (label, count, detail)
    so the live renderer can show a real count AND the actual filename on
    its own line, not just a flat label string.
    """

    def test_mv_verbose_line_yields_bare_filename_as_detail(self):
        _label, count, detail = _progress_for_output_line(
            "renamed '/tmp/a.log' -> '/trash/a.log'", files_moved_so_far=0
        )
        assert count == 1
        assert detail == "a.log"  # not the whole "renamed '...' -> '...'" line

    def test_non_mv_line_is_shown_verbatim_as_detail_with_no_count_bump(self):
        _label, count, detail = _progress_for_output_line(
            "ps output line, not mv -v", files_moved_so_far=2
        )
        assert count == 2  # unchanged
        assert detail == "ps output line, not mv -v"


class TestLiveRunningLineNoteLine:
    """
    `_LiveRunningLine.note_line` -- the richer replacement for a bare
    `update_label` overwrite: the spinner's own label (the step
    description) stays put, while a real count/rate and the latest detail
    line render underneath, updated independently.
    """

    def test_head_label_is_unchanged_by_note_line(self):
        line = _LiveRunningLine("Cleaning up...")
        line.note_line(count=5, noun="files", detail="report.pdf")
        text = _render_to_text(line)
        assert "Cleaning up..." in text.splitlines()[0]

    def test_count_and_detail_appear_on_their_own_lines(self):
        line = _LiveRunningLine("Cleaning up...")
        line.note_line(count=1, noun="files", detail="report.pdf")
        text = _render_to_text(line)
        lines = text.splitlines()
        assert len(lines) == 3  # head, bar+stats, detail
        assert "1 files" in lines[1]
        assert "report.pdf" in lines[2]

    def test_a_line_with_no_count_still_shows_a_detail_line(self):
        # Generic (non-mv) command output has nothing countable, but the
        # raw line itself must still be visible -- this is the "every
        # single operation, not just mv" part of the request.
        line = _LiveRunningLine("Listing processes...")
        line.note_line(count=None, noun="items", detail="root  1  0.0  0.1 /sbin/init")
        text = _render_to_text(line)
        assert "root  1  0.0  0.1 /sbin/init" in text
        # No count was ever given, so no bare number should be invented.
        assert " items" not in text.replace("0 items", "")

    def test_rate_is_shown_once_meaningfully_measurable(self):
        line = _LiveRunningLine("Cleaning up...")
        line._start = time.monotonic() - 2.0  # pretend 2s have already passed
        line.note_line(count=10, noun="files", detail="z.log")
        text = _render_to_text(line)
        assert "/s" in text

    def test_very_long_detail_line_is_tail_truncated(self):
        line = _LiveRunningLine("Executing...")
        long_detail = "x" * 200
        line.note_line(count=None, noun="items", detail=long_detail)
        text = _render_to_text(line)
        detail_line = text.splitlines()[2]
        assert len(detail_line.strip()) <= 79  # _MAX_DETAIL_LINE_CHARS + ellipsis
        assert detail_line.strip().startswith("…")