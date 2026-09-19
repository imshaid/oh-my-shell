"""Tests for the Streaming Executor (Build Order Step 10)."""

from __future__ import annotations

import subprocess
from dataclasses import dataclass

import pytest

from ohmyshell import executor, registry as registry_module
from ohmyshell.executor import (
    ExecutionResult,
    InterruptState,
    StepEvent,
    StepStatus,
    render_command,
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


def _fake_registry(command_template: str):
    class _Reg:
        def command_template_for(self, action):
            return command_template

    return _Reg()


def _plan(action="clean_temp_files", params=None, steps=None) -> Plan:
    return Plan(
        action=action,
        params=params if params is not None else {"days": 7, "paths": ["/tmp"]},
        risk="medium",
        steps=steps if steps is not None else ["Scan /tmp for files older than 7 days"],
    )


# ---------------------------------------------------------------------------
# render_command / shell-escaping
# ---------------------------------------------------------------------------


class TestRenderCommand:
    def test_substitutes_simple_param(self):
        cmd = render_command("kill -{signal} {target}", {"signal": "TERM", "target": "1234"})
        assert cmd == "kill -TERM 1234"

    def test_quotes_string_param_with_spaces(self):
        cmd = render_command("echo {msg}", {"msg": "hello world"})
        assert cmd == "echo 'hello world'"

    def test_quotes_list_param_items_and_space_joins(self):
        cmd = render_command("find {paths} -type f", {"paths": ["/tmp", "~/.cache"]})
        assert cmd == "find /tmp '~/.cache' -type f"

    def test_escapes_shell_metacharacters_in_value(self):
        cmd = render_command("echo {msg}", {"msg": "a; rm -rf /"})
        # The dangerous value must be quoted as a single argument, not
        # allowed to inject a second command.
        assert cmd == "echo 'a; rm -rf /'"
        assert "; rm -rf /" not in cmd.split("'")[0]

    def test_template_syntax_itself_is_untouched(self):
        cmd = render_command("find {paths} -mtime +{days} -exec mv {{}} ~/.oh-my-shell/.trash/ \\;", {"paths": ["/tmp"], "days": 7})
        assert "-exec mv {} ~/.oh-my-shell/.trash/ \\;" in cmd


# ---------------------------------------------------------------------------
# run_plan — happy path
# ---------------------------------------------------------------------------


class TestRunPlanSuccess:
    def test_returns_done_status_on_zero_exit(self):
        reg = _fake_registry("ps aux")
        runner = _ScriptedRunner([_FakeCompleted(returncode=0, stdout="ok")])
        result = run_plan(_plan(), reg, runner=runner)
        assert result.step_results[0].status is StepStatus.DONE
        assert result.all_done

    def test_runs_the_rendered_command(self):
        reg = _fake_registry("kill -{signal} {target}")
        runner = _ScriptedRunner([_FakeCompleted(returncode=0)])
        run_plan(_plan(action="kill_process", params={"target": "999", "signal": "TERM"}), reg, runner=runner)
        assert runner.calls == ["kill -TERM 999"]

    def test_emits_running_then_done_events(self):
        reg = _fake_registry("ps aux")
        runner = _ScriptedRunner([_FakeCompleted(returncode=0)])
        events: list[StepEvent] = []
        run_plan(_plan(), reg, runner=runner, on_event=events.append)
        assert [e.status for e in events] == [StepStatus.RUNNING, StepStatus.DONE]

    def test_step_result_captures_stdout(self):
        reg = _fake_registry("ps aux")
        runner = _ScriptedRunner([_FakeCompleted(returncode=0, stdout="340 files")])
        result = run_plan(_plan(), reg, runner=runner)
        assert result.step_results[0].stdout == "340 files"

    def test_used_sudo_is_false_when_no_escalation_needed(self):
        reg = _fake_registry("ps aux")
        runner = _ScriptedRunner([_FakeCompleted(returncode=0)])
        result = run_plan(_plan(), reg, runner=runner)
        assert result.step_results[0].used_sudo is False


class TestRunPlanFailure:
    def test_non_permission_failure_is_reported_as_failed(self):
        reg = _fake_registry("false_cmd")
        runner = _ScriptedRunner([_FakeCompleted(returncode=1, stderr="command not found")])
        result = run_plan(_plan(), reg, runner=runner)
        assert result.step_results[0].status is StepStatus.FAILED
        assert not result.all_done

    def test_failure_does_not_trigger_sudo_prompt(self):
        reg = _fake_registry("false_cmd")
        runner = _ScriptedRunner([_FakeCompleted(returncode=1, stderr="No such file or directory")])
        prompt = _FakePrompt(SudoDecision.GRANT)
        run_plan(_plan(), reg, runner=runner, prompt=prompt)
        assert prompt.asked is False

    def test_emits_failed_event_with_stderr_detail(self):
        reg = _fake_registry("false_cmd")
        runner = _ScriptedRunner([_FakeCompleted(returncode=2, stderr="boom")])
        events: list[StepEvent] = []
        run_plan(_plan(), reg, runner=runner, on_event=events.append)
        assert events[-1].status is StepStatus.FAILED
        assert events[-1].detail == "boom"


# ---------------------------------------------------------------------------
# run_plan — sudo escalation (reactive detection)
# ---------------------------------------------------------------------------


class TestRunPlanSudoEscalation:
    def test_permission_denied_stderr_triggers_sudo_prompt(self):
        reg = _fake_registry("rm /var/cache/x")
        runner = _ScriptedRunner([
            _FakeCompleted(returncode=1, stderr="rm: cannot remove '/var/cache/x': Permission denied"),
            _FakeCompleted(returncode=0, stdout="done"),
        ])
        prompt = _FakePrompt(SudoDecision.GRANT)
        result = run_plan(_plan(), reg, runner=runner, prompt=prompt)
        assert prompt.asked is True
        assert result.step_results[0].status is StepStatus.DONE
        assert result.step_results[0].used_sudo is True

    def test_grant_retries_with_sudo_prefix(self):
        reg = _fake_registry("rm /var/cache/x")
        runner = _ScriptedRunner([
            _FakeCompleted(returncode=1, stderr="Permission denied"),
            _FakeCompleted(returncode=0),
        ])
        run_plan(_plan(), reg, runner=runner, prompt=_FakePrompt(SudoDecision.GRANT))
        assert runner.calls[1].startswith("sudo ")

    def test_operation_not_permitted_also_triggers_escalation(self):
        reg = _fake_registry("chmod x")
        runner = _ScriptedRunner([
            _FakeCompleted(returncode=1, stderr="chmod: Operation not permitted"),
            _FakeCompleted(returncode=0),
        ])
        prompt = _FakePrompt(SudoDecision.GRANT)
        run_plan(_plan(), reg, runner=runner, prompt=prompt)
        assert prompt.asked is True

    def test_skip_decision_reports_skipped_and_does_not_retry(self):
        reg = _fake_registry("rm /var/cache/x")
        runner = _ScriptedRunner([_FakeCompleted(returncode=1, stderr="Permission denied")])
        result = run_plan(_plan(), reg, runner=runner, prompt=_FakePrompt(SudoDecision.SKIP))
        assert result.step_results[0].status is StepStatus.SKIPPED
        assert len(runner.calls) == 1  # no sudo retry attempted

    def test_abort_decision_marks_aborted_for_sudo(self):
        reg = _fake_registry("rm /var/cache/x")
        runner = _ScriptedRunner([_FakeCompleted(returncode=1, stderr="Permission denied")])
        result = run_plan(_plan(), reg, runner=runner, prompt=_FakePrompt(SudoDecision.ABORT))
        assert result.aborted_for_sudo is True
        assert result.step_results[0].status is StepStatus.INTERRUPTED

    def test_sudo_retry_that_still_fails_is_reported_failed(self):
        reg = _fake_registry("rm /var/cache/x")
        runner = _ScriptedRunner([
            _FakeCompleted(returncode=1, stderr="Permission denied"),
            _FakeCompleted(returncode=1, stderr="still broken"),
        ])
        result = run_plan(_plan(), reg, runner=runner, prompt=_FakePrompt(SudoDecision.GRANT))
        assert result.step_results[0].status is StepStatus.FAILED
        assert result.step_results[0].used_sudo is True

    def test_sudo_retry_permission_denied_again_does_not_loop(self):
        # If even the sudo-prefixed retry says permission denied, we must
        # not escalate a second time (used_sudo=True short-circuits it) --
        # otherwise this could recurse forever.
        reg = _fake_registry("rm /var/cache/x")
        runner = _ScriptedRunner([
            _FakeCompleted(returncode=1, stderr="Permission denied"),
            _FakeCompleted(returncode=1, stderr="Permission denied"),
        ])
        prompt = _FakePrompt(SudoDecision.GRANT)
        result = run_plan(_plan(), reg, runner=runner, prompt=prompt)
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

        reg = _fake_registry("long_running_cmd")
        result = run_plan(_plan(), reg, runner=runner, interrupt_state=state)
        assert result.interrupted is True
        assert result.step_results[0].status is StepStatus.INTERRUPTED

    def test_double_interrupt_force_stops(self):
        state = InterruptState()

        def runner(command, **kwargs):
            state.note_interrupt()
            state.note_interrupt()  # second call raises _DoubleInterrupt internally
            return _FakeCompleted(returncode=0)  # unreachable if force-stop works

        reg = _fake_registry("stuck_cmd")
        result = run_plan(_plan(), reg, runner=runner, interrupt_state=state)
        assert result.interrupted is True
        assert "force-stopped" in result.step_results[0].description or result.step_results[0].status is StepStatus.INTERRUPTED

    def test_interrupted_run_is_not_all_done(self):
        state = InterruptState()

        def runner(command, **kwargs):
            state.note_interrupt()
            return _FakeCompleted(returncode=130)

        reg = _fake_registry("cmd")
        result = run_plan(_plan(), reg, runner=runner, interrupt_state=state)
        assert result.all_done is False

    def test_successful_completion_after_no_interrupt_is_not_marked_interrupted(self):
        reg = _fake_registry("cmd")
        runner = _ScriptedRunner([_FakeCompleted(returncode=0)])
        result = run_plan(_plan(), reg, runner=runner)
        assert result.interrupted is False


# ---------------------------------------------------------------------------
# Integration with the real registry (sanity check against actual templates)
# ---------------------------------------------------------------------------


class TestRunPlanWithRealRegistry:
    def test_kill_process_command_renders_correctly_from_real_registry(self):
        reg = registry_module.load()
        template = reg.command_template_for("kill_process")
        cmd = render_command(template, {"target": "1234", "signal": "TERM"})
        assert cmd == "kill -TERM 1234"

    def test_list_processes_command_renders_correctly(self):
        reg = registry_module.load()
        template = reg.command_template_for("list_processes")
        cmd = render_command(template, {"filter": "chrome", "sort_by": "cpu"})
        assert "chrome" in cmd
        assert "cpu" in cmd

    def test_run_plan_executes_against_real_registry_with_fake_runner(self):
        reg = registry_module.load()
        runner = _ScriptedRunner([_FakeCompleted(returncode=0, stdout="pid 1  0.1%  init")])
        plan = _plan(action="list_processes", params={"filter": "", "sort_by": "none"}, steps=["List running processes"])
        result = run_plan(plan, reg, runner=runner)
        assert result.step_results[0].status is StepStatus.DONE