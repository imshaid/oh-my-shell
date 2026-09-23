"""Tests for the Streaming Executor (Build Order Step 10)."""

from __future__ import annotations

import os
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
        # Non-tilde items still quote exactly as before; the tilde-specific
        # expansion is covered by TestTildeExpansion below (bug fix,
        # post-Build-Order -- see _expand_and_quote's own docstring).
        cmd = render_command("find {paths} -type f", {"paths": ["/tmp", "/var/tmp"]})
        assert cmd == "find /tmp /var/tmp -type f"

    def test_escapes_shell_metacharacters_in_value(self):
        cmd = render_command("echo {msg}", {"msg": "a; rm -rf /"})
        # The dangerous value must be quoted as a single argument, not
        # allowed to inject a second command.
        assert cmd == "echo 'a; rm -rf /'"
        assert "; rm -rf /" not in cmd.split("'")[0]

    def test_template_syntax_itself_is_untouched(self):
        cmd = render_command("find {paths} -mtime +{days} -exec mv {{}} ~/.oh-my-shell/.trash/ \\;", {"paths": ["/tmp"], "days": 7})
        assert "-exec mv {} ~/.oh-my-shell/.trash/ \\;" in cmd


class TestTildeExpansion:
    """
    Bug fix (found via manual end-to-end testing, post-Build-Order):
    shlex.quote("~/.cache") -> "'~/.cache'" defeats shell tilde expansion
    (a POSIX shell never expands `~` inside single quotes), so
    capabilities.json's own clean_temp_files default (paths including
    "~/.cache") silently never matched anything. See executor.py's
    `_expand_and_quote()` docstring for the full root-cause explanation.
    """

    def test_bare_tilde_path_is_expanded_before_quoting(self):
        cmd = render_command("find {paths} -type f", {"paths": ["~/.cache"]})
        home = os.path.expanduser("~")
        assert cmd == f"find {home}/.cache -type f"
        assert "~" not in cmd

    def test_mixed_absolute_and_tilde_paths(self):
        cmd = render_command("find {paths} -type f", {"paths": ["/tmp", "~/.cache"]})
        home = os.path.expanduser("~")
        assert cmd == f"find /tmp {home}/.cache -type f"

    def test_bare_tilde_alone_is_expanded(self):
        cmd = render_command("echo {path}", {"path": "~"})
        home = os.path.expanduser("~")
        assert cmd == f"echo {home}"

    def test_tilde_not_at_start_is_left_alone(self):
        # Only a LEADING ~ or ~/ means "home directory" in shell semantics;
        # a tilde elsewhere in a value (e.g. part of a filename) must not
        # be expanded -- shlex.quote() may still wrap it in quotes (its own
        # normal, unrelated escaping choice for a value containing "~"),
        # but the literal text "file~backup" itself must be untouched, not
        # rewritten into some expanded-home-directory form.
        cmd = render_command("echo {msg}", {"msg": "file~backup"})
        assert "file~backup" in cmd
        assert os.path.expanduser("~") not in cmd

    def test_tilde_username_form_is_not_expanded(self):
        # ~otheruser is a different (unsupported here) shell feature --
        # os.path.expanduser only resolves the current user's own `~`, and
        # this module makes no attempt to resolve another user's home; the
        # value is quoted as an ordinary literal instead of being guessed at.
        cmd = render_command("echo {path}", {"path": "~otheruser/data"})
        assert "otheruser" in cmd


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

class TestRunPlanBeforeAfterExecuteHooks:
    """
    Regression tests for the sudo password-prompt garbling bug fix: a real
    `sudo <command>` subprocess reads/writes its password prompt directly
    on the controlling terminal, which collided with `rich.Live`'s own
    repaint loop still running throughout that blocking call (observed on
    real-terminal testing as a garbled prompt needing several Enter
    presses). `on_before_execute`/`on_after_execute` let main.py pause and
    resume that display around exactly the sudo-prefixed call -- these
    tests check the hooks fire with the right `used_sudo` value and in the
    right order, without asserting anything about `rich` itself (that's
    ui/streaming.py's own test module's job).
    """

    def test_hooks_fire_around_a_plain_non_sudo_command(self):
        reg = _fake_registry("echo hi")
        runner = _ScriptedRunner([_FakeCompleted(returncode=0, stdout="hi")])
        calls: list[tuple[str, bool]] = []
        run_plan(
            _plan(),
            reg,
            runner=runner,
            on_before_execute=lambda used_sudo: calls.append(("before", used_sudo)),
            on_after_execute=lambda used_sudo: calls.append(("after", used_sudo)),
        )
        assert calls == [("before", False), ("after", False)]

    def test_hooks_fire_again_with_used_sudo_true_on_escalation(self):
        reg = _fake_registry("rm /var/cache/x")
        runner = _ScriptedRunner([
            _FakeCompleted(returncode=1, stderr="Permission denied"),
            _FakeCompleted(returncode=0),
        ])
        calls: list[tuple[str, bool]] = []
        run_plan(
            _plan(),
            reg,
            runner=runner,
            prompt=_FakePrompt(SudoDecision.GRANT),
            on_before_execute=lambda used_sudo: calls.append(("before", used_sudo)),
            on_after_execute=lambda used_sudo: calls.append(("after", used_sudo)),
        )
        # First (failed, non-sudo) attempt brackets with used_sudo=False;
        # the sudo-prefixed retry brackets again with used_sudo=True -- the
        # second pair is the one a caller actually needs to react to.
        assert calls == [
            ("before", False),
            ("after", False),
            ("before", True),
            ("after", True),
        ]

    def test_after_hook_still_fires_on_double_interrupt(self):
        """
        `on_after_execute` must fire even when the subprocess call ends via
        the _DoubleInterrupt exception path -- a paused Live display must
        never be left permanently paused because of how a step ended.
        """
        reg = _fake_registry("sleep 100")

        def _raising_runner(command, **kwargs):
            raise executor._DoubleInterrupt()

        calls: list[tuple[str, bool]] = []
        result = run_plan(
            _plan(),
            reg,
            runner=_raising_runner,
            on_before_execute=lambda used_sudo: calls.append(("before", used_sudo)),
            on_after_execute=lambda used_sudo: calls.append(("after", used_sudo)),
        )
        assert calls == [("before", False), ("after", False)]
        assert result.interrupted is True

    def test_hooks_are_optional_and_default_to_none(self):
        """Existing callers that don't pass these hooks must be unaffected --
        no crash from a missing callable."""
        reg = _fake_registry("echo hi")
        runner = _ScriptedRunner([_FakeCompleted(returncode=0, stdout="hi")])
        result = run_plan(_plan(), reg, runner=runner)
        assert result.all_done


# ---------------------------------------------------------------------------
# Live per-line streaming (on_output_line/popen_factory, added post-Build-
# Order per the user's explicit "make the whole shell feel alive, every
# operation" request -- see executor.py's own _run_streaming docstring)
# ---------------------------------------------------------------------------


class _FakeStderr:
    def __init__(self, text: str) -> None:
        self._text = text

    def read(self) -> str:
        return self._text


class _FakePopen:
    """
    Fake Popen exposing just what `_run_streaming` reads: `.stdout` (an
    iterable of raw lines, each already carrying its own trailing "\n"
    exactly like a real pipe does), `.stderr` (has `.read()`), `.returncode`,
    `.wait()`, and `.kill()`.
    """

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
        reg = _fake_registry("mv -v a b")
        popen_factory = _ScriptedPopenFactory(
            [_FakePopen(["renamed 'a' -> 'b'", "renamed 'c' -> 'd'"], returncode=0)]
        )
        seen: list[str] = []
        run_plan(_plan(), reg, on_output_line=seen.append, popen_factory=popen_factory)

        assert seen == ["renamed 'a' -> 'b'", "renamed 'c' -> 'd'"]

    def test_successful_streaming_run_reports_done(self):
        reg = _fake_registry("mv -v a b")
        popen_factory = _ScriptedPopenFactory([_FakePopen(["renamed 'a' -> 'b'"], returncode=0)])
        result = run_plan(_plan(), reg, on_output_line=lambda line: None, popen_factory=popen_factory)

        assert result.step_results[0].status is StepStatus.DONE
        assert "renamed 'a' -> 'b'" in result.step_results[0].stdout

    def test_failed_streaming_run_reports_failed(self):
        reg = _fake_registry("mv -v a b")
        popen_factory = _ScriptedPopenFactory(
            [_FakePopen([], returncode=1, stderr_text="mv: cannot stat 'a': No such file or directory")]
        )
        result = run_plan(_plan(), reg, on_output_line=lambda line: None, popen_factory=popen_factory)

        assert result.step_results[0].status is StepStatus.FAILED
        assert "No such file" in result.step_results[0].stderr

    def test_omitting_on_output_line_uses_plain_runner_not_popen_factory(self):
        """
        Backward-compatibility guarantee: every pre-existing call site
        (no `on_output_line`) must keep using `runner`, never touching
        `popen_factory` at all -- proven here by making the injected
        popen_factory raise if it's ever called.
        """
        reg = _fake_registry("echo hi")
        runner = _ScriptedRunner([_FakeCompleted(returncode=0, stdout="hi")])

        def _popen_factory_that_must_not_be_called(*args, **kwargs):
            raise AssertionError("popen_factory should not be used when on_output_line is omitted")

        result = run_plan(_plan(), reg, runner=runner, popen_factory=_popen_factory_that_must_not_be_called)

        assert result.step_results[0].status is StepStatus.DONE
        assert runner.calls  # the plain runner path was actually used

    def test_sudo_escalation_retry_also_streams(self):
        """
        The sudo-retry path reuses the same _StepRunner instance (see
        _escalate's own call to self._execute), so on_output_line/
        popen_factory must carry through to the retried, sudo-prefixed
        command too -- not just the first attempt.
        """
        reg = _fake_registry("mv a b")
        popen_factory = _ScriptedPopenFactory(
            [
                _FakePopen([], returncode=1, stderr_text="Permission denied"),
                _FakePopen(["renamed 'a' -> 'b'"], returncode=0),
            ]
        )
        seen: list[str] = []
        result = run_plan(
            _plan(),
            reg,
            on_output_line=seen.append,
            popen_factory=popen_factory,
            prompt=_FakePrompt(SudoDecision.GRANT),
        )

        assert result.step_results[0].status is StepStatus.DONE
        assert seen == ["renamed 'a' -> 'b'"]
        assert popen_factory.calls[1].startswith("sudo ")

    def test_double_interrupt_kills_the_process_and_reports_interrupted(self):
        """
        A second Ctrl+C mid-stream must not leave an orphaned child process
        running -- `_run_streaming` kills it before re-raising, the same
        safety property double-Ctrl+C already has on the non-streaming
        (`runner`) path (see TestRunPlanDoubleInterrupt below).
        """
        reg = _fake_registry("mv -v a b")
        fake_popen = _FakePopen(["renamed 'a' -> 'b'", "renamed 'c' -> 'd'"], returncode=0)
        popen_factory = _ScriptedPopenFactory([fake_popen])
        state = InterruptState()

        def _on_line(line: str) -> None:
            # Simulate the second SIGINT arriving while output is streaming.
            state.note_interrupt()
            state.note_interrupt()

        result = run_plan(
            _plan(), reg, on_output_line=_on_line, popen_factory=popen_factory, interrupt_state=state
        )

        assert result.interrupted is True
        assert fake_popen.killed is True