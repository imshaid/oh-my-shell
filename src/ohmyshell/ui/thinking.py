"""
Live Hardware Load Indicator + "Thinking..." display (Build Order Step 11
follow-up, Section 6 Core Feature #14 + Section 8.3.3's confirmed mockup).

Section 6, Core Feature #14 (verbatim): "**Live Hardware Load Indicator**
— শুধু AI-thinking-window-এ (CPU/RAM সবসময়, GPU tiered-detection সাপেক্ষে,
integrated GPU-তে চুপচাপ hide)". Section 8.3.3's confirmed mockup is the
base for the CPU/RAM/GPU line shape; the layout below is the user-confirmed
expansion of it (fan RPM, CPU model, GPU temperature, tokens/sec -- see
this project's own working notes, not a blueprint addition):

    ⚟ Thinking... 0.8s · 94 tok/s · qwen3:8b
    CPU ▓▓▓▓▓▓▓░░░ 82% · 58°C · 2100 RPM · 13th Gen i7-13650HX
    RAM ▓▓▓▓▓▓▓▓░░ 39% · 6.1/15.6GB
    GPU ▓▓▓▓░░░░░░ 39% VRAM · 61°C · RTX 4060

Four lines, one metric-group per line, confirmed by the user over the
plain 2-line original (see AskUserQuestion history in this project's own
working notes): "option 1, but should be aligned properly" -- so the bar
column starts at the same character position on every line regardless of
how long that line's "CPU (<model>)"/"GPU (<name>)" label is. Network and
disk I/O were explicitly asked about and explicitly declined by the user
("বাদ দেয়া হোক") -- mostly-idle numbers for a local Ollama call would just
be visual noise -- so neither appears here.

"Token count ও সময় ... live in-place update হয়" (Section 8.3.3) -- a
genuine token-by-token LIVE count needs intent_parser.py's Ollama call to
use stream=True (currently a single blocking ollama.chat() call -- see
intent_parser.py's own docstring/ParseTelemetry for the non-streaming
telemetry this project reads instead). That is a distinct, larger,
separately-tracked piece of work (real token-by-token streaming, not yet
started). What this module gives live, truthfully, without that: elapsed
time (ticks in place every frame) and a live-refreshing hardware bar --
both real per-second measurements, not simulated. Tokens/sec and model
name only appear once the backend call has actually finished and a
ParseTelemetry is available (see render_thinking_display's `telemetry`
param) -- before that, the top line shows only the elapsed timer, exactly
as before, rather than a fake or zeroed throughput figure.

--- Why a background thread ---
parse_intent() is a single blocking call (no progress callback this
module could hook into to update a Live display from inside it). The only
way to show a live-updating indicator alongside a call that doesn't itself
report progress is to run that call on its own thread while the main
thread drives a `rich.live.Live` refresh loop, polling `Thread.is_alive()`
-- the same shape `ui/streaming.py`'s StreamingRenderer already uses for a
different reason (there, run_plan() calls back into on_event() itself, so
no second thread is needed; here, nothing calls back, so this module
supplies the thread). `run_with_thinking_indicator()` is the whole
public surface: it runs any zero-arg callable (in practice, a
`lambda: parse_intent(...)` closure from main.py) on a worker thread,
shows the indicator while it runs, and returns the callable's return value
(or re-raises its exception) once the thread finishes -- callers don't
need to know a thread was involved at all.

--- Hardware refresh cadence ---
hardware.read_cpu()'s own psutil.cpu_percent(interval=0.1) call blocks for
100ms to produce a real (non-instantaneous, non-zero-on-first-call)
reading -- sampling it every UI-refresh tick (8/sec, matching
StreamingRenderer's own rate) would spend most of the indicator's time
just measuring CPU rather than showing anything. Hardware is instead
sampled on its own slower interval (HARDWARE_REFRESH_SECONDS, from the
main polling loop, not a third thread) while the elapsed-time line still
ticks every UI frame -- the elapsed-time number stays perfectly live, and
the hardware bars update a few times a second rather than once per whole
"Thinking..." call, still fast enough to read as "live" for the multi-
second durations this indicator is actually shown for.

--- GPU: "tiered-detection সাপেক্ষে, integrated GPU-তে চুপচাপ hide" ---
Handled the same way hardware.py's own read_gpu()/render_system_line()
already handle it: when hardware.read_gpu() returns None (no discrete
NVIDIA GPU detected -- see hardware.py's own docstring for why only
NVIDIA/nvidia-smi is implemented), the whole GPU line is simply omitted,
silently, exactly like a None temperature/fan/model reading omits just
its own segment rather than showing a placeholder.
"""

from __future__ import annotations

import threading
import time
from typing import Callable, TypeVar

from rich.console import Console, Group
from rich.live import Live
from rich.text import Text

from ohmyshell import hardware as hardware_module
from ohmyshell.intent_parser import ParseTelemetry

T = TypeVar("T")

UI_REFRESH_PER_SECOND = 8
HARDWARE_REFRESH_SECONDS = 0.8
_BAR_WIDTH = 10


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


_ROW_LABEL_WIDTH = len("CPU")  # "CPU"/"RAM"/"GPU" are always this length


def render_hardware_lines(snapshot: hardware_module.HardwareSnapshot) -> list[Text]:
    """
    The three (or two, GPU-absent) per-metric lines, their bar columns
    aligned to a common start position -- the user's explicit "should be
    aligned properly" requirement. Labels are always the bare "CPU"/"RAM"/
    "GPU" (never "CPU (<model>)"), so alignment no longer depends on model
    name length at all; a model/part name, when known, is appended at the
    END of its own line instead (the user's explicit correction: RAM has
    no discoverable model name without root, via `dmidecode`, so putting
    CPU's and GPU's model names right after their labels made RAM's
    missing one look like an inconsistency rather than a genuine
    unavailable-data gap -- moving all model names to line-end, after the
    other real-time readings, keeps the three rows visually symmetric and
    makes RAM's simply being the one row that never grows a trailing name
    obviously a data-availability difference, not a formatting one).

    Each line silently omits any segment its own reading doesn't have
    (CPU temperature, fan RPM, CPU model, GPU temperature) rather than
    showing a placeholder -- the same rule this module and hardware.py
    have followed for every optional field from the start.
    """
    lines: list[Text] = []

    cpu = snapshot.cpu
    cpu_line = Text(style="dim")
    cpu_line.append(f"{'CPU':<{_ROW_LABEL_WIDTH}} {_bar(cpu.usage_percent)} {cpu.usage_percent:.0f}%")
    if cpu.temperature_celsius is not None:
        cpu_line.append(f" · {cpu.temperature_celsius:.0f}°C")
    if cpu.fan_rpm is not None:
        cpu_line.append(f" · {cpu.fan_rpm} RPM")
    if cpu.model_name is not None:
        cpu_line.append(f" · {cpu.model_name}")
    lines.append(cpu_line)

    ram = snapshot.ram
    ram_percent = (ram.used_gb / ram.total_gb * 100) if ram.total_gb else 0.0
    ram_line = Text(style="dim")
    ram_line.append(f"{'RAM':<{_ROW_LABEL_WIDTH}} {_bar(ram_percent)} {ram_percent:.0f}% · {ram.used_gb}/{ram.total_gb}GB")
    lines.append(ram_line)

    if snapshot.gpu is not None:
        gpu = snapshot.gpu
        gpu_percent = (gpu.vram_used_mb / gpu.vram_total_mb * 100) if gpu.vram_total_mb else 0.0
        gpu_line = Text(style="dim")
        gpu_line.append(f"{'GPU':<{_ROW_LABEL_WIDTH}} {_bar(gpu_percent)} {gpu_percent:.0f}% VRAM")
        if gpu.temperature_celsius is not None:
            gpu_line.append(f" · {gpu.temperature_celsius:.0f}°C")
        gpu_line.append(f" · {gpu.name}")
        lines.append(gpu_line)

    return lines


def _thinking_line(*, elapsed_seconds: float, telemetry: ParseTelemetry | None) -> Text:
    """
    Top line -- elapsed time always; tokens/sec and model name only once a
    finished ParseTelemetry is available (there is nothing genuinely live
    to show for those before the call returns -- see module docstring).
    """
    line = Text(f"⚟ Thinking... {elapsed_seconds:.1f}s", style="dim")
    if telemetry is None:
        return line
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


def render_thinking_display(
    *,
    elapsed_seconds: float,
    snapshot: hardware_module.HardwareSnapshot,
    telemetry: ParseTelemetry | None = None,
) -> Group:
    """The full 4-line (or 3-line, GPU-absent) group this module shows."""
    return Group(
        _thinking_line(elapsed_seconds=elapsed_seconds, telemetry=telemetry),
        *render_hardware_lines(snapshot),
    )


def run_with_thinking_indicator(
    fn: Callable[[], T],
    *,
    console: Console | None = None,
    hardware_reader: Callable[[], hardware_module.HardwareSnapshot] = hardware_module.read_snapshot,
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
    """
    active_console = console if console is not None else Console()

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
            live.update(render_thinking_display(elapsed_seconds=now - start, snapshot=snapshot))
            time.sleep(1 / UI_REFRESH_PER_SECOND)
    thread.join()

    if "error" in error_box:
        raise error_box["error"]
    return result_box["value"]  # type: ignore[return-value]