"""
Tests for main.py (Build Order Step 6, fully wired post-Build-Order to the
Section 4.1 pipeline, then to full `rich`/`prompt_toolkit` visual polish —
Step 11), rewritten for the open-ended architecture (see main.py's own
module docstring and intent_parser.py/validation.py/discussion.py's own
docstrings for the full rationale).

There is no more Capability Registry anywhere in main.py -- every function
signature drops the old `registry` argument. `_handle_natural_language`
now also runs `override_risk()` on the produced command before building
the plan; `_repl_edit_flow`/`edit_command` drive a raw command-text edit,
not a per-param edit.

`input()`/`ReplSession`, `subprocess.run`, `intent_parser.parse_intent`,
and the Executor/Audit Log are all mocked or redirected to a tmp_path
base_dir so these tests exercise only main.py's own wiring logic, not real
shell execution, a real model call, or the user's real ~/.oh-my-shell/.

Rich-rendered output (panels, the prompt, streaming) is asserted against an
injected `Console(file=io.StringIO(), force_terminal=False)` buffer's
rendered text, the same pattern ui/panels.py's and ui/streaming.py's own
test suites already use.
"""

from __future__ import annotations

import io
from unittest.mock import MagicMock, patch

import pytest
from rich.console import Console

from ohmyshell import config as config_module
from ohmyshell import main as main_module
from ohmyshell.discussion import Cancelled, Confirmed
from ohmyshell.executor import ExecutionResult, StepResult, StepStatus
from ohmyshell.intent_parser import IntentParseError, ParseResult
from ohmyshell.plan_generator import Plan
from ohmyshell.ui.panels import RichSudoPrompt
from ohmyshell.ui.prompt import render_prompt_ansi
from ohmyshell.validation import ValidatedIntent


@pytest.fixture
def default_cfg():
    return config_module.default_config()


def _buffer_console() -> tuple[io.StringIO, Console]:
    buffer = io.StringIO()
    console = Console(file=buffer, width=100, force_terminal=False)
    return buffer, console


# --- ui/prompt.render_prompt_ansi (main.py's prompt source) ------------------------


def test_render_prompt_ansi_shows_folder_name_only(default_cfg, tmp_path):
    rendered = render_prompt_ansi(default_cfg, cwd=tmp_path)
    assert tmp_path.name in rendered
    assert str(tmp_path) not in rendered  # full path must not appear, only the folder name


def test_render_prompt_ansi_contains_color_escapes(default_cfg, tmp_path):
    rendered = render_prompt_ansi(default_cfg, cwd=tmp_path)
    assert "\x1b[" in rendered


# --- _handle_raw_shell -------------------------------------------------------------


def test_handle_raw_shell_calls_subprocess_run_with_shell_true(default_cfg, tmp_path):
    with patch("ohmyshell.main.classify") as mock_classify:
        from ohmyshell.danger_classifier import ClassificationResult, Safe

        mock_classify.return_value = ClassificationResult(verdict=Safe(), source="regex")
        with patch("ohmyshell.main.subprocess.run") as mock_run:
            main_module._handle_raw_shell("ls -la", default_cfg, base_dir=tmp_path)
    mock_run.assert_called_once_with("ls -la", shell=True)


def test_handle_raw_shell_reports_os_error_without_raising(default_cfg, tmp_path, capsys):
    with patch("ohmyshell.main.classify") as mock_classify:
        from ohmyshell.danger_classifier import ClassificationResult, Safe

        mock_classify.return_value = ClassificationResult(verdict=Safe(), source="regex")
        with patch("ohmyshell.main.subprocess.run", side_effect=OSError("boom")):
            main_module._handle_raw_shell("whatever", default_cfg, base_dir=tmp_path)  # must not raise
    captured = capsys.readouterr()
    assert "boom" in captured.err


def test_handle_raw_shell_safe_verdict_never_prompts(default_cfg, tmp_path):
    from ohmyshell.danger_classifier import ClassificationResult, Safe

    with patch("ohmyshell.main.classify", return_value=ClassificationResult(verdict=Safe(), source="regex")):
        with patch("ohmyshell.main.subprocess.run"):
            confirm_fn = MagicMock()
            main_module._handle_raw_shell("ls -la", default_cfg, confirm=confirm_fn, base_dir=tmp_path)
    confirm_fn.assert_not_called()


def test_handle_raw_shell_destructive_verdict_prompts_and_runs_on_yes(default_cfg, tmp_path):
    from ohmyshell.danger_classifier import ClassificationResult, Destructive

    destructive = ClassificationResult(
        verdict=Destructive(explanation="dangerous", trash_alternative_possible=True),
        source="regex",
    )
    with patch("ohmyshell.main.classify", return_value=destructive):
        with patch("ohmyshell.main.subprocess.run") as mock_run:
            main_module._handle_raw_shell("rm -rf /tmp/x", default_cfg, confirm=lambda _: "y", base_dir=tmp_path)
    mock_run.assert_called_once_with("rm -rf /tmp/x", shell=True)


def test_handle_raw_shell_destructive_verdict_cancelled_on_no(default_cfg, tmp_path):
    from ohmyshell.danger_classifier import ClassificationResult, Destructive

    destructive = ClassificationResult(
        verdict=Destructive(explanation="dangerous", trash_alternative_possible=True),
        source="regex",
    )
    with patch("ohmyshell.main.classify", return_value=destructive):
        with patch("ohmyshell.main.subprocess.run") as mock_run:
            main_module._handle_raw_shell("rm -rf /tmp/x", default_cfg, confirm=lambda _: "n", base_dir=tmp_path)
    mock_run.assert_not_called()


def test_handle_raw_shell_destructive_verdict_shows_explanation_in_panel(default_cfg, tmp_path):
    from ohmyshell.danger_classifier import ClassificationResult, Destructive

    destructive = ClassificationResult(
        verdict=Destructive(explanation="this will delete everything", trash_alternative_possible=True),
        source="regex",
    )
    buffer, console = _buffer_console()
    with patch("ohmyshell.main.classify", return_value=destructive):
        with patch("ohmyshell.main.subprocess.run"):
            main_module._handle_raw_shell(
                "rm -rf /tmp/x", default_cfg, confirm=lambda _: "n", console=console, base_dir=tmp_path
            )
    assert "this will delete everything" in buffer.getvalue()


def test_handle_raw_shell_classifier_error_does_not_run_command(default_cfg, tmp_path):
    from ohmyshell.danger_classifier import DangerClassifierError

    buffer, console = _buffer_console()
    with patch("ohmyshell.main.classify", side_effect=DangerClassifierError("Ollama unreachable")):
        with patch("ohmyshell.main.subprocess.run") as mock_run:
            main_module._handle_raw_shell("some ambiguous command", default_cfg, console=console, base_dir=tmp_path)
    mock_run.assert_not_called()
    assert "Ollama unreachable" in buffer.getvalue()


def test_handle_raw_shell_destructive_verdict_offers_trash_option_when_possible(default_cfg, tmp_path):
    from ohmyshell.danger_classifier import ClassificationResult, Destructive

    destructive = ClassificationResult(
        verdict=Destructive(explanation="dangerous", trash_alternative_possible=True),
        source="regex",
    )
    buffer, console = _buffer_console()
    with patch("ohmyshell.main.classify", return_value=destructive):
        with patch("ohmyshell.main.subprocess.run"):
            main_module._handle_raw_shell(
                "rm -rf /tmp/x", default_cfg, confirm=lambda _: "n", console=console, base_dir=tmp_path
            )
    assert "Move to trash instead" in buffer.getvalue()


def test_handle_raw_shell_destructive_verdict_omits_trash_option_when_not_possible(default_cfg, tmp_path):
    from ohmyshell.danger_classifier import ClassificationResult, Destructive

    destructive = ClassificationResult(
        verdict=Destructive(explanation="wipes a device", trash_alternative_possible=False),
        source="regex",
    )
    buffer, console = _buffer_console()
    with patch("ohmyshell.main.classify", return_value=destructive):
        with patch("ohmyshell.main.subprocess.run"):
            main_module._handle_raw_shell(
                "dd if=/dev/zero of=/dev/sda", default_cfg, confirm=lambda _: "n", console=console, base_dir=tmp_path
            )
    assert "Move to trash" not in buffer.getvalue()


def test_handle_raw_shell_trash_choice_moves_target_instead_of_running(default_cfg, tmp_path):
    from ohmyshell.danger_classifier import ClassificationResult, Destructive

    target = tmp_path / "doomed.txt"
    target.write_text("x")
    destructive = ClassificationResult(
        verdict=Destructive(explanation="dangerous", trash_alternative_possible=True),
        source="regex",
    )
    with patch("ohmyshell.main.classify", return_value=destructive):
        with patch("ohmyshell.main.subprocess.run") as mock_run:
            main_module._handle_raw_shell(f"rm -rf {target}", default_cfg, confirm=lambda _: "t", base_dir=tmp_path)
    mock_run.assert_not_called()
    assert not target.exists()

    from ohmyshell import trash as trash_module

    assert len(trash_module.list_trash(base_dir=tmp_path)) == 1


def test_handle_raw_shell_logs_audit_entry_on_run(default_cfg, tmp_path):
    from ohmyshell.danger_classifier import ClassificationResult, Safe

    with patch("ohmyshell.main.classify", return_value=ClassificationResult(verdict=Safe(), source="regex")):
        with patch("ohmyshell.main.subprocess.run"):
            main_module._handle_raw_shell("ls -la", default_cfg, base_dir=tmp_path)

    from ohmyshell import audit_log as audit_log_module

    entries = audit_log_module.read_entries(base_dir=tmp_path)
    assert len(entries) == 1
    assert entries[0].source == "raw_shell"
    assert entries[0].status == "done"


# --- _handle_natural_language --------------------------------------------------------


def _fake_parse_result(command="ps aux", risk="low", explanation="List processes.", attempts=1):
    return ParseResult(
        action=command,
        intent=ValidatedIntent(command=command, risk=risk, explanation=explanation),
        attempts=attempts,
    )


def _fake_plan(command="ps aux", risk="low", explanation="List processes.", steps=None):
    return Plan(command=command, risk=risk, explanation=explanation, steps=steps or [explanation or command])


def test_handle_natural_language_reports_unmapped(default_cfg, tmp_path):
    fake_result = ParseResult(action="unmapped", intent=None, attempts=2, last_error="nope")
    buffer, console = _buffer_console()
    with patch("ohmyshell.main.parse_intent", return_value=fake_result):
        main_module._handle_natural_language(
            "do something weird", default_cfg, console=console, base_dir=tmp_path
        )
    assert "couldn't turn that into a command" in buffer.getvalue().lower()


def test_handle_natural_language_reports_backend_failure(default_cfg, tmp_path):
    buffer, console = _buffer_console()
    with patch("ohmyshell.main.parse_intent", side_effect=IntentParseError("Ollama unreachable")):
        main_module._handle_natural_language(
            "clean up temp files", default_cfg, console=console, base_dir=tmp_path
        )
    assert "Ollama unreachable" in buffer.getvalue()


def test_handle_natural_language_cancel_never_executes_anything(default_cfg, tmp_path):
    """Cancelling at the Discussion Loop must not touch subprocess/run_plan at all."""
    fake_result = _fake_parse_result(command="find /tmp -mtime +7 -delete", risk="medium")

    with patch("ohmyshell.main.parse_intent", return_value=fake_result):
        with patch("ohmyshell.main.run_discussion", return_value=Cancelled()):
            with patch("ohmyshell.main.run_plan") as mock_run_plan:
                main_module._handle_natural_language(
                    "clean up temp files", default_cfg, base_dir=tmp_path
                )
    mock_run_plan.assert_not_called()


def test_handle_natural_language_cancel_logs_cancelled_status(default_cfg, tmp_path):
    fake_result = _fake_parse_result(command="find /tmp -mtime +7 -delete", risk="medium")

    with patch("ohmyshell.main.parse_intent", return_value=fake_result):
        with patch("ohmyshell.main.run_discussion", return_value=Cancelled()):
            main_module._handle_natural_language("clean up temp files", default_cfg, base_dir=tmp_path)

    from ohmyshell import audit_log as audit_log_module

    entries = audit_log_module.read_entries(base_dir=tmp_path)
    assert len(entries) == 1
    assert entries[0].status == "cancelled"
    assert entries[0].source == "natural_language"


def test_handle_natural_language_confirmed_runs_plan_and_logs_done(default_cfg, tmp_path):
    fake_result = _fake_parse_result(command="ps aux | grep chrome", risk="low")
    execution = ExecutionResult(
        action="ps aux | grep chrome",
        step_results=[
            StepResult(step_number=1, description="list", status=StepStatus.DONE, returncode=0, stdout="", stderr="")
        ],
    )

    def _fake_run_discussion(plan, **kwargs):
        return Confirmed(plan=plan)

    with patch("ohmyshell.main.parse_intent", return_value=fake_result):
        with patch("ohmyshell.main.run_discussion", side_effect=_fake_run_discussion):
            with patch("ohmyshell.main.run_plan", return_value=execution) as mock_run_plan:
                main_module._handle_natural_language(
                    "find chrome processes", default_cfg, base_dir=tmp_path
                )

    mock_run_plan.assert_called_once()
    from ohmyshell import audit_log as audit_log_module

    entries = audit_log_module.read_entries(base_dir=tmp_path)
    assert len(entries) == 1
    assert entries[0].status == "done"
    assert entries[0].action == "ps aux | grep chrome"
    assert entries[0].risk == "low"


def test_handle_natural_language_confirmed_never_calls_subprocess_directly(default_cfg, tmp_path):
    """main.py itself must not shell out for NL input -- only run_plan() may."""
    fake_result = _fake_parse_result(command="find /tmp -mtime +7 -delete", risk="medium")
    execution = ExecutionResult(
        action="find /tmp -mtime +7 -delete",
        step_results=[
            StepResult(step_number=1, description="clean", status=StepStatus.DONE, returncode=0, stdout="", stderr="")
        ],
    )

    def _fake_run_discussion(plan, **kwargs):
        return Confirmed(plan=plan)

    with patch("ohmyshell.main.parse_intent", return_value=fake_result):
        with patch("ohmyshell.main.run_discussion", side_effect=_fake_run_discussion):
            with patch("ohmyshell.main.run_plan", return_value=execution):
                with patch("ohmyshell.main.subprocess.run") as mock_run:
                    main_module._handle_natural_language(
                        "clean up temp files", default_cfg, base_dir=tmp_path
                    )
    mock_run.assert_not_called()


def test_handle_natural_language_interrupted_execution_logs_interrupted(default_cfg, tmp_path):
    fake_result = _fake_parse_result(command="find /tmp -mtime +7 -delete", risk="medium")
    execution = ExecutionResult(
        action="find /tmp -mtime +7 -delete",
        step_results=[
            StepResult(
                step_number=1, description="clean", status=StepStatus.INTERRUPTED,
                returncode=None, stdout="", stderr="",
            )
        ],
        interrupted=True,
    )

    def _fake_run_discussion(plan, **kwargs):
        return Confirmed(plan=plan)

    with patch("ohmyshell.main.parse_intent", return_value=fake_result):
        with patch("ohmyshell.main.run_discussion", side_effect=_fake_run_discussion):
            with patch("ohmyshell.main.run_plan", return_value=execution):
                main_module._handle_natural_language(
                    "clean up temp files", default_cfg, base_dir=tmp_path
                )

    from ohmyshell import audit_log as audit_log_module

    entries = audit_log_module.read_entries(base_dir=tmp_path)
    assert entries[0].status == "interrupted"


def test_handle_natural_language_aborted_for_sudo_logs_cancelled(default_cfg, tmp_path):
    fake_result = _fake_parse_result(command="find /tmp -mtime +7 -delete", risk="medium")
    execution = ExecutionResult(
        action="find /tmp -mtime +7 -delete",
        step_results=[
            StepResult(step_number=1, description="clean", status=StepStatus.INTERRUPTED, returncode=None)
        ],
        aborted_for_sudo=True,
    )

    def _fake_run_discussion(plan, **kwargs):
        return Confirmed(plan=plan)

    with patch("ohmyshell.main.parse_intent", return_value=fake_result):
        with patch("ohmyshell.main.run_discussion", side_effect=_fake_run_discussion):
            with patch("ohmyshell.main.run_plan", return_value=execution):
                main_module._handle_natural_language(
                    "clean up temp files", default_cfg, base_dir=tmp_path
                )

    from ohmyshell import audit_log as audit_log_module

    entries = audit_log_module.read_entries(base_dir=tmp_path)
    assert entries[0].status == "cancelled"


def test_handle_natural_language_failed_step_logs_failed(default_cfg, tmp_path):
    fake_result = _fake_parse_result(command="find /tmp -mtime +7 -delete", risk="medium")
    execution = ExecutionResult(
        action="find /tmp -mtime +7 -delete",
        step_results=[
            StepResult(
                step_number=1, description="clean", status=StepStatus.FAILED,
                returncode=1, stdout="", stderr="boom",
            )
        ],
    )

    def _fake_run_discussion(plan, **kwargs):
        return Confirmed(plan=plan)

    with patch("ohmyshell.main.parse_intent", return_value=fake_result):
        with patch("ohmyshell.main.run_discussion", side_effect=_fake_run_discussion):
            with patch("ohmyshell.main.run_plan", return_value=execution):
                main_module._handle_natural_language(
                    "clean up temp files", default_cfg, base_dir=tmp_path
                )

    from ohmyshell import audit_log as audit_log_module

    entries = audit_log_module.read_entries(base_dir=tmp_path)
    assert entries[0].status == "failed"
    assert "boom" in entries[0].detail


def test_handle_natural_language_passes_rich_sudo_prompt_to_run_plan(default_cfg, tmp_path):
    """A step needing sudo must be able to reach the Step 11 boxed sudo prompt."""
    fake_result = _fake_parse_result(command="find /tmp -mtime +7 -delete", risk="medium")
    execution = ExecutionResult(
        action="find /tmp -mtime +7 -delete",
        step_results=[StepResult(step_number=1, description="clean", status=StepStatus.DONE, returncode=0)],
    )

    def _fake_run_discussion(plan, **kwargs):
        return Confirmed(plan=plan)

    with patch("ohmyshell.main.parse_intent", return_value=fake_result):
        with patch("ohmyshell.main.run_discussion", side_effect=_fake_run_discussion):
            with patch("ohmyshell.main.run_plan", return_value=execution) as mock_run_plan:
                main_module._handle_natural_language(
                    "clean up temp files", default_cfg, base_dir=tmp_path
                )

    _, kwargs = mock_run_plan.call_args
    assert isinstance(kwargs["prompt"], RichSudoPrompt)


# --- Independent risk override integration (override_risk) -----------------------


def test_handle_natural_language_applies_independent_risk_override(default_cfg, tmp_path):
    """
    This session's own testing found models under-risking port-opening and
    passwordless-user-creation commands (see danger_classifier.py's own
    module docstring) -- _handle_natural_language must call override_risk()
    on the produced command before the plan ever reaches the user, and take
    the more severe verdict.
    """
    # A command matching override_risk's port-opening pattern, with the
    # model itself claiming "low" -- override_risk must force this to "high".
    fake_result = _fake_parse_result(command="ufw allow 22", risk="low", explanation="Open port 22.")

    captured_plans = []

    def _fake_run_discussion(plan, **kwargs):
        captured_plans.append(plan)
        return Cancelled()

    with patch("ohmyshell.main.parse_intent", return_value=fake_result):
        with patch("ohmyshell.main.run_discussion", side_effect=_fake_run_discussion):
            main_module._handle_natural_language("open port 22", default_cfg, base_dir=tmp_path)

    assert len(captured_plans) == 1
    assert captured_plans[0].risk == "high"  # overridden, not the model's own "low"


def test_handle_natural_language_does_not_override_when_model_risk_already_adequate(default_cfg, tmp_path):
    """override_risk() never LOWERS a risk either -- a benign, correctly
    low-risk command's plan must still show "low"."""
    fake_result = _fake_parse_result(command="ls -la", risk="low", explanation="List files.")

    captured_plans = []

    def _fake_run_discussion(plan, **kwargs):
        captured_plans.append(plan)
        return Cancelled()

    with patch("ohmyshell.main.parse_intent", return_value=fake_result):
        with patch("ohmyshell.main.run_discussion", side_effect=_fake_run_discussion):
            main_module._handle_natural_language("list files", default_cfg, base_dir=tmp_path)

    assert captured_plans[0].risk == "low"


def test_handle_natural_language_override_applies_on_chat_reparse_too(default_cfg, tmp_path):
    """
    The independent risk override must be re-applied on every chat-adjust
    reparse too, not just the initial parse -- otherwise a chat-adjust
    could "launder" an under-risked command past the override by simply
    triggering a second parse_intent() call. Drives the real
    discussion.run_discussion loop (not mocked) through
    _handle_natural_language's own choice_read/read plumbing, exactly like
    a real terminal session would: [c] chat -> adjustment text -> [Enter]
    confirm.
    """
    initial_result = _fake_parse_result(command="ls -la", risk="low", explanation="List files.")
    adjusted_result = _fake_parse_result(command="ufw allow 22", risk="low", explanation="Open port 22.")

    parse_calls = {"n": 0}

    def _fake_parse_intent(text, **kwargs):
        parse_calls["n"] += 1
        return initial_result if parse_calls["n"] == 1 else adjusted_result

    execution = ExecutionResult(
        action="ufw allow 22",
        step_results=[StepResult(step_number=1, description="x", status=StepStatus.DONE, returncode=0)],
    )

    choice_responses = iter(["c", ""])  # [c] chat-adjust, then bare Enter to confirm

    with patch("ohmyshell.main.parse_intent", side_effect=_fake_parse_intent):
        with patch("ohmyshell.main.run_plan", return_value=execution):
            main_module._handle_natural_language(
                "list files",
                default_cfg,
                read=lambda _: "open port 22 instead",
                choice_read=lambda _: next(choice_responses),
                base_dir=tmp_path,
            )

    assert parse_calls["n"] == 2  # initial parse + one chat-adjust reparse

    from ohmyshell import audit_log as audit_log_module

    entries = audit_log_module.read_entries(base_dir=tmp_path)
    assert len(entries) == 1
    # The FINAL plan (after the chat-adjust reparse) must reflect the
    # override, even though its own risk came back "low" from the model.
    assert entries[0].risk == "high"
    assert entries[0].action == "ufw allow 22"


# --- _repl_get_user_choice / _repl_edit_flow ------------------------------------------


def test_repl_get_user_choice_bare_enter_confirms():
    plan = _fake_plan()
    choice = main_module._repl_get_user_choice(plan, read=lambda _: "")
    assert choice == "confirm"


def test_repl_get_user_choice_maps_keys():
    plan = _fake_plan()
    assert main_module._repl_get_user_choice(plan, read=lambda _: "e") == "edit"
    assert main_module._repl_get_user_choice(plan, read=lambda _: "c") == "chat"
    assert main_module._repl_get_user_choice(plan, read=lambda _: "q") == "cancel"


def test_repl_get_user_choice_renders_plan_panel():
    plan = _fake_plan(command="find /tmp -mtime +7 -delete", risk="medium", explanation="Scan for old files")
    buffer, console = _buffer_console()
    main_module._repl_get_user_choice(plan, read=lambda _: "", console=console)
    assert "Scan for old files" in buffer.getvalue()
    assert "Confirm" in buffer.getvalue()


def test_repl_edit_flow_applies_edit():
    plan = _fake_plan(command="find /tmp -mtime +7 -delete", risk="medium")
    updated = main_module._repl_edit_flow(plan, read=lambda _: "find /tmp -mtime +30 -delete")
    assert updated.command == "find /tmp -mtime +30 -delete"


def test_repl_edit_flow_blank_input_leaves_plan_unchanged():
    plan = _fake_plan(command="find /tmp -mtime +7 -delete", risk="medium")
    updated = main_module._repl_edit_flow(plan, read=lambda _: "")
    assert updated == plan


# --- _extract_trash_target --------------------------------------------------------


def test_extract_trash_target_picks_trailing_path():
    assert main_module._extract_trash_target("rm -rf /tmp/build") == "/tmp/build"


def test_extract_trash_target_skips_flags():
    assert main_module._extract_trash_target("rm -rf -v /tmp/build") == "/tmp/build"


def test_extract_trash_target_none_for_empty_text():
    assert main_module._extract_trash_target("") is None


def test_extract_trash_target_none_when_only_flags_after_command():
    assert main_module._extract_trash_target("rm -rf") is None


# --- _handle_slash_command --------------------------------------------------------


def test_slash_exit_returns_true(default_cfg):
    assert main_module._handle_slash_command("/exit", default_cfg, 0.0) is True


def test_slash_quit_returns_true(default_cfg):
    assert main_module._handle_slash_command("/quit", default_cfg, 0.0) is True


def test_slash_help_is_fully_handled_by_meta_commands(default_cfg):
    buffer, console = _buffer_console()
    result = main_module._handle_slash_command("/help", default_cfg, 0.0, console=console)
    assert result is False
    assert "Command Reference" in buffer.getvalue()


def test_unrecognized_slash_command_prints_error_and_does_not_exit(default_cfg):
    buffer, console = _buffer_console()
    result = main_module._handle_slash_command("/totally-bogus", default_cfg, 0.0, console=console)
    assert result is False
    assert "Unrecognized command" in buffer.getvalue()


# --- run(): REPL loop integration ---------------------------------------------------


class _FakeSession:
    """Stands in for ui.session.ReplSession in run()'s REPL loop."""

    def __init__(self, responses):
        self._responses = iter(responses)

    def prompt(self, *_args, **_kwargs):
        try:
            return next(self._responses)
        except StopIteration:
            raise EOFError from None

    def __call__(self, *args, **kwargs):
        return self.prompt(*args, **kwargs)


def test_run_exits_cleanly_on_slash_exit():
    with patch("ohmyshell.main.ReplSession", return_value=_FakeSession(["/exit"])):
        with patch("ohmyshell.main.wizard_module.should_run_wizard", return_value=False):
            main_module.run()  # must return without raising


def test_run_exits_on_eof():
    with patch("ohmyshell.main.ReplSession", return_value=_FakeSession([])):
        with patch("ohmyshell.main.wizard_module.should_run_wizard", return_value=False):
            main_module.run()


def test_run_dispatches_raw_shell_then_exits(tmp_path, monkeypatch):
    monkeypatch.setattr(config_module, "CONFIG_DIR", tmp_path)
    with patch("ohmyshell.main.ReplSession", return_value=_FakeSession(["ls -la", "/exit"])):
        with patch("ohmyshell.main.wizard_module.should_run_wizard", return_value=False):
            with patch("ohmyshell.main.subprocess.run") as mock_run:
                main_module.run()
    mock_run.assert_called_once_with("ls -la", shell=True)


def test_run_invokes_wizard_on_genuine_first_run(tmp_path, monkeypatch):
    monkeypatch.setattr(config_module, "CONFIG_DIR", tmp_path)
    monkeypatch.setattr(config_module, "CONFIG_PATH", tmp_path / "config.json")
    assert not (tmp_path / "config.json").exists()

    with patch("ohmyshell.main.ReplSession", return_value=_FakeSession(["/exit"])):
        with patch("ohmyshell.main.wizard_module.run_wizard") as mock_wizard:
            main_module.run()

    mock_wizard.assert_called_once()


def test_run_skips_wizard_when_config_already_exists(tmp_path, monkeypatch):
    monkeypatch.setattr(config_module, "CONFIG_DIR", tmp_path)
    config_path = tmp_path / "config.json"
    monkeypatch.setattr(config_module, "CONFIG_PATH", config_path)
    config_path.write_text("{}", encoding="utf-8")

    with patch("ohmyshell.main.ReplSession", return_value=_FakeSession(["/exit"])):
        with patch("ohmyshell.main.wizard_module.run_wizard") as mock_wizard:
            main_module.run()

    mock_wizard.assert_not_called()


def test_run_survives_ctrl_c_at_a_mid_command_confirmation_prompt(tmp_path, monkeypatch):
    monkeypatch.setattr(config_module, "CONFIG_DIR", tmp_path)

    class _RaisesOnConfirm:
        def __init__(self, repl_inputs):
            self._repl_inputs = iter(repl_inputs)

        def prompt(self, *_args, **_kwargs):
            try:
                return next(self._repl_inputs)
            except StopIteration:
                raise EOFError from None

        def __call__(self, *_args, **_kwargs):
            raise KeyboardInterrupt

    fake_session = _RaisesOnConfirm(["rm -rf /tmp/somedir", "/exit"])

    with patch("ohmyshell.main.ReplSession", return_value=fake_session):
        with patch("ohmyshell.main.wizard_module.should_run_wizard", return_value=False):
            with patch("ohmyshell.main.subprocess.run") as mock_run:
                main_module.run()  # must return normally, not raise

    mock_run.assert_not_called()


# --- Removed / superseded tests --------------------------------------------------
#
# test_run_exits_process_if_registry_fails_to_load, and every `registry`
# fixture/argument in this file, are gone: run() no longer loads a registry
# at all (see main.py's own module docstring: "run() no longer loads a
# registry at all"), so there is no registry-load failure path left to
# test. test_handle_natural_language_edit_then_confirm_executes_edited_plan
# was translated into test_repl_edit_flow_applies_edit (edit is now a
# direct command-text replacement, not a param sub-flow) plus the discussion
# loop's own edit-reaches-final-plan coverage in test_discussion.py.