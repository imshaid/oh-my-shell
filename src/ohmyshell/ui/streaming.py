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

Real per-file live progress: executor.py's `run_plan(..., on_output_line=
...)` (see that module's `_run_streaming` docstring) streams a command's
stdout one line at a time, as it's produced, instead of only reporting a
result after the whole command finishes. `StreamingRenderer.on_output_line`
(below) is the `on_output_line` callback main.py wires into `run_plan()`;
it recognizes `mv -v`'s "renamed 'X' -> 'Y'" line shape to keep a running
file count and the most recent filename, and falls back to showing the
raw line verbatim for any other command's output -- either way, always
real command output, never a simulated/estimated number.

This needs no change to executor.py's Ctrl+C/SIGINT-handling contract
(reading a Popen's stdout is already incremental -- no worker thread is
needed the way ui/thinking.py needs one for parse_intent(); see
`_run_streaming`'s own docstring) and is fully additive: `on_output_line`
defaults to None, and every pre-existing (non-streaming) call site keeps
using the exact same `runner`-based blocking path as before.

`_LiveRunningLine` (below) carries both a live-ticking elapsed-time
counter AND the live label text, updated in place via `update_label()` as
each output line arrives -- `__rich_console__` reads both fresh on every
`Live` refresh tick, so neither needs an explicit re-render call.

`_LiveRunningLine` renders three lines every tick (spinner+label+elapsed
on top, an animated indeterminate bar + live count/rate underneath, the
latest raw output line at the bottom) -- still zero extra threads: `Live`'s
timer calls `__rich_console__` on this same mutable object every tick, so
a bar animated purely from `time.monotonic()` (no state to advance
between ticks, same trick `Spinner` itself uses) animates for free even
between real `update_label()`/`note_line()` calls, keeping a slow step
(few output lines) looking alive instead of frozen. The bar is
indeterminate (a moving highlighted segment, not a filled percentage)
because no command reports a total up front, so a percentage would have
to be fabricated -- an honest "in progress, working" animation instead.

The "AI: N tokens · Ns reasoning time" line: `render_execution_summary`
takes an optional `telemetry` (the same ParseTelemetry main.py threads into
the plan panel's own footer) and, when given, prints this line using the
plan's total token count (tokens_in + tokens_out) and duration_seconds as
reasoning time. Left out when telemetry is None or empty -- a raw-shell
command never went through the Intent Parser and has nothing genuine to
show here.
"""

from __future__ import annotations

import re
import time
from contextlib import contextmanager
from typing import Iterator

from rich.ansi import AnsiDecoder
from rich.console import Console, ConsoleOptions, Group, RenderResult
from rich.live import Live
from rich.spinner import Spinner
from rich.style import Style
from rich.text import Text

from ohmyshell.executor import ExecutionResult, StepEvent, StepStatus
from ohmyshell.intent_parser import ParseTelemetry
from ohmyshell.ui.theme import OMSH_THEME, themed_console

# These status glyphs are this app's own chrome, not command output, so
# they use ui/theme.py's fixed "omsh.*" style names instead of rich's
# terminal-theme-relative named colors ("green"/"red"/"yellow"), for a
# consistent look across terminals. See ui/theme.py's own module docstring.
_STATUS_GLYPH = {
    StepStatus.DONE: ("✓", "omsh.success"),
    StepStatus.FAILED: ("✗", "omsh.danger"),
    StepStatus.INTERRUPTED: ("⚠", "omsh.warning"),
    StepStatus.SKIPPED: ("⊘", "omsh.warning"),
}

_MAX_RUNNING_LABEL_CHARS = 72  # keeps the spinner line to roughly one terminal row
_MAX_DETAIL_LINE_CHARS = 78  # keeps the raw-output line to roughly one terminal row

_BAR_WIDTH = 28
_BAR_SEGMENT_WIDTH = 6  # width of the moving highlighted segment inside the bar
_BAR_CYCLE_SECONDS = 1.6  # how long one full sweep across the bar takes


def _render_activity_bar(elapsed: float) -> Text:
    """
    An animated, indeterminate progress bar -- a highlighted segment sweeps
    back and forth across `_BAR_WIDTH` cells, computed purely from
    `elapsed` (no stored/advanced state, same trick `Spinner.render(t)`
    uses -- see `_LiveRunningLine`'s docstring), so it animates on every
    `Live` refresh tick even between real output lines.

    Deliberately not a filled percentage bar: no command this executor
    runs reports a total up front, so a percentage would have to be
    fabricated. A sweeping bar is the honest "actively working" signal
    used by real tools (apt, pip) for the same reason.
    """
    cycle = _BAR_CYCLE_SECONDS * 2  # one full there-and-back sweep
    phase = (elapsed % cycle) / cycle  # 0..1 over the full sweep
    # Fold the back half of the cycle so the segment reverses direction
    # smoothly instead of jumping from the end back to the start.
    triangle = phase * 2 if phase < 0.5 else 2 - phase * 2
    max_start = _BAR_WIDTH - _BAR_SEGMENT_WIDTH
    start = round(triangle * max_start)
    bar = Text()
    bar.append("│", style="omsh.muted")
    for col in range(_BAR_WIDTH):
        if start <= col < start + _BAR_SEGMENT_WIDTH:
            bar.append("█", style="omsh.accent")
        else:
            bar.append("░", style="omsh.muted")
    bar.append("│", style="omsh.muted")
    return bar


class _LiveRunningLine:
    """
    Spinner + a genuinely live, ticking elapsed-time counter, an animated
    activity bar, a live throughput readout, and the most recent raw
    output line -- all for the RUNNING step.

    A plain `rich.spinner.Spinner` already animates on its own under a
    `Live` display -- Live's background refresh thread re-renders whatever
    renderable is currently stored every tick, and `Spinner.render(t)`
    computes its glyph frame fresh from wall-clock time each call, with no
    extra plumbing needed. This class uses that exact same mechanism for
    the elapsed-time text, the activity bar (`_render_activity_bar`,
    above) AND the mutable label/count/detail fields: `__rich_console__`
    is called by Live on every refresh tick (same as Spinner's own), and
    reads everything fresh each time -- so calling `update_label()` /
    `note_line()` between ticks (from StreamingRenderer.on_output_line, as
    real command output arrives) is picked up on the very next automatic
    repaint, with no extra `live.update()` call needed, and the bar itself
    keeps sweeping even when no new output has arrived yet. No background
    thread, no change to `on_event`'s call pattern (still called exactly
    once for RUNNING, since a plan is always one command; see executor.py's
    own module docstring), and no change to run_plan()'s blocking
    subprocess/SIGINT-handling contract: executor.py's Ctrl+C handling
    relies on `signal.signal()`, which only works on the main thread, so a
    worker thread here (the pattern ui/thinking.py uses for parse_intent)
    would risk breaking that Ctrl+C behavior. This class needs none of
    that -- it is a passive renderable, not an active poller; executor.py's
    own `_run_streaming` is what makes the updates arrive incrementally,
    on the same main thread, not this class.
    """

    def __init__(self, label: str) -> None:
        self._label = label
        self._start = time.monotonic()
        self._spinner = Spinner("dots")
        self._count: int | None = None  # None until a countable line (e.g. mv -v) arrives
        self._count_noun = "items"
        self._detail_line: str | None = None

    def update_label(self, label: str) -> None:
        if len(label) > _MAX_RUNNING_LABEL_CHARS:
            label = "…" + label[-(_MAX_RUNNING_LABEL_CHARS - 1) :]
        self._label = label

    def note_line(self, *, count: int | None, noun: str, detail: str) -> None:
        """
        Record one real unit of progress from the running command's
        output: `count` (running total, e.g. files moved so far, or None
        for a command with nothing countable), `noun` (what's being
        counted, e.g. "files"), and `detail` (the raw/derived line itself,
        shown on its own line so the actual thing that just happened --
        not just a number -- stays visible).
        """
        if count is not None:
            self._count = count
            self._count_noun = noun
        if len(detail) > _MAX_DETAIL_LINE_CHARS:
            detail = "…" + detail[-(_MAX_DETAIL_LINE_CHARS - 1) :]
        self._detail_line = detail

    def __rich_console__(self, console: Console, options: ConsoleOptions) -> RenderResult:
        elapsed = time.monotonic() - self._start
        frame = self._spinner.render(time.monotonic())
        head = Text.assemble(frame, f" {self._label}", (f"  ·  {elapsed:.1f}s", "omsh.muted"))
        yield head

        bar = _render_activity_bar(elapsed)
        stats = Text("  ")
        stats.append_text(bar)
        if self._count is not None:
            rate = self._count / elapsed if elapsed > 0.05 else 0.0
            # Composite style (bold + a theme-registered omsh.* name) built
            # as a real `Style` object rather than the string "bold
            # omsh.accent" -- a composite style string mixing a plain
            # attribute with an omsh.* theme name silently renders
            # completely unstyled in rich (see ui/prompt.py's render_prompt
            # for the same issue).
            stats.append(f"  {self._count} {self._count_noun}", style=Style(bold=True) + OMSH_THEME.styles["omsh.accent"])
            if rate >= 0.1:
                stats.append(f"  ·  {rate:.1f}/s", style="omsh.muted")
        yield stats

        if self._detail_line is not None:
            yield Text(f"    {self._detail_line}", style=Style(dim=True, italic=True) + OMSH_THEME.styles["omsh.muted"])


def render_running_line(event: StepEvent) -> _LiveRunningLine:
    """A spinner + live elapsed-time line shown while a step is RUNNING (Section 8.3.4's ⠙)."""
    label = event.detail if event.detail else "Executing..."
    return _LiveRunningLine(label)


# GNU coreutils' `mv -v` output shape (`renamed '<src>' -> '<dst>'`), one
# per file actually moved.
_MV_VERBOSE_RE = re.compile(r"^renamed '.*' -> '(?P<dest>.*)'$")


def _running_label_for_output_line(line: str, *, files_moved_so_far: int) -> tuple[str, int]:
    """
    Turn one real line of a running command's stdout into the next live
    label + updated file count. Recognizes `mv -v`'s line shape
    specifically; any other command's raw output is shown verbatim, so
    nothing here silently hides real output it doesn't understand.

    Kept for backward compatibility with existing callers/tests that want
    the pre-formatted "N files moved  ·  name" label string. New code
    (`StreamingRenderer.on_output_line`) uses `_progress_for_output_line`
    below instead, which returns the pieces (count/noun/detail)
    separately so the live renderer can lay them out across the richer
    multi-line display rather than one flat string.
    """
    label, count, _detail = _progress_for_output_line(line, files_moved_so_far=files_moved_so_far)
    return label, count


def _progress_for_output_line(line: str, *, files_moved_so_far: int) -> tuple[str, int, str]:
    """
    Parse one real line of a running command's stdout into
    (label, updated_count, detail) for `_LiveRunningLine.note_line`:
    - `label`/`count`: the same "N files moved" accounting
      `_running_label_for_output_line` has always produced.
    - `detail`: what to show on the live renderer's own detail line --
      just the destination filename for a recognized `mv -v` line (the
      count already says "moved", repeating the whole "renamed 'X' -> 'Y'"
      line would be noise), or the raw line verbatim for anything else,
      so real command output is never silently hidden.
    """
    match = _MV_VERBOSE_RE.match(line)
    if match is not None:
        files_moved_so_far += 1
        dest_name = match.group("dest").rsplit("/", 1)[-1]
        noun = "file" if files_moved_so_far == 1 else "files"
        label = f"{files_moved_so_far} {noun} moved  ·  {dest_name}"
        return label, files_moved_so_far, dest_name
    return line, files_moved_so_far, line


def render_result_line(event: StepEvent) -> Text:
    """
    One collapsed summary line for a finished step (DONE/FAILED/
    INTERRUPTED/SKIPPED), matching the "✓ Cleanup complete [12.3s]" style.
    """
    glyph, color = _STATUS_GLYPH.get(event.status, ("?", "omsh.muted"))
    text = Text(f"{glyph} ", style=color)
    if event.status is StepStatus.DONE:
        text.append("Done", style=color)
    elif event.status is StepStatus.FAILED:
        text.append("Failed", style=color)
    elif event.status is StepStatus.INTERRUPTED:
        text.append("Interrupted", style=color)
    elif event.status is StepStatus.SKIPPED:
        text.append("Skipped", style=color)
    if event.detail:
        text.append(f"  —  {event.detail.strip()}", style="omsh.muted")
    return text


def _ai_telemetry_line(telemetry: ParseTelemetry | None) -> str | None:
    """
    "AI: N tokens total · Ns reasoning time" (Section 8.3.4's mockup),
    built from the same real ParseTelemetry the plan panel's own footer
    shows. Returns None (nothing to show) when telemetry is missing or
    carries no usable fields, the same "honest partial line" rule
    ui/panels.py's own _telemetry_footer_text follows.
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


_ansi_decoder = AnsiDecoder()


def _decode_ansi_block(raw: str) -> list[Text]:
    """
    Turn a captured command output block (possibly containing real ANSI
    color escapes -- see executor.py's `_PtyPopen` docstring for why those
    survive all the way to here) into a list of styled `Text` lines, one
    per line of output.

    `rich.Text.append(raw_string)` -- what this function replaces calling
    directly -- treats ANSI escape bytes as literal text to display, not
    as styling instructions. `AnsiDecoder` is `rich`'s own tool for the
    opposite: parsing real ANSI SGR sequences into `Text` objects with
    actual `Style`s attached, which is what makes a colorized `ls`/`git`/
    etc. output actually show its colors here instead of either raw
    escape-code garbage or plain white text.

    PTY output uses `\\r\\n` line endings (a real terminal's own
    convention); normalized to `\\n` first since `AnsiDecoder.decode`
    splits on `\\n` and a stray `\\r` would otherwise leave a trailing
    carriage-return character rendered as part of the line.
    """
    return list(_ansi_decoder.decode(raw.replace("\r\n", "\n").replace("\r", "\n")))


def render_execution_summary(
    result: ExecutionResult, *, telemetry: ParseTelemetry | None = None
) -> Group:
    """
    Final collapsed summary after run_plan() returns, for all its steps.

    `telemetry` (intent_parser.ParseTelemetry, optional) adds the mockup's
    "AI: N tokens total · Ns reasoning time" line -- see this module's
    docstring and `_ai_telemetry_line`. Omitted entirely when not given (a
    raw-shell command, which never went through the Intent Parser at all,
    or a caller that hasn't been updated to pass it).

    A DONE step's actual command output (`StepResult.stdout`) is printed
    here, once, after the step glyph line, for any DONE step whose stdout
    is non-empty -- `on_output_line`'s live progress line only covers
    commands that emit recognizable per-line progress (e.g. `mv -v`) and
    disappears once the transient `Live` display closes, so a single-shot
    command like `dpkg --get-selections | wc -l` would otherwise produce a
    step description and nothing else. stderr is similarly surfaced for
    FAILED steps.

    Return type changed from `Text` to `Group` (both are valid
    `rich.console.Console.print()` arguments, so `print_summary`'s call
    site needed no change) specifically to carry the decoded-ANSI output
    lines from `_decode_ansi_block` -- those are already-styled `Text`
    objects, and `Text.append()` cannot embed one styled `Text` inside
    another the way plain string interpolation could; `Group` is `rich`'s
    own container for "print several renderables in sequence."
    """
    header = Text()
    output_blocks: list[Text] = []
    for step_result in result.step_results:
        glyph, color = _STATUS_GLYPH.get(step_result.status, ("?", "omsh.muted"))
        header.append(f"{glyph} ", style=color)
        # The description text carries the same semantic color as its own
        # glyph (success/danger/warning), so a DONE step's whole line
        # reads as "this succeeded" at a glance, not just its checkmark.
        header.append(f"Step {step_result.step_number}: {step_result.description}\n", style=color)
        if step_result.status is StepStatus.DONE and step_result.stdout.strip():
            output_blocks.extend(_decode_ansi_block(step_result.stdout.rstrip()))
        elif step_result.status is StepStatus.FAILED and step_result.stderr.strip():
            for line in _decode_ansi_block(step_result.stderr.rstrip()):
                line.stylize("omsh.danger")
                output_blocks.append(line)
    trailer = Text()
    lines = trailer
    ai_line = _ai_telemetry_line(telemetry)
    if ai_line is not None:
        # `omsh.active` (this app's own "something the AI did" accent,
        # already used for the AI-active prompt icon) makes this line read
        # as its own distinct, AI-attributed piece of chrome rather than
        # blending into the muted metadata around it.
        lines.append(f"\n{ai_line}", style="omsh.active")
    if result.interrupted:
        lines.append("\n[Ctrl+C] Stopped early — see above for what completed.", style="omsh.warning")
        # [u] Undo is offered here too, not only when result.all_done:
        # Section 8.3.4's own mockup shows "[u] Undo what was moved"
        # alongside an interrupted stop, and whatever did complete before
        # the interrupt (e.g. files already moved into .trash/) is exactly
        # as undoable as a fully-finished run's files -- the audit log
        # records status="interrupted" with the same action_id trash.py's
        # undo looks up regardless of how the run ended. "[r] Resume
        # remaining" (also in that mockup) is a separate, unimplemented
        # feature and is not offered here, so this line only ever
        # promises what actually works today.
        #
        # `omsh.accent` (this app's signature blue, already used for
        # /help's own command names) marks the "[u]" key hint as
        # actionable, matching how every other keyed hint in this app is
        # colored.
        lines.append("\n▸ [u] Undo this action", style="omsh.accent")
    elif result.aborted_for_sudo:
        lines.append("\nAborted — elevated permission was declined.", style="omsh.warning")
    elif result.all_done:
        lines.append("\n▸ [u] Undo this action", style="omsh.accent")
    return Group(header, *output_blocks, trailer)


class StreamingRenderer:
    """
    Stateful `on_event` callback for executor.run_plan(), driving a
    `rich.live.Live` display so RUNNING updates in place and the final
    status replaces it -- the "collapse to summary" behavior from Section
    8.3.4/8.3.7. Used as:

        with StreamingRenderer(console=console) as renderer:
            result = run_plan(plan, on_event=renderer.on_event)
        renderer.print_summary(result)
    """

    def __init__(self, *, console: Console | None = None) -> None:
        self._console = console if console is not None else themed_console()
        self._live: Live | None = None
        # Tracks the currently-active RUNNING line + its file count, so
        # `on_output_line` (below) can update the SAME instance already
        # being repainted by Live, and so the file counter accumulates
        # correctly across multiple lines of one command's output rather
        # than resetting per line.
        self._running_line: _LiveRunningLine | None = None
        self._files_moved: int = 0

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
            self._files_moved = 0
            self._running_line = render_running_line(event)
            self._live.update(self._running_line)
        else:
            self._running_line = None
            self._live.update(render_result_line(event))

    def on_output_line(self, line: str) -> None:
        """
        `executor.run_plan(..., on_output_line=...)` callback: called once
        per line of the running command's stdout, as it's produced. Feeds
        the currently-active RUNNING line's `note_line` (count + detail,
        rendered on their own lines below the spinner -- see
        `_LiveRunningLine`'s docstring); a no-op before RUNNING fires or
        after the display has closed, the same defensive shape `on_event`
        uses.
        """
        if self._running_line is None:
            return
        _label, self._files_moved, detail = _progress_for_output_line(
            line, files_moved_so_far=self._files_moved
        )
        noun = "file" if self._files_moved == 1 else "files"
        count = self._files_moved if _MV_VERBOSE_RE.match(line) else None
        self._running_line.note_line(count=count, noun=noun, detail=detail)

    def print_summary(self, result: ExecutionResult, *, telemetry: ParseTelemetry | None = None) -> None:
        self._console.print(render_execution_summary(result, telemetry=telemetry))

    # `Live`'s own refresh loop repaints this renderer's spinner on a timer
    # for as long as the display is active -- including while executor.py
    # is blocked inside a real `sudo <command>` subprocess call waiting for
    # a password. `sudo` reads/writes that prompt directly on the
    # controlling terminal (bypassing this process's own stdout/stderr
    # entirely), so its writes would collide with `Live`'s own redraws
    # without pausing. `pause_for_sudo`/`resume_after_sudo` are
    # executor.run_plan()'s `on_before_execute`/`on_after_execute` hooks
    # (see that function's own docstring) -- main.py passes them through
    # only for the `used_sudo=True` case, so an ordinary (non-elevated)
    # command's spinner is unaffected.
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