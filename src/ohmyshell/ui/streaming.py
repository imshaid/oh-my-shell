"""
rich-rendered live execution progress (Build Order Step 11).

Turns executor.py's StepEvent stream (Step 10) into the blueprint's live
progress display (Section 8.3.4's confirmed mockup):

    ▸ Executing (3 steps)

    ✓ Scanned /tmp and ~/.cache                    [0.4s]
    ⠙ Moving files to .trash/...
      ████████████████░░░░░░░░░░  178/340 files  ·  0.64 GB/1.2 GB  ·  52%
      /tmp/npm-install-8821/

    [Ctrl+C] Abort

...collapsing to a summary on completion:

    ✓ Cleanup complete                              [12.3s]

      340 files removed  ·  1.2 GB reclaimed  ·  moved to .trash/
      AI: 156 tokens total · 1.2s reasoning time

    ▸ [v] View detailed log     [u] Undo this action

Scope note (Section 16 Rule 5): executor.py's current StepEvent only
carries {step_number, total_steps, status, detail} -- it has no live
file-count/byte-size/percentage fields, because none of the four registered
capabilities' command_templates report granular progress (they're each one
opaque shell command run via subprocess.run(), blocking, not a Python loop
this codebase controls step-by-step). A real per-file bar (the richer
mockup above) would need the command_template itself to emit structured
progress AND executor.py to switch from a single blocking subprocess.run()
call to Popen + live line-reading -- a genuine architecture change to a
module whose current design (blocking subprocess, `runner` injected as a
plain callable) is itself a "confirmed with the user" decision (see
executor.py's own module docstring, decision 1-3) with broad existing test
coverage built on that exact contract. Not attempted here without an
explicit go-ahead, to avoid quietly destabilizing a heavily-tested,
already-fixed-many-times module. What IS implemented here (post-Build-
Order, per the user's explicit "make the whole shell feel alive, not just
the AI thinking step" request): a genuinely live-TICKING elapsed-time
counter on the RUNNING spinner line, via `_LiveRunningLine` below -- see
its own docstring for why this needed no threading or executor.py changes
at all, and is exactly as safe as the plain static spinner it replaces.

The "AI: N tokens · Ns reasoning time" line (added post-Build-Order, found
via manual end-to-end testing): intent_parser.OllamaBackend already reads
real prompt_eval_count/eval_count/total_duration off every ollama.chat()
response into ParseTelemetry -- that data existed all along, it just never
reached this renderer. `render_execution_summary` now takes an optional
`telemetry` (the same ParseTelemetry main.py already threads into the plan
panel's own footer) and, when given, prints this line using the plan's
total token count (tokens_in + tokens_out, matching "N tokens total" in the
mockup, as distinct from the plan panel's separate in/out breakdown) and
duration_seconds as reasoning time. Left out (not a fake "0 tokens" line)
when telemetry is None or empty -- a raw-shell command, for instance, never
went through the Intent Parser at all and has nothing genuine to show here.
"""

from __future__ import annotations

import time
from contextlib import contextmanager
from typing import Iterator

from rich.console import Console, ConsoleOptions, RenderResult
from rich.live import Live
from rich.spinner import Spinner
from rich.text import Text

from ohmyshell.executor import ExecutionResult, StepEvent, StepStatus
from ohmyshell.intent_parser import ParseTelemetry

_STATUS_GLYPH = {
    StepStatus.DONE: ("✓", "green"),
    StepStatus.FAILED: ("✗", "red"),
    StepStatus.INTERRUPTED: ("⚠", "yellow"),
    StepStatus.SKIPPED: ("⊘", "yellow"),
}


class _LiveRunningLine:
    """
    Spinner + a genuinely live, ticking elapsed-time counter for the
    RUNNING step (added post-Build-Order, per the user's explicit "make
    the whole shell feel alive" request -- see this module's docstring for
    why real per-file progress isn't attempted here).

    A plain `rich.spinner.Spinner` already animates on its own under a
    `Live` display -- Live's background refresh thread re-renders whatever
    renderable is currently stored every tick, and `Spinner.render(t)`
    computes its glyph frame fresh from wall-clock time each call, with no
    extra plumbing needed. This class uses that exact same mechanism for
    the elapsed-time text: `__rich_console__` is called by Live on every
    refresh tick (same as Spinner's own), and computes `time.monotonic() -
    self._start` fresh each time, so the "12.4s" text ticks in place
    exactly like the spinner glyph next to it -- no background thread, no
    change to `on_event`'s call pattern (still called exactly once for
    RUNNING, since a plan is currently always one command; see executor.py's
    own module docstring), and critically no change to run_plan()'s
    blocking subprocess/SIGINT-handling contract: executor.py's Ctrl+C
    handling relies on `signal.signal()`, which only works on the main
    thread, so anything that would need a worker thread here (the pattern
    ui/thinking.py uses for parse_intent) would risk breaking the
    already-fixed Ctrl+C behavior (this session's own Bug #8/#9). This
    class needs none of that -- it is a passive renderable, not an active
    poller.
    """

    def __init__(self, label: str) -> None:
        self._label = label
        self._start = time.monotonic()
        self._spinner = Spinner("dots")

    def __rich_console__(self, console: Console, options: ConsoleOptions) -> RenderResult:
        elapsed = time.monotonic() - self._start
        frame = self._spinner.render(time.monotonic())
        yield Text.assemble(frame, f" {self._label}", (f"  ·  {elapsed:.1f}s", "dim"))


def render_running_line(event: StepEvent) -> _LiveRunningLine:
    """A spinner + live elapsed-time line shown while a step is RUNNING (Section 8.3.4's ⠙)."""
    label = event.detail if event.detail else "Executing..."
    return _LiveRunningLine(label)


def render_result_line(event: StepEvent) -> Text:
    """
    One collapsed summary line for a finished step (DONE/FAILED/
    INTERRUPTED/SKIPPED), matching the "✓ Cleanup complete [12.3s]" style.
    """
    glyph, color = _STATUS_GLYPH.get(event.status, ("?", "white"))
    text = Text(f"{glyph} ", style=color)
    if event.status is StepStatus.DONE:
        text.append("Done")
    elif event.status is StepStatus.FAILED:
        text.append("Failed")
    elif event.status is StepStatus.INTERRUPTED:
        text.append("Interrupted")
    elif event.status is StepStatus.SKIPPED:
        text.append("Skipped")
    if event.detail:
        text.append(f"  —  {event.detail.strip()}")
    return text


def _ai_telemetry_line(telemetry: ParseTelemetry | None) -> str | None:
    """
    "AI: N tokens total · Ns reasoning time" (Section 8.3.4's mockup),
    built from the same real ParseTelemetry the plan panel's own footer
    already shows -- see this module's docstring for why this used to be
    omitted. Returns None (nothing to show) when telemetry is missing or
    carries no usable fields, same "honest partial line" rule ui/panels.py's
    own _telemetry_footer_text already follows.
    """
    if telemetry is None:
        return None
    parts: list[str] = []
    if telemetry.tokens_in is not None or telemetry.tokens_out is not None:
        total_tokens = (telemetry.tokens_in or 0) + (telemetry.tokens_out or 0)
        parts.append(f"{total_tokens} tokens total")
    if telemetry.duration_seconds is not None:
        parts.append(f"{telemetry.duration_seconds:.1f}s reasoning time")
    if not parts:
        return None
    return f"AI: {' · '.join(parts)}"


def render_execution_summary(result: ExecutionResult, *, telemetry: ParseTelemetry | None = None) -> Text:
    """
    Final collapsed summary after run_plan() returns, for all its steps.

    `telemetry` (intent_parser.ParseTelemetry, optional) adds the mockup's
    "AI: N tokens total · Ns reasoning time" line -- see this module's
    docstring and `_ai_telemetry_line`. Omitted entirely when not given (a
    raw-shell command, which never went through the Intent Parser at all,
    or a caller that hasn't been updated to pass it).
    """
    lines = Text()
    for step_result in result.step_results:
        glyph, color = _STATUS_GLYPH.get(step_result.status, ("?", "white"))
        lines.append(f"{glyph} ", style=color)
        lines.append(f"Step {step_result.step_number}: {step_result.description}\n")
    ai_line = _ai_telemetry_line(telemetry)
    if ai_line is not None:
        lines.append(f"\n{ai_line}", style="dim")
    if result.interrupted:
        lines.append("\n[Ctrl+C] Stopped early — see above for what completed.", style="yellow")
        # Bug fix (found via manual end-to-end testing, in a real terminal
        # session): [u] Undo used to be offered only when result.all_done,
        # so a single-Ctrl+C graceful stop showed no undo option at all --
        # but Section 8.3.4's own mockup shows "[u] Undo what was moved"
        # right alongside an interrupted stop, and whatever DID complete
        # before the interrupt (e.g. files already moved into .trash/) is
        # exactly as undoable as a fully-finished run's files -- the audit
        # log already records status="interrupted" with the same action_id
        # trash.py's undo looks up regardless of how the run ended. This is
        # distinct from "[r] Resume remaining" (also in that same mockup),
        # which is a genuinely unimplemented, separate feature (resuming a
        # partially-completed plan) -- not offered here, so this line only
        # ever promises what actually works today.
        lines.append("\n▸ [u] Undo this action", style="dim")
    elif result.aborted_for_sudo:
        lines.append("\nAborted — elevated permission was declined.", style="yellow")
    elif result.all_done:
        lines.append("\n▸ [u] Undo this action", style="dim")
    return lines


class StreamingRenderer:
    """
    Stateful `on_event` callback for executor.run_plan(), driving a
    `rich.live.Live` display so RUNNING updates in place and the final
    status replaces it -- the "collapse to summary" behavior from Section
    8.3.4/8.3.7. Used as:

        with StreamingRenderer(console=console) as renderer:
            result = run_plan(plan, registry, on_event=renderer.on_event)
        renderer.print_summary(result)
    """

    def __init__(self, *, console: Console | None = None) -> None:
        self._console = console if console is not None else Console()
        self._live: Live | None = None

    def __enter__(self) -> "StreamingRenderer":
        self._live = Live(console=self._console, refresh_per_second=8, transient=True)
        self._live.__enter__()
        return self

    def __exit__(self, exc_type, exc, tb) -> None:
        if self._live is not None:
            self._live.__exit__(exc_type, exc, tb)
            self._live = None

    def on_event(self, event: StepEvent) -> None:
        if self._live is None:
            return
        if event.status is StepStatus.RUNNING:
            self._live.update(render_running_line(event))
        else:
            self._live.update(render_result_line(event))

    def print_summary(self, result: ExecutionResult, *, telemetry: ParseTelemetry | None = None) -> None:
        self._console.print(render_execution_summary(result, telemetry=telemetry))

    # --- Sudo password-prompt garbling bug fix (post-Build-Order) ---
    # `Live`'s own refresh loop repaints this renderer's spinner on a timer
    # for as long as the display is active -- including while executor.py
    # is blocked inside a real `sudo <command>` subprocess call waiting for
    # a password. `sudo` reads/writes that prompt directly on the
    # controlling terminal (bypassing this process's own stdout/stderr
    # entirely), so its writes and `Live`'s own redraws were colliding on
    # real-terminal testing: the prompt appeared garbled/overlapping and
    # typed characters didn't reliably register, needing several Enter
    # presses before a password attempt "took". `pause_for_sudo`/`resume`
    # are executor.run_plan()'s `on_before_execute`/`on_after_execute`
    # hooks (see that function's own docstring) -- main.py passes them
    # through only for the `used_sudo=True` case, so an ordinary
    # (non-elevated) command's spinner is completely unaffected.
    def pause_for_sudo(self, used_sudo: bool) -> None:
        if used_sudo and self._live is not None:
            self._live.stop()

    def resume_after_sudo(self, used_sudo: bool) -> None:
        if used_sudo and self._live is not None:
            self._live.start()


@contextmanager
def streaming(*, console: Console | None = None) -> Iterator[StreamingRenderer]:
    """Convenience context manager wrapping StreamingRenderer's enter/exit."""
    renderer = StreamingRenderer(console=console)
    with renderer:
        yield renderer