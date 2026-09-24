"""
Streaming Executor (Build Order Step 10, half of "executor.py + trash.py";
rewritten for the open-ended architecture -- see validation.py's module
docstring for the full rationale).

Runs a Plan (plan_generator.py), blocking, emitting a structured event
stream -- Section 8.3.4's "Execution — Live Streaming ও Interrupt Handling"
and the Module 2<->Module 4 event-format contract (Section 4.2):

    {"step": 2, "status": "running"|"done"|"failed"|"interrupted",
     "progress": {"done": 178, "total": 340}}

Step 11 (ui/streaming.py) turns these events into the blueprint's
`rich`-rendered progress bars; this module only emits StepEvent objects
(same split used by plan_generator.py/discussion.py/sudo_layer.py: plain
data + a plain-text default renderer here, `rich` panels later).

--- Architecture change ---
Under the original 4-action registry, a Plan carried an `action`/`params`
pair and the executable command was rendered from capabilities.json's
`command_template` at run time (see the removed `render_command()` and its
shell-injection-safe `shlex.quote()` param substitution — no longer needed
here, since there are no template params to substitute any more).

Under the open-ended architecture, the Intent Parser's own model call
already produced the final, complete, directly-runnable command text
(validation.py's ValidatedIntent.command); plan_generator.py carries it
through unchanged onto Plan.command. So run_plan() now executes
`plan.command` directly — no template, no registry lookup, no per-param
quoting step. Shell-injection safety no longer applies the same way either:
the whole command is model-generated free text, not a trusted template with
untrusted param values spliced in, so there is no "trusted syntax vs.
untrusted value" boundary left to enforce with shlex.quote() — the model's
raw command IS the syntax. The one thing standing between a bad command and
real execution is confirmation (discussion.py's plan panel) plus
danger_classifier.py's independent risk check (both upstream of this
module, unchanged in spirit from the raw-shell path that already worked
this way before this rewrite).

Scope note: a plan is still always exactly one executable command per run
(same as before — nothing in the open-ended architecture introduces
multi-command plans), so run_plan() still reports one StepEvent/StepResult
(step 1 of 1).

--- Design decisions (Section 16 Rule 5 -- confirmed with the user) ---

1. Sudo-escalation trigger: detection is reactive, same as before -- a step
   runs normally first; if it fails with a shell "Permission denied" /
   "Operation not permitted" signature in stderr, the executor treats that
   as a permission-escalation case and calls sudo_layer.decide_step()
   before deciding whether to retry the same command prefixed with `sudo`.

2. Timeouts/exit codes/error handling (the blueprint is silent on all
   three): a non-zero exit for any reason OTHER than the permission-denied
   case above is reported as "failed" (matching the event-format's status
   enum); since a plan is currently always one command, "stopping the rest
   of the plan" on failure is automatic. No process timeout is enforced
   (nothing in the blueprint calls for one, and an artificial timeout sized
   to the user's own data would be an arbitrary guess); Ctrl+C is the way
   to stop a stuck step.

Audit-log tie-in: run_plan() returns a complete ExecutionResult (step
outcomes, interrupted flag, sudo usage) so whatever calls it has everything
needed to log it.
"""

from __future__ import annotations

import os
import pty
import select
import shutil
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


class _PtyLineReader:
    """
    Wraps a PTY master fd as a line iterator (`for line in this: ...`),
    matching the shape `_run_streaming` already expects from a plain pipe's
    `proc.stdout` (see `_FakePopen.stdout` in tests/test_executor.py, which
    this mirrors on purpose). Internally buffers partial reads until a
    newline shows up, same as a real file object's line iteration would.
    """

    def __init__(self, fd: int) -> None:
        self._fd = fd
        self._buffer = ""
        self._closed = False

    def __iter__(self):
        return self

    def __next__(self) -> str:
        while "\n" not in self._buffer:
            if self._closed:
                if self._buffer:
                    line, self._buffer = self._buffer, ""
                    return line
                raise StopIteration
            try:
                chunk = os.read(self._fd, 4096)
            except OSError:
                # Slave side closed (child exited) -- EIO on Linux ptys.
                self._closed = True
                continue
            if not chunk:
                self._closed = True
                continue
            self._buffer += chunk.decode(errors="replace")
        line, self._buffer = self._buffer.split("\n", 1)
        return line + "\n"


class _PtyPopen:
    """
    Bug fix (found via manual end-to-end testing, real-terminal session):
    the plain-pipe `subprocess.Popen(..., stdout=PIPE)` path this executor
    always used made every AI-generated/raw command's output arrive with
    NO color, even for commands (`ls`, `grep`, `dpkg`, `git`, ...) whose
    default behavior is to colorize automatically -- coreutils and most
    modern CLI tools call `isatty()` on their stdout and silently disable
    color the moment it's a pipe rather than a real terminal, which a
    plain `Popen(stdout=PIPE)` always is. Compare against this same
    project's OWN raw-shell path (Section 8.3.5's pass-through), which
    never had this problem because it never redirects the child's
    stdout/stderr at all -- they inherit this process's real terminal fds
    directly.

    The fix: give each child TWO real pseudo-terminals (one for stdout,
    one for stderr -- kept separate so `_looks_like_permission_denied`'s
    stderr-only check still works) instead of pipes. A pty's slave end
    behaves like a real terminal from the child's point of view --
    `isatty()` is true on it -- so `ls`'s (etc.) own `--color=auto`
    default activates with no special-casing per command, exactly as it
    would typing the same command directly into a terminal. Verified
    directly (both in this fix's own development and by a plain-pipe vs.
    pty comparison): `ls --color=auto` emits zero ANSI codes over a pipe,
    real ANSI color codes over a pty.

    This class exists to keep that pty plumbing entirely inside one
    object with the exact same shape `_run_streaming` already consumes
    (`stdout` as a line iterator, `stderr.read()`, `wait()`, `kill()`,
    `returncode`) -- see tests/test_executor.py's `_FakePopen`, which this
    mirrors on purpose, so `_run_streaming` itself needed zero changes.
    Only the *default* `popen_factory` (this class, via
    `_default_popen_factory`) uses real ptys; every existing test injects
    its own `popen_factory` and is completely unaffected.

    --- Second bug fix: wrong shell binary (found via a real screenshot --
    `la` through an AI-generated plan came out with zero color/no custom
    `eza` output at all, while the exact same `ls -la` run as a raw shell
    command showed full color) ---
    `subprocess.Popen(command, shell=True, ...)` with no `executable=`
    kwarg always runs the command through `/bin/sh` (dash, on Ubuntu/most
    distros) -- NOT the person's own real login shell, regardless of what
    `$SHELL` is set to. That's a completely different bug from the pty-vs-
    pipe one above: even with a real pty giving `isatty()` a true answer,
    `/bin/sh` never sources the person's fish `config.fish`, so their own
    aliases (`alias la=...`/`alias ls="eza ..."`) and `LS_COLORS`/similar
    exports are simply never in scope for an AI-generated plan's command --
    `main.py`'s OWN raw-shell path (`_raw_shell_popen_args`) already had
    the right fix for the identical problem (prefer `$SHELL` when it points
    at a real, existing executable); this constructor now does the same,
    via `subprocess.Popen`'s `executable=` kwarg, which is precisely what
    it exists for: pick a specific interpreter to run a `shell=True`
    command string with, while `shell=True` itself is kept (pipes/
    redirects/chaining an AI-generated command might use still need a real
    shell to interpret the whole string, exactly as before).
    """

    def __init__(self, command: str, *, shell: bool, text: bool, bufsize: int) -> None:
        out_master, out_slave = pty.openpty()
        err_master, err_slave = pty.openpty()
        shell_path = os.environ.get("SHELL")
        executable = shell_path if shell_path and shutil.which(shell_path) else None
        # Same fish-greeting-noise env var main.py's OWN raw-shell path sets
        # (`main._FISH_GUARD_ENV`, value "OMSH_RAW_EXEC") -- not imported
        # directly (main.py imports FROM executor.py, so the reverse import
        # would be circular), just the same literal string. Now that this
        # constructor runs AI-generated plan commands through the person's
        # real fish (see this class's own docstring, "Second bug fix"
        # above), config.fish's fastfetch/neofetch call would otherwise
        # reprint before every single AI-executed command too, exactly the
        # noise `ensure_fish_guard_installed()` already wraps that call to
        # prevent for raw shell commands -- this makes that same wrapped
        # guard fire here as well, so an AI-executed `ls`/`la`/etc. gets the
        # person's real aliases/LS_COLORS without the banner coming back.
        child_env = dict(os.environ, OMSH_RAW_EXEC="1")
        try:
            self._proc = subprocess.Popen(
                command,
                shell=shell,
                executable=executable,
                stdout=out_slave,
                stderr=err_slave,
                close_fds=True,
                env=child_env,
            )
        finally:
            # The child has its own duplicated fds now; this process's
            # copies of the slave ends must close so the master ends see
            # EOF/EIO once the child exits (otherwise reads on the master
            # would block forever waiting for a write that never comes).
            os.close(out_slave)
            os.close(err_slave)
        self._out_master = out_master
        self._err_master = err_master
        self.stdout = _PtyLineReader(out_master)
        self.stderr = _PtyStderrReader(err_master)
        self.returncode: int | None = None

    def wait(self) -> int:
        self.returncode = self._proc.wait()
        return self.returncode

    def kill(self) -> None:
        self._proc.kill()


class _PtyStderrReader:
    """`.read()`-once shape matching `proc.stderr.read()`'s existing use in
    `_run_streaming` (called only after the child has exited)."""

    def __init__(self, fd: int) -> None:
        self._fd = fd

    def read(self) -> str:
        chunks: list[bytes] = []
        while True:
            try:
                chunk = os.read(self._fd, 4096)
            except OSError:
                break
            if not chunk:
                break
            chunks.append(chunk)
        return b"".join(chunks).decode(errors="replace")


def _default_popen_factory(command: str, **kwargs: Any) -> _PtyPopen:
    """
    `run_plan()`'s real (non-test) `popen_factory` default -- see
    `_PtyPopen`'s own docstring for why this replaced a plain
    `subprocess.Popen(..., stdout=PIPE)` call. `kwargs` mirrors what
    `_run_streaming` has always passed (`shell`, `stdout`, `stderr`,
    `text`, `bufsize`) -- `stdout`/`stderr` are accepted and ignored here
    (both always become ptys now; there is no alternative destination for
    this factory) rather than removed from the call site, so
    `_run_streaming` itself needed no change beyond which factory it's
    given by default.
    """
    return _PtyPopen(command, shell=kwargs.get("shell", True), text=kwargs.get("text", True), bufsize=kwargs.get("bufsize", 1))


def _run_streaming(
    command: str,
    *,
    state: InterruptState,
    on_output_line: Callable[[str], None],
    popen_factory: Callable[..., Any],
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
        popen_factory: Callable[..., Any] = _default_popen_factory,
    ) -> None:
        self.plan = plan
        self.total = total
        self.runner = runner
        self.prompt = prompt
        self.emit = emit
        self.state = state
        self.description = plan.steps[0] if plan.steps else plan.command
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
                    action=self.plan.command,
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
                action=self.plan.command,
                step_results=[self._result(StepStatus.INTERRUPTED, completed, used_sudo=used_sudo)],
                interrupted=True,
            )

        if completed.returncode == 0:
            self.emit(StepStatus.DONE, completed.stdout)
            return ExecutionResult(
                action=self.plan.command,
                step_results=[self._result(StepStatus.DONE, completed, used_sudo=used_sudo)],
            )

        if not used_sudo and _looks_like_permission_denied(completed.stderr):
            return self._escalate(command)

        self.emit(StepStatus.FAILED, completed.stderr)
        return ExecutionResult(
            action=self.plan.command,
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
            return ExecutionResult(action=self.plan.command, step_results=[self._result(StepStatus.SKIPPED)])

        # ABORT
        self.emit(StepStatus.INTERRUPTED, "aborted (elevated permission declined)")
        return ExecutionResult(
            action=self.plan.command,
            step_results=[self._result(StepStatus.INTERRUPTED)],
            aborted_for_sudo=True,
        )

    def run(self, command: str) -> ExecutionResult:
        self.emit(StepStatus.RUNNING, command)
        return self._execute(command, used_sudo=False)


def run_plan(
    plan: Plan,
    *,
    prompt: SudoPrompt | None = None,
    on_event: Callable[[StepEvent], None] | None = None,
    runner: Callable[..., Any] = subprocess.run,
    interrupt_state: InterruptState | None = None,
    on_before_execute: Callable[[bool], None] | None = None,
    on_after_execute: Callable[[bool], None] | None = None,
    on_output_line: Callable[[str], None] | None = None,
    popen_factory: Callable[..., Any] = _default_popen_factory,
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
    command = plan.command

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