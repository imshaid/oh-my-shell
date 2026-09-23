"""
Tests for the Streaming Executor (Build Order Step 10), rewritten for the
open-ended architecture (see executor.py's own module docstring). There is
no more registry/render_command/param-quoting pipeline: run_plan() executes
plan.command directly, with no registry argument at all.
"""

from __future__ import annotations

import subprocess
from dataclasses import dataclass

import pytest

from ohmyshell import executor
from ohmyshell.executor import (
    ExecutionResult,
    InterruptState,
    StepEvent,
    StepStatus,
    run_plan,
)
from ohmyshell.plan_generator import Plan
from ohmyshell.sudo_layer import SudoDecision


# ---------------------------------------------------------------------------
# Test doubles
# ---------------------------------------------------------------------------


@dataclass
class _FakeCompleted:
    returncode: int
    stdout: str = ""
    stderr: str = ""


class _ScriptedRunner:
    """Fake subprocess.run replacement returning pre-scripted results in order."""

    def __init__(self, results: list[_FakeCompleted]):
        self._results = list(results)
        self.calls: list[str] = []

    def __call__(self, command, **kwargs):
        self.calls.append(command)
        return self._results.pop(0)


class _FakePrompt:
    def __init__(self, decision: SudoDecision):
        self.decision = decision
        self.asked = False

    def ask(self, step):
        self.asked = True
        return self.decision


def _plan(command="find /tmp -mtime +7 -delete", steps=None) -> Plan:
    return Plan(
        command=command,
        risk="medium",
        explanation="Scan /tmp for files older than 7 days",
        steps=steps if steps is not None else ["Scan /tmp for files older than 7 days"],
    )


# ---------------------------------------------------------------------------
# run_plan — happy path
# ---------------------------------------------------------------------------


class TestRunPlanSuccess:
    def test_returns_done_status_on_zero_exit(self):
        runner = _ScriptedRunner([_FakeCompleted(returncode=0, stdout="ok")])
        result = run_plan(_plan(), runner=runner)
        assert result.step_results[0].status is StepStatus.DONE
        assert result.all_done

    def test_runs_the_plan_command_directly(self):
        runner = _ScriptedRunner([_FakeCompleted(returncode=0)])
        run_plan(_plan(command="kill -TERM 999"), runner=runner)
        assert runner.calls == ["kill -TERM 999"]

    def test_emits_running_then_done_events(self):
        runner = _ScriptedRunner([_FakeCompleted(returncode=0)])
        events: list[StepEvent] = []
        run_plan(_plan(), runner=runner, on_event=events.append)
        assert [e.status for e in events] == [StepStatus.RUNNING, StepStatus.DONE]

    def test_step_result_captures_stdout(self):
        runner = _ScriptedRunner([_FakeCompleted(returncode=0, stdout="340 files")])
        result = run_plan(_plan(), runner=runner)
        assert result.step_results[0].stdout == "340 files"

    def test_used_sudo_is_false_when_no_escalation_needed(self):
        runner = _ScriptedRunner([_FakeCompleted(returncode=0)])
        result = run_plan(_plan(), runner=runner)
        assert result.step_results[0].used_sudo is False

    def test_execution_result_action_carries_the_raw_command(self):
        """ExecutionResult.action stays named .action for back-compat, but now
        carries the raw command string, not a capability action name."""
        runner = _ScriptedRunner([_FakeCompleted(returncode=0)])
        result = run_plan(_plan(command="uptime"), runner=runner)
        assert result.action == "uptime"


class TestRunPlanFailure:
    def test_non_permission_failure_is_reported_as_failed(self):
        runner = _ScriptedRunner([_FakeCompleted(returncode=1, stderr="command not found")])
        result = run_plan(_plan(command="false_cmd"), runner=runner)
        assert result.step_results[0].status is StepStatus.FAILED
        assert not result.all_done

    def test_failure_does_not_trigger_sudo_prompt(self):
        runner = _ScriptedRunner([_FakeCompleted(returncode=1, stderr="No such file or directory")])
        prompt = _FakePrompt(SudoDecision.GRANT)
        run_plan(_plan(command="false_cmd"), runner=runner, prompt=prompt)
        assert prompt.asked is False

    def test_emits_failed_event_with_stderr_detail(self):
        runner = _ScriptedRunner([_FakeCompleted(returncode=2, stderr="boom")])
        events: list[StepEvent] = []
        run_plan(_plan(command="false_cmd"), runner=runner, on_event=events.append)
        assert events[-1].status is StepStatus.FAILED
        assert events[-1].detail == "boom"


# ---------------------------------------------------------------------------
# run_plan — sudo escalation (reactive detection)
# ---------------------------------------------------------------------------


class TestRunPlanSudoEscalation:
    def test_permission_denied_stderr_triggers_sudo_prompt(self):
        runner = _ScriptedRunner([
            _FakeCompleted(returncode=1, stderr="rm: cannot remove '/var/cache/x': Permission denied"),
            _FakeCompleted(returncode=0, stdout="done"),
        ])
        prompt = _FakePrompt(SudoDecision.GRANT)
        result = run_plan(_plan(command="rm /var/cache/x"), runner=runner, prompt=prompt)
        assert prompt.asked is True
        assert result.step_results[0].status is StepStatus.DONE
        assert result.step_results[0].used_sudo is True

    def test_grant_retries_with_sudo_prefix(self):
        runner = _ScriptedRunner([
            _FakeCompleted(returncode=1, stderr="Permission denied"),
            _FakeCompleted(returncode=0),
        ])
        run_plan(_plan(command="rm /var/cache/x"), runner=runner, prompt=_FakePrompt(SudoDecision.GRANT))
        assert runner.calls[1].startswith("sudo ")

    def test_operation_not_permitted_also_triggers_escalation(self):
        runner = _ScriptedRunner([
            _FakeCompleted(returncode=1, stderr="chmod: Operation not permitted"),
            _FakeCompleted(returncode=0),
        ])
        prompt = _FakePrompt(SudoDecision.GRANT)
        run_plan(_plan(command="chmod x"), runner=runner, prompt=prompt)
        assert prompt.asked is True

    def test_skip_decision_reports_skipped_and_does_not_retry(self):
        runner = _ScriptedRunner([_FakeCompleted(returncode=1, stderr="Permission denied")])
        result = run_plan(_plan(command="rm /var/cache/x"), runner=runner, prompt=_FakePrompt(SudoDecision.SKIP))
        assert result.step_results[0].status is StepStatus.SKIPPED
        assert len(runner.calls) == 1  # no sudo retry attempted

    def test_abort_decision_marks_aborted_for_sudo(self):
        runner = _ScriptedRunner([_FakeCompleted(returncode=1, stderr="Permission denied")])
        result = run_plan(_plan(command="rm /var/cache/x"), runner=runner, prompt=_FakePrompt(SudoDecision.ABORT))
        assert result.aborted_for_sudo is True
        assert result.step_results[0].status is StepStatus.INTERRUPTED

    def test_sudo_retry_that_still_fails_is_reported_failed(self):
        runner = _ScriptedRunner([
            _FakeCompleted(returncode=1, stderr="Permission denied"),
            _FakeCompleted(returncode=1, stderr="still broken"),
        ])
        result = run_plan(_plan(command="rm /var/cache/x"), runner=runner, prompt=_FakePrompt(SudoDecision.GRANT))
        assert result.step_results[0].status is StepStatus.FAILED
        assert result.step_results[0].used_sudo is True

    def test_sudo_retry_permission_denied_again_does_not_loop(self):
        runner = _ScriptedRunner([
            _FakeCompleted(returncode=1, stderr="Permission denied"),
            _FakeCompleted(returncode=1, stderr="Permission denied"),
        ])
        prompt = _FakePrompt(SudoDecision.GRANT)
        result = run_plan(_plan(command="rm /var/cache/x"), runner=runner, prompt=prompt)
        assert result.step_results[0].status is StepStatus.FAILED
        assert len(runner.calls) == 2


# ---------------------------------------------------------------------------
# run_plan — Ctrl+C interrupt handling
# ---------------------------------------------------------------------------


class TestRunPlanInterrupt:
    def test_single_interrupt_during_command_is_reported_interrupted(self):
        state = InterruptState()

        def runner(command, **kwargs):
            state.note_interrupt()  # simulate one SIGINT arriving mid-command
            return _FakeCompleted(returncode=130, stderr="")

        result = run_plan(_plan(command="long_running_cmd"), runner=runner, interrupt_state=state)
        assert result.interrupted is True
        assert result.step_results[0].status is StepStatus.INTERRUPTED

    def test_double_interrupt_force_stops(self):
        state = InterruptState()

        def runner(command, **kwargs):
            state.note_interrupt()
            state.note_interrupt()  # second call raises _DoubleInterrupt internally
            return _FakeCompleted(returncode=0)  # unreachable if force-stop works

        result = run_plan(_plan(command="stuck_cmd"), runner=runner, interrupt_state=state)
        assert result.interrupted is True
        assert "force-stopped" in result.step_results[0].description or result.step_results[0].status is StepStatus.INTERRUPTED

    def test_interrupted_run_is_not_all_done(self):
        state = InterruptState()

        def runner(command, **kwargs):
            state.note_interrupt()
            return _FakeCompleted(returncode=130)

        result = run_plan(_plan(command="cmd"), runner=runner, interrupt_state=state)
        assert result.all_done is False

    def test_successful_completion_after_no_interrupt_is_not_marked_interrupted(self):
        runner = _ScriptedRunner([_FakeCompleted(returncode=0)])
        result = run_plan(_plan(command="cmd"), runner=runner)
        assert result.interrupted is False


class TestRunPlanBeforeAfterExecuteHooks:
    """
    Regression tests for the sudo password-prompt garbling bug fix (see
    executor.py's own docstring for the full explanation).
    """

    def test_hooks_fire_around_a_plain_non_sudo_command(self):
        runner = _ScriptedRunner([_FakeCompleted(returncode=0, stdout="hi")])
        calls: list[tuple[str, bool]] = []
        run_plan(
            _plan(command="echo hi"),
            runner=runner,
            on_before_execute=lambda used_sudo: calls.append(("before", used_sudo)),
            on_after_execute=lambda used_sudo: calls.append(("after", used_sudo)),
        )
        assert calls == [("before", False), ("after", False)]

    def test_hooks_fire_again_with_used_sudo_true_on_escalation(self):
        runner = _ScriptedRunner([
            _FakeCompleted(returncode=1, stderr="Permission denied"),
            _FakeCompleted(returncode=0),
        ])
        calls: list[tuple[str, bool]] = []
        run_plan(
            _plan(command="rm /var/cache/x"),
            runner=runner,
            prompt=_FakePrompt(SudoDecision.GRANT),
            on_before_execute=lambda used_sudo: calls.append(("before", used_sudo)),
            on_after_execute=lambda used_sudo: calls.append(("after", used_sudo)),
        )
        assert calls == [
            ("before", False),
            ("after", False),
            ("before", True),
            ("after", True),
        ]

    def test_after_hook_still_fires_on_double_interrupt(self):
        def _raising_runner(command, **kwargs):
            raise executor._DoubleInterrupt()

        calls: list[tuple[str, bool]] = []
        result = run_plan(
            _plan(command="sleep 100"),
            runner=_raising_runner,
            on_before_execute=lambda used_sudo: calls.append(("before", used_sudo)),
            on_after_execute=lambda used_sudo: calls.append(("after", used_sudo)),
        )
        assert calls == [("before", False), ("after", False)]
        assert result.interrupted is True

    def test_hooks_are_optional_and_default_to_none(self):
        runner = _ScriptedRunner([_FakeCompleted(returncode=0, stdout="hi")])
        result = run_plan(_plan(command="echo hi"), runner=runner)
        assert result.all_done


# ---------------------------------------------------------------------------
# Live per-line streaming (on_output_line/popen_factory)
# ---------------------------------------------------------------------------


class _FakeStderr:
    def __init__(self, text: str) -> None:
        self._text = text

    def read(self) -> str:
        return self._text


class _FakePopen:
    def __init__(self, lines: list[str], *, returncode: int = 0, stderr_text: str = ""):
        self.stdout = iter(f"{line}\n" for line in lines)
        self.stderr = _FakeStderr(stderr_text)
        self.returncode = returncode
        self.killed = False

    def wait(self) -> int:
        return self.returncode

    def kill(self) -> None:
        self.killed = True


class _ScriptedPopenFactory:
    def __init__(self, fake_popens: list[_FakePopen]):
        self._fake_popens = list(fake_popens)
        self.calls: list[str] = []

    def __call__(self, command, **kwargs):
        self.calls.append(command)
        return self._fake_popens.pop(0)


class TestRunPlanLiveOutputStreaming:
    def test_on_output_line_called_once_per_line_in_order(self):
        popen_factory = _ScriptedPopenFactory(
            [_FakePopen(["renamed 'a' -> 'b'", "renamed 'c' -> 'd'"], returncode=0)]
        )
        seen: list[str] = []
        run_plan(_plan(command="mv -v a b"), on_output_line=seen.append, popen_factory=popen_factory)

        assert seen == ["renamed 'a' -> 'b'", "renamed 'c' -> 'd'"]

    def test_successful_streaming_run_reports_done(self):
        popen_factory = _ScriptedPopenFactory([_FakePopen(["renamed 'a' -> 'b'"], returncode=0)])
        result = run_plan(_plan(command="mv -v a b"), on_output_line=lambda line: None, popen_factory=popen_factory)

        assert result.step_results[0].status is StepStatus.DONE
        assert "renamed 'a' -> 'b'" in result.step_results[0].stdout

    def test_failed_streaming_run_reports_failed(self):
        popen_factory = _ScriptedPopenFactory(
            [_FakePopen([], returncode=1, stderr_text="mv: cannot stat 'a': No such file or directory")]
        )
        result = run_plan(_plan(command="mv -v a b"), on_output_line=lambda line: None, popen_factory=popen_factory)

        assert result.step_results[0].status is StepStatus.FAILED
        assert "No such file" in result.step_results[0].stderr

    def test_omitting_on_output_line_uses_plain_runner_not_popen_factory(self):
        runner = _ScriptedRunner([_FakeCompleted(returncode=0, stdout="hi")])

        def _popen_factory_that_must_not_be_called(*args, **kwargs):
            raise AssertionError("popen_factory should not be used when on_output_line is omitted")

        result = run_plan(_plan(command="echo hi"), runner=runner, popen_factory=_popen_factory_that_must_not_be_called)

        assert result.step_results[0].status is StepStatus.DONE
        assert runner.calls  # the plain runner path was actually used

    def test_sudo_escalation_retry_also_streams(self):
        popen_factory = _ScriptedPopenFactory(
            [
                _FakePopen([], returncode=1, stderr_text="Permission denied"),
                _FakePopen(["renamed 'a' -> 'b'"], returncode=0),
            ]
        )
        seen: list[str] = []
        result = run_plan(
            _plan(command="mv a b"),
            on_output_line=seen.append,
            popen_factory=popen_factory,
            prompt=_FakePrompt(SudoDecision.GRANT),
        )

        assert result.step_results[0].status is StepStatus.DONE
        assert seen == ["renamed 'a' -> 'b'"]
        assert popen_factory.calls[1].startswith("sudo ")

    def test_double_interrupt_kills_the_process_and_reports_interrupted(self):
        fake_popen = _FakePopen(["renamed 'a' -> 'b'", "renamed 'c' -> 'd'"], returncode=0)
        popen_factory = _ScriptedPopenFactory([fake_popen])
        state = InterruptState()

        def _on_line(line: str) -> None:
            state.note_interrupt()
            state.note_interrupt()

        result = run_plan(
            _plan(command="mv -v a b"), on_output_line=_on_line, popen_factory=popen_factory, interrupt_state=state
        )

        assert result.interrupted is True
        assert fake_popen.killed is True


# ---------------------------------------------------------------------------
# Removed: TestRenderCommand / TestTildeExpansion / TestRunPlanWithRealRegistry
# ---------------------------------------------------------------------------
#
# render_command(), _expand_and_quote(), and _quote_param() were all removed
# from executor.py under the open-ended architecture (see its own module
# docstring: the Intent Parser's model call now produces the final,
# complete, directly-runnable command text itself -- there is no template
# to render or per-param value to quote any more). Their shell-injection-
# safety property (a value can't inject a second command) no longer applies
# the same way either -- the whole command is model-generated free text,
# not a trusted template with untrusted values spliced in.