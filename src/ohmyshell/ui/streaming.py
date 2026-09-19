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
opaque shell command run via subprocess, not a Python loop this codebase
controls step-by-step). Rendering a real file-count/percentage progress bar
would require either parsing command output live (fragile, command-
specific) or the command_template itself emitting structured progress
(not in any capability's definition) -- neither exists yet. So this module
renders what executor.py actually emits: a single spinner while RUNNING,
then a collapsed one-line summary on DONE/FAILED/INTERRUPTED/SKIPPED. The
richer bar-with-file-count mockup above is left as the target shape for
once a capability's execution model can report granular progress -- this
renderer's structure (spinner -> collapse) is already the right shape to
extend, it just has only one "file" (the whole command) to show progress
for today.

The "AI: N tokens · Ns reasoning time" line in the blueprint's summary
mockup is not rendered here either -- no token/timing telemetry is
threaded through intent_parser.py's ParseResult today; a future step would
need to add that before this line could be shown truthfully rather than
faked.
"""

from __future__ import annotations

from contextlib import contextmanager
from typing import Iterator

from rich.console import Console
from rich.live import Live
from rich.spinner import Spinner
from rich.text import Text

from ohmyshell.executor import ExecutionResult, StepEvent, StepStatus

_STATUS_GLYPH = {
    StepStatus.DONE: ("✓", "green"),
    StepStatus.FAILED: ("✗", "red"),
    StepStatus.INTERRUPTED: ("⚠", "yellow"),
    StepStatus.SKIPPED: ("⊘", "yellow"),
}


def render_running_line(event: StepEvent) -> Spinner:
    """A single spinner line shown while a step is RUNNING (Section 8.3.4's ⠙)."""
    label = event.detail if event.detail else "Executing..."
    return Spinner("dots", text=Text(label))


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


def render_execution_summary(result: ExecutionResult) -> Text:
    """Final collapsed summary after run_plan() returns, for all its steps."""
    lines = Text()
    for step_result in result.step_results:
        glyph, color = _STATUS_GLYPH.get(step_result.status, ("?", "white"))
        lines.append(f"{glyph} ", style=color)
        lines.append(f"Step {step_result.step_number}: {step_result.description}\n")
    if result.interrupted:
        lines.append("\n[Ctrl+C] Stopped early — see above for what completed.", style="yellow")
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

    def print_summary(self, result: ExecutionResult) -> None:
        self._console.print(render_execution_summary(result))

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