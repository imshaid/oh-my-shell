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
   results space-joined, so `{paths}` expands to e.g. `/tmp '~/.cache'` --
   safe to place directly into `find {paths} ...`.

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


def _quote_param(value: Any) -> str:
    """shlex.quote() a single param value, or space-join quoted list items."""
    if isinstance(value, (list, tuple)):
        return " ".join(shlex.quote(str(item)) for item in value)
    return shlex.quote(str(value))


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
    )
    return step_runner.run(command)