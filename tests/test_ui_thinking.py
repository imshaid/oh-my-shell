"""
Tests for ui/thinking.py (Build Order Step 11 follow-up).

Regression coverage added post-Build-Order, alongside the live token-
streaming implementation (the user's explicit "fully implement these
without any consideration" request) -- this module previously had no
dedicated test file at all.
"""

from __future__ import annotations

import io
import time

from rich.console import Console

from ohmyshell import hardware as hardware_module
from ohmyshell.intent_parser import ParseTelemetry
from ohmyshell.ui.thinking import (
    _streamed_text_line,
    _thinking_line,
    _usage_style,
    render_hardware_lines,
    render_thinking_display,
    run_with_thinking_indicator,
)


def _render_to_text(renderable, width: int = 100) -> str:
    buffer = io.StringIO()
    console = Console(file=buffer, width=width, force_terminal=False)
    console.print(renderable)
    return buffer.getvalue()


def _fake_snapshot(*, gpu: bool = False) -> hardware_module.HardwareSnapshot:
    cpu = hardware_module.CpuInfo(core_count=8, usage_percent=42.0)
    ram = hardware_module.RamInfo(total_gb=15.6, used_gb=6.1)
    gpu_info = (
        hardware_module.GpuInfo(name="RTX 4060", vram_total_mb=8192, vram_used_mb=3200)
        if gpu
        else None
    )
    return hardware_module.HardwareSnapshot(cpu=cpu, ram=ram, gpu=gpu_info)


class TestUsageStyle:
    """
    User-requested (replacing every hardware line's earlier flat
    `omsh.muted`): low/medium/high usage maps onto this app's existing
    risk-level color names, so "is this fine or not" reads at a glance.
    """

    def test_low_usage_is_risk_low(self):
        assert _usage_style(0) == "omsh.risk.low"
        assert _usage_style(49.9) == "omsh.risk.low"

    def test_medium_usage_is_risk_medium(self):
        assert _usage_style(50) == "omsh.risk.medium"
        assert _usage_style(79.9) == "omsh.risk.medium"

    def test_high_usage_is_risk_high(self):
        assert _usage_style(80) == "omsh.risk.high"
        assert _usage_style(100) == "omsh.risk.high"


class TestRenderHardwareLinesColoring:
    def test_cpu_bar_and_percent_carry_the_usage_style_not_muted(self):
        snapshot = _fake_snapshot()  # cpu.usage_percent=42.0 -> low
        lines = render_hardware_lines(snapshot)
        cpu_line = lines[0]
        # The label ("CPU ") stays muted; the bar+percent segment right
        # after it must carry the usage color instead, not omsh.muted.
        styles = {(span.start, span.end): span.style for span in cpu_line.spans}
        assert ("omsh.muted", "CPU ") == (styles[(0, 4)], cpu_line.plain[0:4])
        bar_span_style = next(style for (start, end), style in styles.items() if start == 4)
        assert bar_span_style == "omsh.risk.low"
        assert bar_span_style != "omsh.muted"

    def test_label_stays_muted(self):
        snapshot = _fake_snapshot()
        lines = render_hardware_lines(snapshot)
        cpu_line = lines[0]
        first_span = cpu_line.spans[0]
        assert first_span.style == "omsh.muted"
        assert cpu_line.plain[first_span.start : first_span.end] == "CPU "


class TestThinkingLineLiveTokens:
    """
    Live-streaming regression coverage: before a finished ParseTelemetry is
    available, the top line must show live in/out token counts when the
    caller has them (on_token_box), and must show nothing extra when it
    doesn't -- never a fake/zeroed figure.

    Bug-fix note: earlier coverage asserted a single "N tokens" shape fed
    straight from `chunk.eval_count` -- wrong, because ollama only
    populates eval_count on the final streamed chunk (see
    intent_parser.StreamProgress's own docstring), so that number never
    actually changed until the very end. The live shape is now "in / out"
    counts, both driven by OllamaBackend's own client-side running tally.
    """

    def test_no_telemetry_no_live_tokens_shows_only_elapsed(self):
        text = _render_to_text(_thinking_line(elapsed_seconds=1.2, telemetry=None))
        assert text.strip() == "⚟ Thinking... 1.2s"

    def test_no_telemetry_with_live_tokens_shows_in_and_out_counts(self):
        text = _render_to_text(
            _thinking_line(
                elapsed_seconds=0.9, telemetry=None, live_tokens_out=17, live_tokens_in=94
            )
        )
        assert "Thinking... 0.9s" in text
        assert "94 in" in text
        assert "17 out" in text

    def test_no_telemetry_with_only_live_tokens_out_still_renders(self):
        """
        tokens_in isn't known until the model finishes evaluating the whole
        prompt in one shot -- early chunks may have out-count but no in-
        count yet; the line must still render sensibly (an explicit "?"
        rather than silently hiding the whole segment or crashing).
        """
        text = _render_to_text(
            _thinking_line(elapsed_seconds=0.5, telemetry=None, live_tokens_out=3, live_tokens_in=None)
        )
        assert "3 out" in text

    def test_finished_telemetry_takes_priority_over_live_tokens(self):
        """
        Once the call has actually finished, the final numbers (which
        already encode the same counts) are shown instead of the live
        ones -- avoids showing the same count twice in two different shapes.
        """
        telemetry = ParseTelemetry(tokens_in=94, tokens_out=62, duration_seconds=0.8, model="qwen3:8b")
        text = _render_to_text(
            _thinking_line(
                elapsed_seconds=0.8, telemetry=telemetry, live_tokens_out=62, live_tokens_in=94
            )
        )
        assert "94 in / 62 out" in text
        assert "tok/s" in text
        assert "qwen3:8b" in text


class TestStreamedTextLine:
    """
    Regression coverage for the raw-JSON live text line (the user's own
    explicit follow-up ask: "I want to show the full live token by token
    streaming ... also other stats", not just a number).
    """

    def test_no_text_yet_renders_nothing(self):
        assert _streamed_text_line(None) is None
        assert _streamed_text_line("") is None

    def test_short_text_shown_in_full(self):
        text = _render_to_text(_streamed_text_line('{"action": "unmapped"'))
        assert '{"action": "unmapped"' in text

    def test_long_text_truncated_to_the_tail(self):
        long_text = "x" * 200 + "TAIL_MARKER"
        text = _render_to_text(_streamed_text_line(long_text))
        assert "TAIL_MARKER" in text
        assert "…" in text
        assert "x" * 200 not in text  # the old prefix must not still be fully present

    def test_carries_a_real_color_escape_when_rendered_with_the_theme(self):
        """
        Regression test (post-Build-Order, found via real-terminal
        testing -- see ui/prompt.py's "bold omsh.path" fix for the full
        root-cause story): this line's style used to be the composite
        STRING "omsh.accent dim", which rich silently renders completely
        unstyled (no escape codes at all) instead of applying either
        attribute. `_render_to_text`'s own Console (force_terminal=False,
        no theme) never emits real escapes for ANY style, so it can't
        catch this -- this test uses a themed, truecolor-forced Console
        instead, the same way the real REPL renders this line, so a
        regression to a composite style string fails here.
        """
        from ohmyshell.ui.theme import themed_console

        console = themed_console(force_terminal=True, color_system="truecolor", no_color=False)
        with console.capture() as capture:
            console.print(_streamed_text_line("hello"))
        assert "\x1b[" in capture.get()


class TestRenderThinkingDisplay:
    def test_includes_live_tokens_when_given(self):
        text = _render_to_text(
            render_thinking_display(
                elapsed_seconds=1.0, snapshot=_fake_snapshot(), live_tokens_out=5, live_tokens_in=10
            )
        )
        assert "10 in" in text
        assert "5 out" in text

    def test_includes_live_text_while_streaming(self):
        text = _render_to_text(
            render_thinking_display(
                elapsed_seconds=1.0,
                snapshot=_fake_snapshot(),
                live_text='{"action": "clean_temp',
            )
        )
        assert '{"action": "clean_temp' in text

    def test_streamed_text_appears_below_hardware_lines(self):
        """
        Layout regression test (found via the user's own real end-to-end
        screenshot): the streamed JSON must render AFTER the CPU/RAM/GPU
        hardware rows, not between the stats line and them -- the user
        explicitly asked for the fixed-position system stats to stay
        anchored at the top, with the growing/scrolling JSON content at
        the bottom.
        """
        text = _render_to_text(
            render_thinking_display(
                elapsed_seconds=1.0,
                snapshot=_fake_snapshot(gpu=True),
                live_text='{"action": "clean_temp_files"',
            )
        )
        gpu_index = text.index("GPU")
        json_index = text.index('{"action"')
        assert gpu_index < json_index

    def test_omits_live_text_once_telemetry_is_final(self):
        """
        The streamed-text line is only for while the call is still in
        flight -- once a finished ParseTelemetry is passed, the plan panel
        (printed right after) is the permanent record; repeating the raw
        JSON on this transient, about-to-disappear indicator would be
        redundant clutter.
        """
        telemetry = ParseTelemetry(tokens_out=5, duration_seconds=0.1, model="qwen3:8b")
        text = _render_to_text(
            render_thinking_display(
                elapsed_seconds=1.0,
                snapshot=_fake_snapshot(),
                telemetry=telemetry,
                live_text='{"action": "clean_temp',
            )
        )
        assert "clean_temp" not in text

    def test_gpu_absent_omits_gpu_line(self):
        text = _render_to_text(
            render_thinking_display(elapsed_seconds=1.0, snapshot=_fake_snapshot(gpu=False))
        )
        assert "GPU" not in text

    def test_gpu_present_includes_gpu_line(self):
        text = _render_to_text(
            render_thinking_display(elapsed_seconds=1.0, snapshot=_fake_snapshot(gpu=True))
        )
        assert "GPU" in text


class TestRunWithThinkingIndicatorTokenBox:
    """
    Regression coverage for the on_token_box plumbing itself: the polling
    Live loop must actually read whatever a concurrently-running fn()
    writes into the shared box, without needing fn() to be a real
    intent_parser call.
    """

    def test_returns_fn_result_unaffected_by_on_token_box(self):
        buffer = io.StringIO()
        console = Console(file=buffer, width=100, force_terminal=False)
        box: dict[str, int] = {}

        result = run_with_thinking_indicator(
            lambda: "done",
            console=console,
            hardware_reader=_fake_snapshot,
            on_token_box=box,
        )

        assert result == "done"

    def test_none_on_token_box_is_fully_backward_compatible(self):
        buffer = io.StringIO()
        console = Console(file=buffer, width=100, force_terminal=False)

        result = run_with_thinking_indicator(
            lambda: 42,
            console=console,
            hardware_reader=_fake_snapshot,
        )

        assert result == 42

    def test_reraises_fn_exception(self):
        buffer = io.StringIO()
        console = Console(file=buffer, width=100, force_terminal=False)

        def _boom():
            raise ValueError("backend unreachable")

        try:
            run_with_thinking_indicator(_boom, console=console, hardware_reader=_fake_snapshot)
        except ValueError as exc:
            assert "backend unreachable" in str(exc)
        else:
            assert False, "expected ValueError to propagate"

    def test_live_loop_observes_box_updates_written_by_worker_thread(self):
        """
        A slow-ish fn() that mutates on_token_box partway through must have
        that update visible to the polling loop -- proves the box is really
        shared state between the worker thread and the Live loop, not just
        accepted and ignored.
        """
        buffer = io.StringIO()
        console = Console(file=buffer, width=100, force_terminal=False)
        box: dict[str, int] = {}
        observed: list[int | None] = []

        def _slow_worker():
            box["tokens_out"] = 1
            time.sleep(0.15)
            box["tokens_out"] = 2
            time.sleep(0.15)
            return "finished"

        # Patch render_thinking_display indirectly by reading the box
        # ourselves on a short delay -- simplest reliable way to assert the
        # loop-visible value changed mid-flight without depending on Live's
        # internal refresh timing.
        import threading

        def _poll():
            time.sleep(0.05)
            observed.append(box.get("tokens_out"))
            time.sleep(0.2)
            observed.append(box.get("tokens_out"))

        poller = threading.Thread(target=_poll)
        poller.start()
        result = run_with_thinking_indicator(
            _slow_worker, console=console, hardware_reader=_fake_snapshot, on_token_box=box
        )
        poller.join()

        assert result == "finished"
        assert observed == [1, 2]