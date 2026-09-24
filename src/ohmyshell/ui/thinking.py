"""
Live Hardware Load Indicator + "Thinking..." display (Build Order Step 11
follow-up, Section 6 Core Feature #14 + Section 8.3.3's confirmed mockup).

Section 6, Core Feature #14 (verbatim): "**Live Hardware Load Indicator**
— শুধু AI-thinking-window-এ (CPU/RAM সবসময়, GPU tiered-detection সাপেক্ষে,
integrated GPU-তে চুপচাপ hide)". Section 8.3.3's confirmed mockup is the
base for the CPU/RAM/GPU line shape; the layout below is the user-confirmed
expansion of it (fan RPM, CPU model, GPU temperature, tokens/sec -- see
this project's own working notes, not a blueprint addition):

    ⚟ Thinking... 0.8s · 12 in / 34 out · 43 tok/s · qwen3:8b
    CPU ▓▓▓▓▓▓▓░░░ 82% · 58°C · 2100 RPM · 13th Gen i7-13650HX
    RAM ▓▓▓▓▓▓▓▓░░ 39% · 6.1/15.6GB
    GPU ▓▓▓▓░░░░░░ 39% VRAM · 61°C · RTX 4060
    {"action": "clean_temp_files", "risk": "medium", "para

One metric-group per hardware line, confirmed by the user over the plain
2-line original (see AskUserQuestion history in this project's own working
notes): "option 1, but should be aligned properly" -- so the bar column
starts at the same character position on every line regardless of how long
that line's "CPU (<model>)"/"GPU (<name>)" label is. Network and disk I/O
were explicitly asked about and explicitly declined by the user
("বাদ দেয়া হোক") -- mostly-idle numbers for a local Ollama call would just
be visual noise -- so neither appears here.

"Token count ও সময় ... live in-place update হয়" (Section 8.3.3), and the raw
generated text streaming live underneath it (added post-Build-Order, per
the user's own explicit follow-up after first seeing the token-count-only
version: "I want to show the full live token by token streaming like this
conversation, also other stats" -- i.e. not just a growing number, the
actual text the model is producing, plus live in/out counts).

Bug fix baked into this version (found via the user's own real end-to-end
run): the first cut of "live" streaming read `chunk.eval_count` straight
off each streamed chunk as if it were a running total. It isn't -- ollama's
real client types mark prompt_eval_count/eval_count/total_duration as
Optional, and in practice only the FINAL (done=True) streamed chunk carries
them; every earlier chunk has them unset. That made the old "live" count
change exactly once, right at the very end -- which is why the user's own
test run showed nothing until the already-finished plan panel appeared.
intent_parser.StreamProgress (see that module) now carries a genuinely
growing `tokens_out` (a running chunk count kept by OllamaBackend.generate
itself, not read off the server's own field) and the full `text_so_far`
accumulated raw JSON text -- this module just renders whatever the latest
StreamProgress says, every UI frame, via `on_token_box`.

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
`Thread.is_alive()` -- the same shape `ui/streaming.py`'s StreamingRenderer
already uses for a different reason (there, run_plan() calls back into
on_event() itself, so no second thread is needed; here, nothing calls back
into THIS thread, so this module supplies the thread; intent_parser's own
`on_token` callback runs ON the worker thread, writing into the shared
`on_token_box` dict that this module's main-thread loop reads).
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
    line = Text(f"⚟ Thinking... {elapsed_seconds:.1f}s", style="dim")
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
    # Bug fix (post-Build-Order, found via real-terminal testing -- see
    # ui/prompt.py's own "bold omsh.path" fix for the full root-cause
    # story): a composite style STRING mixing a plain attribute ("dim")
    # with a ui/theme.py "omsh.*" theme name silently renders completely
    # unstyled in rich, rather than raising or falling back to the color
    # alone. Combining a real `Style` object with the theme-resolved style
    # directly sidesteps rich's string parser for the composite case.
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

    Layout fix (found via the user's own real end-to-end run, screenshot
    attached): the raw JSON stream was originally placed directly under the
    stats line, ABOVE the CPU/RAM/GPU hardware rows -- the user explicitly
    asked for it the other way around, streamed text BELOW the hardware
    stats, so the fixed-position system stats stay visually anchored at the
    top of the indicator and the growing/scrolling JSON content sits at the
    bottom where its variable height doesn't push the hardware rows around
    frame to frame.
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