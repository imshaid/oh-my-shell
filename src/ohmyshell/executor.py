"""
Streaming Executor (Build Order Step 10, half of "executor.py + trash.py").

Runs a Plan (plan_generator.py, Step 7), blocking, emitting a structured
event stream -- Section 8.3.4's "Execution — Live Streaming ও Interrupt
Handling" and the Module 2<->Module 4 event-format contract (Section 4.2):

    {"step": 2, "status": "running"|"done"|"failed"|"interrupted",
     "progress": {"done": 178, "total": 340}}

Step 11 (ui/streaming.py) turns these events into the blueprint's
`rich`-rendered progress bars; this module only emits StepEvent objects
(same split used by plan_generator.py/discussion.py/sudo_layer.py: plain
data + a plain-text default renderer here, `rich` panels later).

Scope note: capabilities.json defines one `command_template` per capability
action, not one per `plan_steps` entry -- `plan_steps` is display text for
the Confirmation/Discussion Loop (Step 7), while the *executable* unit is
the single rendered command_template. So run_plan() executes one shell
command per plan and reports it as one StepEvent/StepResult (step 1 of 1),
covering all of the plan's human-readable steps together. A future
capability with a genuinely multi-command execution would need its own
handling; none of the four registered capabilities need that yet.

--- Design decisions (Section 16 Rule 5 -- confirmed with the user) ---

1. Template parameter substitution & subprocess invocation: command_template
   strings are themselves shell syntax (pipes, `-exec`, `&&`, globs -- see
   capabilities.json), so they run via `subprocess.run(cmd, shell=True)`.
   To avoid shell-injection through parameter VALUES (as opposed to the
   template's own trusted shell syntax), every param value is escaped with
   `shlex.quote()` before substitution. A list-type param (e.g.
   `paths: ["/tmp", "~/.cache"]`) has each item quoted individually and the
   results space-joined, so `{paths}` expands to e.g. `/tmp ~/.cache` --
   safe to place directly into `find {paths} ...`.

   Correction (post-Build-Order, found via manual end-to-end testing): a
   value quoted as-is with `shlex.quote()` alone -- e.g. "~/.cache" ->
   "'~/.cache'" -- defeats shell tilde expansion, since a POSIX shell never
   expands `~` inside single quotes; the rendered command silently failed
   to find "'~/.cache'" as a literal directory name. `_quote_param()` (see
   `_expand_and_quote()`'s own docstring for the full story) now expands a
   leading `~`/`~/` to an absolute path in Python first, so the value has
   no `~` left by the time it's quoted -- the example above now correctly
   renders as `/tmp ~/.cache` (already expanded, so still safe once quoted
   as an ordinary absolute path).

2. Sudo-escalation trigger: capabilities.json has no static "requires_sudo"
   flag (none of the four registered capabilities need one). Detection is
   reactive: a step runs normally first; if it fails with a shell
   "Permission denied" / "Operation not permitted" signature in stderr, the
   executor treats that as a permission-escalation case and calls
   sudo_layer.decide_step() before deciding whether to retry the same
   command prefixed with `sudo`. This works for any capability without new
   registry fields, at the cost of one wasted attempt on a step that turns
   out to need sudo -- an acceptable trade for a solo/CEP-scope build,
   documented here per Rule 5.

3. Timeouts/exit codes/error handling (the blueprint is silent on all
   three): a non-zero exit for any reason OTHER than the permission-denied
   case above is reported as "failed" (matching the event-format's status
   enum); since a plan is currently always one command, "stopping the rest
   of the plan" on failure is automatic. No process timeout is enforced
   (nothing in the blueprint calls for one, and an artificial timeout sized
   to the user's own data would be an arbitrary guess); Ctrl+C is the way
   to stop a stuck step.

Audit-log tie-in: audit_log.py doesn't exist yet (Step 12). run_plan()
returns a complete ExecutionResult (step outcomes, interrupted flag, sudo
usage) so whatever calls it -- main.py today, the Audit Log once Step 12
lands -- has everything needed to log it; this module does not import
audit_log.py itself, keeping Step 10 independent of a step that comes after
it in the Build Order.
"""

from __future__ import annotations

import os
import shlex
import signal
import subprocess
from contextlib import contextmanager
from dataclasses import dataclass
from enum import Enum, auto
from typing import Any, Callable

from ohmyshell.plan_generator import Plan
from ohmyshell.sudo_layer import ElevatedStep, SudoDecision, SudoPrompt, decide_step

# Bare stderr signatures for "this failed because of a permission problem",
# checked case-insensitively. Not exhaustive of every possible OS message,
# but covers the common `sh`/`bash`/coreutils wording this project's own
# command_templates would realistically hit.
_PERMISSION_DENIED_SIGNATURES = ("permission denied", "operation not permitted")


class StepStatus(Enum):
    RUNNING = auto()
    DONE = auto()
    FAILED = auto()
    INTERRUPTED = auto()
    SKIPPED = auto()


@dataclass(frozen=True)
class StepEvent:
    """One structured execution event (Section 4.2's event-format contract)."""

    step_number: int
    total_steps: int
    status: StepStatus
    detail: str = ""


@dataclass(frozen=True)
class StepResult:
    step_number: int
    description: str
    status: StepStatus
    returncode: int | None = None
    stdout: str = ""
    stderr: str = ""
    used_sudo: bool = False


@dataclass(frozen=True)
class ExecutionResult:
    """What run_plan() returns -- everything a caller/audit-log needs."""

    action: str
    step_results: list[StepResult]
    interrupted: bool = False
    aborted_for_sudo: bool = False

    @property
    def all_done(self) -> bool:
        return (
            not self.interrupted
            and not self.aborted_for_sudo
            and all(r.status is StepStatus.DONE for r in self.step_results)
        )


class _DoubleInterrupt(Exception):
    """Raised internally when a second Ctrl+C arrives during a running step."""


def _expand_and_quote(item: Any) -> str:
    """
    shlex.quote() a single scalar param value, expanding a leading `~`/`~/`
    to the real home directory first.

    Bug fix (found via manual end-to-end testing, post-Build-Order):
    capabilities.json's own clean_temp_files default is
    `paths: ["/tmp", "~/.cache"]` (Section 5.4), and command_template
    renders it straight into `find {paths} ...`. shlex.quote("~/.cache")
    returns "'~/.cache'" (wrapped in real single quotes) -- correct
    shell-injection-safe escaping in general, but inside single quotes a
    POSIX shell never performs tilde expansion, so the rendered command
    became `find /tmp '~/.cache' ...`, which `find` reports as
    "No such file or directory" for a literal directory named "~/.cache"
    in the current working directory (verified directly: `find '~/.cache'
    -maxdepth 0` fails, while the unquoted `find ~/.cache -maxdepth 0`
    correctly resolves to $HOME/.cache). Every capability using this
    codebase's only home-relative default silently only ever cleaned
    /tmp; the ~/.cache half of the request quietly no-op'd.

    The fix expands `~`/`~/...` to an absolute path in Python (via
    os.path.expanduser, which never touches the shell) BEFORE quoting --
    shlex.quote() on an already-absolute path is a no-op for the
    resulting shell safety guarantee, but now there is no leading `~`
    left for the shell to fail to expand. Values with no leading `~` are
    unaffected (os.path.expanduser is a no-op for them), so every other
    existing param (a plain path, a PID, a process-name filter, a signal
    name) quotes exactly as before.
    """
    text = str(item)
    if text == "~" or text.startswith("~/"):
        text = os.path.expanduser(text)
    return shlex.quote(text)


def _quote_param(value: Any) -> str:
    """_expand_and_quote() a single param value, or space-join quoted list items."""
    if isinstance(value, (list, tuple)):
        return " ".join(_expand_and_quote(item) for item in value)
    return _expand_and_quote(value)


def render_command(command_template: str, params: dict[str, Any]) -> str:
    """
    Fill a capability's command_template with params, shell-escaping every
    value with shlex.quote() first (see module docstring, decision 1).
    """
    quoted_params = {key: _quote_param(value) for key, value in params.items()}
    return command_template.format(**quoted_params)


def _looks_like_permission_denied(stderr: str) -> bool:
    lowered = stderr.lower()
    return any(sig in lowered for sig in _PERMISSION_DENIED_SIGNATURES)


class InterruptState:
    """
    Tracks Ctrl+C presses during one run_plan() call (Section 8.3.4):
    first SIGINT -> graceful stop (let the current subprocess finish, then
    report interrupted); second SIGINT -> force-stop (kill immediately).
    """

    def __init__(self) -> None:
        self.interrupt_count = 0

    def reset(self) -> None:
        self.interrupt_count = 0

    def note_interrupt(self) -> None:
        self.interrupt_count += 1
        if self.interrupt_count >= 2:
            raise _DoubleInterrupt()


@contextmanager
def _sigint_handler(state: InterruptState):
    """Install a SIGINT handler for the duration of one subprocess call."""

    def _handler(signum, frame):  # noqa: ARG001 - required signal handler signature
        state.note_interrupt()

    previous = signal.signal(signal.SIGINT, _handler)
    try:
        yield
    finally:
        signal.signal(signal.SIGINT, previous)


def _run_one_command(command: str, *, runner: Callable[..., Any]) -> subprocess.CompletedProcess:
    return runner(command, shell=True, capture_output=True, text=True)


def _run_streaming(
    command: str,
    *,
    state: InterruptState,
    on_output_line: Callable[[str], None],
    popen_factory: Callable[..., subprocess.Popen],
) -> subprocess.CompletedProcess:
    """
    Real per-line live progress (added post-Build-Order, per the user's
    explicit "make the whole shell feel alive, every operation" request).

    Runs `command` via `popen_factory` (default `subprocess.Popen`) instead
    of the plain `runner` callable, reading its stdout ONE LINE AT A TIME
    as the child process produces it (e.g. `mv -v`'s "renamed 'X' -> 'Y'"
    per file, added to capabilities.json's clean_temp_files/organize_files
    templates specifically so there's something genuine to stream) and
    calling `on_output_line` for each -- this is what lets the UI layer
    (ui/streaming.py's StreamingRenderer) show a real, growing file count
    instead of a spinner that never changes for the whole command's
    duration.

    Still entirely on the calling (main) thread, by design: unlike
    ui/thinking.py's parse_intent streaming (which needed a worker thread
    because parse_intent() itself never yields control until it returns),
    reading a Popen's stdout is already incremental -- `for line in
    proc.stdout` returns each line as soon as it's flushed, without
    blocking for the whole command. So Ctrl+C handling is completely
    unaffected: `_sigint_handler`'s `signal.signal()` still runs on the
    main thread exactly as it does for the non-streaming `runner` path
    (see `_StepRunner._execute`), and a raised `_DoubleInterrupt` simply
    propagates out of the `for line in proc.stdout` loop the same way it
    already propagates out of a blocking `subprocess.run()` call.

    Returns a `subprocess.CompletedProcess` built from what was actually
    observed, so every downstream consumer (`_StepRunner._result`,
    `_looks_like_permission_denied`, etc.) sees the exact same shape it
    already handles from the non-streaming path -- this function is a
    drop-in alternative source of that one object, not a new contract.
    """
    proc = popen_factory(
        command, shell=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, bufsize=1
    )
    collected_lines: list[str] = []
    try:
        assert proc.stdout is not None  # guaranteed by stdout=PIPE above
        for raw_line in proc.stdout:
            collected_lines.append(raw_line)
            stripped = raw_line.rstrip("\n")
            if stripped:
                on_output_line(stripped)
    except BaseException:
        # Covers _DoubleInterrupt (raised mid-iteration by the SIGINT
        # handler) and any other failure while reading -- don't leave an
        # orphaned child process behind either way.
        proc.kill()
        raise
    proc.wait()
    stderr_text = proc.stderr.read() if proc.stderr is not None else ""
    return subprocess.CompletedProcess(
        args=command,
        returncode=proc.returncode,
        stdout="".join(collected_lines),
        stderr=stderr_text,
    )


class _StepRunner:
    """
    Internal helper bundling the plan/registry/runner/emit context so
    run_plan()'s branches don't have to repeatedly pass the same five
    arguments or re-derive the step's description each time.
    """

    def __init__(
        self,
        *,
        plan: Plan,
        total: int,
        runner: Callable[..., Any],
        prompt: SudoPrompt | None,
        emit: Callable[[StepStatus, str], None],
        state: InterruptState,
        on_before_execute: Callable[[bool], None] | None = None,
        on_after_execute: Callable[[bool], None] | None = None,
        on_output_line: Callable[[str], None] | None = None,
        popen_factory: Callable[..., subprocess.Popen] = subprocess.Popen,
    ) -> None:
        self.plan = plan
        self.total = total
        self.runner = runner
        self.prompt = prompt
        self.emit = emit
        self.state = state
        self.description = plan.steps[0] if plan.steps else plan.action
        self.on_before_execute = on_before_execute
        self.on_after_execute = on_after_execute
        # `on_output_line`/`popen_factory` (added post-Build-Order): purely
        # additive -- when `on_output_line` is None (every call site and
        # test written before this feature, and still every call site that
        # doesn't opt in), `_execute` below calls `self.runner` exactly as
        # it always has. Only when a caller (main.py, for the real live
        # terminal UI) supplies `on_output_line` does execution switch to
        # `_run_streaming`/Popen. This keeps the existing `runner` contract
        # (and every test built on it) completely untouched.
        self.on_output_line = on_output_line
        self.popen_factory = popen_factory

    def _result(self, status: StepStatus, completed: subprocess.CompletedProcess | None = None, used_sudo: bool = False) -> StepResult:
        return StepResult(
            step_number=1,
            description=self.description,
            status=status,
            returncode=completed.returncode if completed is not None else None,
            stdout=completed.stdout if completed is not None else "",
            stderr=completed.stderr if completed is not None else "",
            used_sudo=used_sudo,
        )

    def _execute(self, command: str, *, used_sudo: bool) -> ExecutionResult:
        # `on_before_execute`/`on_after_execute` (both optional, UI-layer
        # hooks -- this module stays rich/terminal-agnostic, per this
        # file's own module docstring split) bracket the actual subprocess
        # call so a caller can suspend anything that keeps repainting the
        # terminal (a `rich.Live` spinner, specifically) while `used_sudo`
        # is True.
        #
        # --- Sudo password-prompt garbling bug fix (post-Build-Order,
        # found via real-terminal testing) ---
        # A `sudo <command>` child process reads its password prompt from
        # (and writes it to) the controlling terminal directly -- not
        # through this process's own stdout/stderr, which `capture_output=
        # True` pipes away regardless. `ui.streaming.StreamingRenderer`'s
        # `rich.Live` display was still actively repainting the same
        # terminal region at its own 8Hz refresh rate for the entire
        # duration of that blocking call (nothing here ever told it to
        # stop), so its redraws and sudo's own prompt/keystroke-echo writes
        # collided -- observed as a garbled, overlapping prompt line and
        # password entry silently not registering, needing several Enter
        # presses before it "took". `used_sudo` is passed through so the
        # hook only pauses rendering for the actual sudo-prefixed call,
        # not the normal (non-elevated) command.
        if self.on_before_execute is not None:
            self.on_before_execute(used_sudo)
        try:
            try:
                with _sigint_handler(self.state):
                    if self.on_output_line is not None:
                        completed = _run_streaming(
                            command,
                            state=self.state,
                            on_output_line=self.on_output_line,
                            popen_factory=self.popen_factory,
                        )
                    else:
                        completed = self.runner(command, shell=True, capture_output=True, text=True)
            except _DoubleInterrupt:
                self.emit(StepStatus.INTERRUPTED, "force-stopped (second Ctrl+C)")
                return ExecutionResult(
                    action=self.plan.action,
                    step_results=[self._result(StepStatus.INTERRUPTED, used_sudo=used_sudo)],
                    interrupted=True,
                )
        finally:
            # Always fires, even on the double-Ctrl+C path above -- a
            # paused Live display must resume no matter how the subprocess
            # call ended, not only on its ordinary completion.
            if self.on_after_execute is not None:
                self.on_after_execute(used_sudo)

        if self.state.interrupt_count >= 1 and completed.returncode != 0:
            # Single (graceful) Ctrl+C: the subprocess was allowed to finish
            # on its own; report interrupted rather than failed, since the
            # user explicitly asked to stop (Section 8.3.4).
            self.emit(StepStatus.INTERRUPTED, "stopped after current operation finished")
            return ExecutionResult(
                action=self.plan.action,
                step_results=[self._result(StepStatus.INTERRUPTED, completed, used_sudo=used_sudo)],
                interrupted=True,
            )

        if completed.returncode == 0:
            self.emit(StepStatus.DONE, completed.stdout)
            return ExecutionResult(
                action=self.plan.action,
                step_results=[self._result(StepStatus.DONE, completed, used_sudo=used_sudo)],
            )

        if not used_sudo and _looks_like_permission_denied(completed.stderr):
            return self._escalate(command)

        self.emit(StepStatus.FAILED, completed.stderr)
        return ExecutionResult(
            action=self.plan.action,
            step_results=[self._result(StepStatus.FAILED, completed, used_sudo=used_sudo)],
        )

    def _escalate(self, command: str) -> ExecutionResult:
        elevated = ElevatedStep(
            step_number=1,
            total_steps=self.total,
            description=self.description,
            reason="this operation requires elevated (root) permission",
        )
        decision = decide_step(elevated, prompt=self.prompt)

        if decision is SudoDecision.GRANT:
            return self._execute(f"sudo {command}", used_sudo=True)

        if decision is SudoDecision.SKIP:
            self.emit(StepStatus.SKIPPED, "skipped (elevated permission declined)")
            return ExecutionResult(action=self.plan.action, step_results=[self._result(StepStatus.SKIPPED)])

        # ABORT
        self.emit(StepStatus.INTERRUPTED, "aborted (elevated permission declined)")
        return ExecutionResult(
            action=self.plan.action,
            step_results=[self._result(StepStatus.INTERRUPTED)],
            aborted_for_sudo=True,
        )

    def run(self, command: str) -> ExecutionResult:
        self.emit(StepStatus.RUNNING, command)
        return self._execute(command, used_sudo=False)


def run_plan(
    plan: Plan,
    registry,
    *,
    prompt: SudoPrompt | None = None,
    on_event: Callable[[StepEvent], None] | None = None,
    runner: Callable[..., Any] = subprocess.run,
    interrupt_state: InterruptState | None = None,
    on_before_execute: Callable[[bool], None] | None = None,
    on_after_execute: Callable[[bool], None] | None = None,
    on_output_line: Callable[[str], None] | None = None,
    popen_factory: Callable[..., subprocess.Popen] = subprocess.Popen,
) -> ExecutionResult:
    """
    Execute `plan` (blocking), streaming a StepEvent for its one executable
    command (see module docstring's Scope note for why a plan is currently
    always one command). `on_event`, if given, receives every StepEvent as
    it happens -- Step 11 wires this to `rich` rendering.

    `on_before_execute`/`on_after_execute` (both optional, each called with
    one bool -- `used_sudo`) bracket the actual blocking subprocess call.
    See `_StepRunner._execute`'s own docstring comment for why this exists:
    a `sudo`-prefixed command reads/writes its password prompt directly on
    the controlling terminal, which collides with anything still
    repainting that same terminal on a timer (Step 11's `rich.Live`
    spinner) -- main.py uses these hooks to pause/resume that display
    around exactly the sudo-prefixed call, and only that one.

    `on_output_line` (added post-Build-Order, per the user's explicit
    "make the whole shell feel alive, every operation" request): optional,
    called once per line of the command's stdout AS IT ARRIVES (not after
    the command finishes). When given, execution switches internally from
    `runner` to Popen-based real-time reading (see `_run_streaming`'s own
    docstring for why this needed no threading and doesn't touch Ctrl+C
    handling); when omitted (every pre-existing call site and test), the
    original `runner`-based blocking call happens exactly as before --
    this parameter is purely additive. `popen_factory` is the Popen
    equivalent of `runner`, injectable for tests of the streaming path
    specifically.
    """
    state = interrupt_state if interrupt_state is not None else InterruptState()
    total = len(plan.steps) if plan.steps else 1
    command = render_command(registry.command_template_for(plan.action), plan.params)

    def emit(status: StepStatus, detail: str = "") -> None:
        if on_event is not None:
            on_event(StepEvent(step_number=1, total_steps=total, status=status, detail=detail))

    step_runner = _StepRunner(
        plan=plan,
        total=total,
        runner=runner,
        prompt=prompt,
        emit=emit,
        state=state,
        on_before_execute=on_before_execute,
        on_after_execute=on_after_execute,
        on_output_line=on_output_line,
        popen_factory=popen_factory,
    )
    return step_runner.run(command)