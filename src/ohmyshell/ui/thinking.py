"""
Live Hardware Load Indicator + "Thinking..." display (Build Order Step 11
follow-up, Section 6 Core Feature #14 + Section 8.3.3's confirmed mockup).

Section 6, Core Feature #14 (verbatim): "**Live Hardware Load Indicator**
— শুধু AI-thinking-window-এ (CPU/RAM সবসময়, GPU tiered-detection সাপেক্ষে,
integrated GPU-তে চুপচাপ hide)". Section 8.3.3's confirmed mockup is the
base for the CPU/RAM/GPU line shape, extended here with fan RPM, CPU model,
GPU temperature, and tokens/sec:

    ⚟ Thinking... 0.8s · 12 in / 34 out · 43 tok/s · gemini-3.5-flash-lite
    CPU ▓▓▓▓▓▓▓░░░ 82% · 58°C · 2100 RPM · 13th Gen i7-13650HX
    RAM ▓▓▓▓▓▓▓▓░░ 39% · 6.1/15.6GB
    GPU ▓▓▓▓░░░░░░ 39% VRAM · 61°C · RTX 4060
    {"action": "clean_temp_files", "risk": "medium", "para

One metric-group per hardware line, with the bar column starting at the
same character position on every line regardless of how long that line's
label is. Network and disk I/O are intentionally excluded -- mostly-idle
numbers there would just be visual noise.

Token count and elapsed time update live in place (Section 8.3.3), and the
raw generated text streams live underneath it -- not just a growing
number, but the actual text the model is producing, plus live in/out
counts.

Only the final streamed chunk from the backend carries real token counts;
intent_parser.StreamProgress instead tracks a genuinely growing
`tokens_out` (a running chunk count kept by the backend's own generate()
method) and the full `text_so_far` accumulated raw JSON text -- this
module just renders whatever the latest StreamProgress says, every UI
frame, via `on_token_box`.

`run_with_thinking_indicator` accepts an optional `on_token_box` (a plain
dict the caller's own on_token closure writes into every chunk, e.g.
`{"tokens_out": progress.tokens_out, "text": progress.text_so_far,
"tokens_in": progress.tokens_in}`) and reads it every UI frame from the
polling Live loop below, the same shared-mutable-state shape
`result_box`/`error_box` already use for getting the worker thread's
outcome back to the main thread. Before the first chunk arrives (a real,
if usually sub-second, gap while the model loads/starts generating) the
display still shows only the elapsed timer and hardware bars, same as
always -- there is nothing live to show yet, and this module has never
shown a fake or zeroed figure. Once chunks start arriving, the token
counts and text line update in place every frame; tok/s + model name
(which need the FINAL duration/model, not available until the call
completes) still only appear once a finished ParseTelemetry is passed in.

--- Why a background thread ---
parse_intent() is a single blocking call from this module's own point of
view (it returns once, at the very end) -- the only way to show a live-
updating indicator alongside it is to run that call on its own thread
while the main thread drives a `rich.live.Live` refresh loop, polling
`Thread.is_alive()`. `ui/streaming.py`'s StreamingRenderer uses a
different shape (there, run_plan() calls back into on_event() itself, so
no second thread is needed); here, nothing calls back into this thread,
so this module supplies the thread -- intent_parser's own `on_token`
callback runs on the worker thread, writing into the shared
`on_token_box` dict that this module's main-thread loop reads.
`run_with_thinking_indicator()` is the whole public surface: it runs any
zero-arg callable (in practice, a `lambda: parse_intent(...)` closure from
main.py) on a worker thread, shows the indicator while it runs, and
returns the callable's return value (or re-raises its exception) once the
thread finishes -- callers don't need to know a thread was involved at all.

--- Hardware refresh cadence ---
hardware.read_cpu()'s own psutil.cpu_percent(interval=0.1) call blocks for
100ms to produce a real (non-instantaneous, non-zero-on-first-call)
reading -- sampling it every UI-refresh tick (8/sec, matching
StreamingRenderer's own rate) would spend most of the indicator's time
just measuring CPU rather than showing anything. Hardware is instead
sampled on its own slower interval (HARDWARE_REFRESH_SECONDS, from the
main polling loop, not a third thread) while the elapsed-time line and the
streamed text still tick/grow every UI frame -- those stay perfectly live,
and the hardware bars update a few times a second rather than once per
whole "Thinking..." call, still fast enough to read as "live" for the
multi-second durations this indicator is actually shown for.

--- GPU: "tiered-detection সাপেক্ষে, integrated GPU-তে চুপচাপ hide" ---
Handled the same way hardware.py's own read_gpu()/render_system_line()
handle it: when hardware.read_gpu() returns None (no discrete NVIDIA GPU
detected), the whole GPU line is omitted silently, the same as a None
temperature/fan/model reading omits just its own segment.
"""

from __future__ import annotations

import threading
import time
from typing import Callable, TypeVar

from rich.console import Console, Group
from rich.live import Live
from rich.style import Style
from rich.text import Text

from ohmyshell import hardware as hardware_module
from ohmyshell.intent_parser import ParseTelemetry
from ohmyshell.ui.theme import OMSH_THEME, themed_console

T = TypeVar("T")

UI_REFRESH_PER_SECOND = 8
HARDWARE_REFRESH_SECONDS = 0.8
_BAR_WIDTH = 10
_STREAM_TEXT_MAX_CHARS = 78  # keeps the streamed-JSON line to roughly one terminal row


def _bar(percent: float, *, width: int = _BAR_WIDTH) -> str:
    """
    Filled/empty block-character bar, e.g. "▓▓▓▓▓▓▓░░░" for 70% at width
    10 -- matching Section 8.3.3's exact glyph choice (▓ filled, ░ empty).
    Clamped to [0, 100] since a percentage source (CPU especially, on a
    brief burst) can occasionally read fractionally over 100.
    """
    clamped = max(0.0, min(100.0, percent))
    filled = round(clamped / 100 * width)
    return "▓" * filled + "░" * (width - filled)


# Usage-based dynamic color: low/medium/high usage maps onto the same three
# semantic shades this app already uses elsewhere for exactly this
# distinction (ui/panels.py's risk-level text, ui/streaming.py's
# step-status glyphs) -- reusing `OMSH_THEME`'s own `omsh.risk.*` names
# keeps "what green/yellow/red mean" consistent across the whole app.
# Thresholds (< 50% low, < 80% medium, else high) match common
# system-monitor convention (htop/fastfetch's own bar coloring).
def _usage_style(percent: float) -> str:
    if percent < 50:
        return "omsh.risk.low"
    if percent < 80:
        return "omsh.risk.medium"
    return "omsh.risk.high"


_ROW_LABEL_WIDTH = len("CPU")  # "CPU"/"RAM"/"GPU" are always this length


def render_hardware_lines(snapshot: hardware_module.HardwareSnapshot) -> list[Text]:
    """
    The three (or two, GPU-absent) per-metric lines, their bar columns
    aligned to a common start position. Labels are always the bare
    "CPU"/"RAM"/"GPU" (never "CPU (<model>)"), so alignment doesn't depend
    on model name length; a model/part name, when known, is appended at
    the end of its own line instead. RAM has no discoverable model name
    without root (via `dmidecode`), so putting CPU's and GPU's model names
    right after their labels would make RAM's missing one look like an
    inconsistency rather than a genuine unavailable-data gap -- moving all
    model names to line-end keeps the three rows visually symmetric.

    Each line silently omits any segment its own reading doesn't have
    (CPU temperature, fan RPM, CPU model, GPU temperature) rather than
    showing a placeholder -- the same rule this module and hardware.py
    follow for every optional field.

    The label stays `omsh.muted` (a constant, not something that needs to
    draw the eye), but the bar glyph string and its percentage carry
    `_usage_style()`'s low/medium/high color, since that's the number a
    person glancing at this indicator most wants a fast reading of.
    Everything else on the line (temperature, RPM, model/part name) stays
    `omsh.muted`, a quiet-detail role.
    """
    lines: list[Text] = []

    cpu = snapshot.cpu
    cpu_style = _usage_style(cpu.usage_percent)
    cpu_line = Text()
    cpu_line.append(f"{'CPU':<{_ROW_LABEL_WIDTH}} ", style="omsh.muted")
    cpu_line.append(f"{_bar(cpu.usage_percent)} {cpu.usage_percent:.0f}%", style=cpu_style)
    if cpu.temperature_celsius is not None:
        cpu_line.append(f" · {cpu.temperature_celsius:.0f}°C", style="omsh.muted")
    if cpu.fan_rpm is not None:
        cpu_line.append(f" · {cpu.fan_rpm} RPM", style="omsh.muted")
    if cpu.model_name is not None:
        cpu_line.append(f" · {cpu.model_name}", style="omsh.muted")
    lines.append(cpu_line)

    ram = snapshot.ram
    ram_percent = (ram.used_gb / ram.total_gb * 100) if ram.total_gb else 0.0
    ram_line = Text()
    ram_line.append(f"{'RAM':<{_ROW_LABEL_WIDTH}} ", style="omsh.muted")
    ram_line.append(f"{_bar(ram_percent)} {ram_percent:.0f}%", style=_usage_style(ram_percent))
    ram_line.append(f" · {ram.used_gb}/{ram.total_gb}GB", style="omsh.muted")
    lines.append(ram_line)

    if snapshot.gpu is not None:
        gpu = snapshot.gpu
        gpu_percent = (gpu.vram_used_mb / gpu.vram_total_mb * 100) if gpu.vram_total_mb else 0.0
        gpu_line = Text()
        gpu_line.append(f"{'GPU':<{_ROW_LABEL_WIDTH}} ", style="omsh.muted")
        gpu_line.append(f"{_bar(gpu_percent)} {gpu_percent:.0f}% VRAM", style=_usage_style(gpu_percent))
        if gpu.temperature_celsius is not None:
            gpu_line.append(f" · {gpu.temperature_celsius:.0f}°C", style="omsh.muted")
        gpu_line.append(f" · {gpu.name}", style="omsh.muted")
        lines.append(gpu_line)

    return lines


def _thinking_line(
    *,
    elapsed_seconds: float,
    telemetry: ParseTelemetry | None,
    live_tokens_out: int | None = None,
    live_tokens_in: int | None = None,
) -> Text:
    """
    Top line -- elapsed time always; live in/out token counts while the
    call is still streaming (see module docstring); tok/s and model name
    only once a finished ParseTelemetry is available (both need the FINAL
    duration/model, not knowable until the call completes).

    `live_tokens_out`/`live_tokens_in` are ignored once `telemetry` is
    available -- the finished call's own numbers are that same count's
    final, authoritative value, so showing both would just repeat it.
    """
    line = Text(f"⚟ Thinking... {elapsed_seconds:.1f}s", style="omsh.muted")
    if telemetry is None:
        if live_tokens_in is not None or live_tokens_out is not None:
            in_part = f"{live_tokens_in} in" if live_tokens_in is not None else "? in"
            out_part = f"{live_tokens_out} out" if live_tokens_out is not None else "0 out"
            line.append(f" · {in_part} / {out_part}")
        return line
    if telemetry.tokens_in is not None or telemetry.tokens_out is not None:
        in_part = f"{telemetry.tokens_in} in" if telemetry.tokens_in is not None else "? in"
        out_part = f"{telemetry.tokens_out} out" if telemetry.tokens_out is not None else "? out"
        line.append(f" · {in_part} / {out_part}")
    if (
        telemetry.tokens_out is not None
        and telemetry.duration_seconds is not None
        and telemetry.duration_seconds > 0
    ):
        tokens_per_second = telemetry.tokens_out / telemetry.duration_seconds
        line.append(f" · {tokens_per_second:.0f} tok/s")
    if telemetry.model is not None:
        line.append(f" · {telemetry.model}")
    return line


def _streamed_text_line(text: str | None) -> Text | None:
    """
    The raw JSON the model is generating, live, right under the stats line
    -- the user's own explicit ask ("I want to show the full live token by
    token streaming like this conversation"), not just a growing number.
    Returns None (nothing to render) when there's no text yet, same "don't
    show an empty placeholder" rule this module already follows elsewhere.

    Truncated to the tail end of the accumulated text once it exceeds
    `_STREAM_TEXT_MAX_CHARS` -- an indicator line, not a scrollback pager;
    the tail (most recently generated content) is what's actually "live"
    right now, so that's what stays visible rather than a frozen prefix.
    """
    if not text:
        return None
    display_text = text if len(text) <= _STREAM_TEXT_MAX_CHARS else "…" + text[-(_STREAM_TEXT_MAX_CHARS - 1) :]
    # A composite style string mixing a plain attribute ("dim") with a
    # ui/theme.py "omsh.*" theme name silently renders unstyled in rich
    # (see ui/prompt.py's render_prompt for the same issue). Combining a
    # real `Style` object with the theme-resolved style directly sidesteps
    # rich's string parser for the composite case.
    style = Style(dim=True) + OMSH_THEME.styles["omsh.accent"]
    return Text(display_text, style=style)


def render_thinking_display(
    *,
    elapsed_seconds: float,
    snapshot: hardware_module.HardwareSnapshot,
    telemetry: ParseTelemetry | None = None,
    live_tokens_out: int | None = None,
    live_tokens_in: int | None = None,
    live_text: str | None = None,
) -> Group:
    """
    The full stats + hardware + streamed-text group this module shows.

    The streamed JSON text is placed below the CPU/RAM/GPU hardware rows,
    not above them, so the fixed-position system stats stay visually
    anchored at the top of the indicator and the growing/scrolling JSON
    content sits at the bottom where its variable height doesn't push the
    hardware rows around frame to frame.
    """
    renderables: list[Text] = [
        _thinking_line(
            elapsed_seconds=elapsed_seconds,
            telemetry=telemetry,
            live_tokens_out=live_tokens_out,
            live_tokens_in=live_tokens_in,
        )
    ]
    renderables.extend(render_hardware_lines(snapshot))
    # Only shown while still streaming (telemetry is None) -- once the call
    # has finished, the plan panel right below takes over as the permanent
    # record of what was produced; repeating the raw JSON here too would be
    # redundant clutter on a transient indicator that's about to disappear.
    if telemetry is None:
        streamed_line = _streamed_text_line(live_text)
        if streamed_line is not None:
            renderables.append(streamed_line)
    return Group(*renderables)


def run_with_thinking_indicator(
    fn: Callable[[], T],
    *,
    console: Console | None = None,
    hardware_reader: Callable[[], hardware_module.HardwareSnapshot] = hardware_module.read_snapshot,
    on_token_box: dict[str, object] | None = None,
) -> T:
    """
    Run `fn` (a zero-arg blocking callable) on a background thread while
    showing the live Section 8.3.3 indicator; returns `fn`'s return value,
    or re-raises whatever `fn` raised, once it finishes. The indicator is
    `transient=True` (rich.Live) -- it disappears when done rather than
    leaving a stale "Thinking..." block in the scrollback once the plan
    panel (which carries the same numbers, permanently, in its own footer)
    is about to be printed right below it.

    `hardware_reader` is injectable for tests (avoids sampling real CPU/RAM
    every test run) and for anyone wanting a cheaper/fake snapshot source.

    `on_token_box` (added post-Build-Order, for genuine live token+text
    streaming -- see module docstring): a plain dict the CALLER's own `fn`
    closure is expected to write into as chunks arrive -- in practice, `fn`
    is a `lambda: parse_intent(..., on_token=lambda progress:
    on_token_box.update(tokens_out=progress.tokens_out,
    tokens_in=progress.tokens_in, text=progress.text_so_far))` closure from
    main.py. This loop just reads `on_token_box.get(...)` once per frame
    and passes it to `render_thinking_display` -- it never writes the box
    itself, so `fn` need not be an intent_parser call at all; a caller that
    doesn't pass `on_token_box` gets exactly today's behavior (elapsed
    timer + hardware only, until a final ParseTelemetry arrives). Reading a
    handful of plain dict keys once per frame from two threads needs no
    lock here: both sides only ever set/get simple int/str values, which
    are atomic under the GIL, and a torn read (an old value shown for up to
    one frame) is invisible at 8 refreshes/second.
    """
    active_console = console if console is not None else themed_console()

    result_box: dict[str, object] = {}
    error_box: dict[str, BaseException] = {}

    def _worker() -> None:
        try:
            result_box["value"] = fn()
        except BaseException as exc:  # re-raised on the main thread below
            error_box["error"] = exc

    thread = threading.Thread(target=_worker, daemon=True)
    start = time.monotonic()
    last_hardware_read = 0.0
    snapshot = hardware_reader()

    thread.start()
    with Live(console=active_console, refresh_per_second=UI_REFRESH_PER_SECOND, transient=True) as live:
        while thread.is_alive():
            now = time.monotonic()
            if now - last_hardware_read >= HARDWARE_REFRESH_SECONDS:
                snapshot = hardware_reader()
                last_hardware_read = now
            if on_token_box is not None:
                live_tokens_out = on_token_box.get("tokens_out")
                live_tokens_in = on_token_box.get("tokens_in")
                live_text = on_token_box.get("text")
            else:
                live_tokens_out = live_tokens_in = live_text = None
            live.update(
                render_thinking_display(
                    elapsed_seconds=now - start,
                    snapshot=snapshot,
                    live_tokens_out=live_tokens_out,
                    live_tokens_in=live_tokens_in,
                    live_text=live_text,
                )
            )
            time.sleep(1 / UI_REFRESH_PER_SECOND)
    thread.join()

    if "error" in error_box:
        raise error_box["error"]
    return result_box["value"]  # type: ignore[return-value]