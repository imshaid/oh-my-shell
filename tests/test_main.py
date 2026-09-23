"""
Tests for main.py (Build Order Step 6, fully wired post-Build-Order to the
Section 4.1 pipeline, then to full `rich`/`prompt_toolkit` visual polish —
Step 11).

`input()`/`ReplSession`, `subprocess.run`, `intent_parser.parse_intent`, and
the Executor/Audit Log are all mocked or redirected to a tmp_path base_dir
so these tests exercise only main.py's own wiring logic, not real shell
execution, a real Ollama call, or the user's real ~/.oh-my-shell/.

Rich-rendered output (panels, the prompt, streaming) is asserted against an
injected `Console(file=io.StringIO(), force_terminal=False)` buffer's
rendered text, the same pattern ui/panels.py's and ui/streaming.py's own
test suites already use -- a plain substring check against the buffer
content, exactly like the old plain-text tests did against capsys, just
reading from the injected Console instead of stdout.
"""

from __future__ import annotations

import io
from unittest.mock import MagicMock, patch

import pytest
from rich.console import Console

from ohmyshell import config as config_module
from ohmyshell import main as main_module
from ohmyshell import registry as registry_module
from ohmyshell.discussion import Cancelled, Confirmed
from ohmyshell.executor import ExecutionResult, StepResult, StepStatus
from ohmyshell.intent_parser import IntentParseError, ParseResult
from ohmyshell.plan_generator import Plan
from ohmyshell.ui.panels import RichSudoPrompt
from ohmyshell.ui.prompt import render_prompt_ansi
from ohmyshell.validation import ValidatedIntent


@pytest.fixture
def registry():
    return registry_module.load()


@pytest.fixture
def default_cfg():
    return config_module.default_config()


def _buffer_console() -> tuple[io.StringIO, Console]:
    buffer = io.StringIO()
    console = Console(file=buffer, width=100, force_terminal=False)
    return buffer, console


# --- ui/prompt.render_prompt_ansi (main.py's prompt source) ------------------------
#
# main.py's REPL loop calls ui.prompt.render_prompt_ansi directly (there is
# no more main.py-local _render_prompt -- see main.py's own module
# docstring for why). These tests exercise it through the same import path
# main.py uses, confirming the wiring is live, while the exhaustive
# content/behavior assertions already live in test_ui_prompt.py.


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


def _fake_parse_result(action="list_processes", params=None, risk="low"):
    return ParseResult(
        action=action,
        intent=ValidatedIntent(action=action, params=params or {}, risk=risk),
        attempts=1,
    )


def _fake_plan(action="list_processes", params=None, risk="low", steps=None):
    return Plan(action=action, params=params or {}, risk=risk, steps=steps or ["do the thing"])


def test_handle_natural_language_reports_unmapped(registry, default_cfg, tmp_path):
    fake_result = ParseResult(action="unmapped", intent=None, attempts=2, last_error="nope")
    buffer, console = _buffer_console()
    with patch("ohmyshell.main.parse_intent", return_value=fake_result):
        main_module._handle_natural_language(
            "do something weird", registry, default_cfg, console=console, base_dir=tmp_path
        )
    assert "couldn't map" in buffer.getvalue().lower()


def test_handle_natural_language_reports_backend_failure(registry, default_cfg, tmp_path):
    buffer, console = _buffer_console()
    with patch("ohmyshell.main.parse_intent", side_effect=IntentParseError("Ollama unreachable")):
        main_module._handle_natural_language(
            "clean up temp files", registry, default_cfg, console=console, base_dir=tmp_path
        )
    assert "Ollama unreachable" in buffer.getvalue()


def test_handle_natural_language_cancel_never_executes_anything(registry, default_cfg, tmp_path):
    """Cancelling at the Discussion Loop must not touch subprocess/run_plan at all."""
    fake_result = _fake_parse_result(action="clean_temp_files", params={"days": 7}, risk="medium")
    fake_plan = _fake_plan(action="clean_temp_files", params={"days": 7}, risk="medium")

    with patch("ohmyshell.main.parse_intent", return_value=fake_result):
        with patch("ohmyshell.main.generate_plan", return_value=fake_plan):
            with patch("ohmyshell.main.run_discussion", return_value=Cancelled()):
                with patch("ohmyshell.main.run_plan") as mock_run_plan:
                    main_module._handle_natural_language(
                        "clean up temp files", registry, default_cfg, base_dir=tmp_path
                    )
    mock_run_plan.assert_not_called()


def test_handle_natural_language_cancel_logs_cancelled_status(registry, default_cfg, tmp_path):
    fake_result = _fake_parse_result(action="clean_temp_files", params={"days": 7}, risk="medium")
    fake_plan = _fake_plan(action="clean_temp_files", params={"days": 7}, risk="medium")

    with patch("ohmyshell.main.parse_intent", return_value=fake_result):
        with patch("ohmyshell.main.generate_plan", return_value=fake_plan):
            with patch("ohmyshell.main.run_discussion", return_value=Cancelled()):
                main_module._handle_natural_language("clean up temp files", registry, default_cfg, base_dir=tmp_path)

    from ohmyshell import audit_log as audit_log_module

    entries = audit_log_module.read_entries(base_dir=tmp_path)
    assert len(entries) == 1
    assert entries[0].status == "cancelled"
    assert entries[0].source == "natural_language"


def test_handle_natural_language_confirmed_runs_plan_and_logs_done(registry, default_cfg, tmp_path):
    fake_result = _fake_parse_result(action="list_processes", params={"filter": "chrome"}, risk="low")
    fake_plan = _fake_plan(action="list_processes", params={"filter": "chrome"}, risk="low")
    execution = ExecutionResult(
        action="list_processes",
        step_results=[
            StepResult(step_number=1, description="list", status=StepStatus.DONE, returncode=0, stdout="", stderr="")
        ],
    )

    with patch("ohmyshell.main.parse_intent", return_value=fake_result):
        with patch("ohmyshell.main.generate_plan", return_value=fake_plan):
            with patch("ohmyshell.main.run_discussion", return_value=Confirmed(plan=fake_plan)):
                with patch("ohmyshell.main.run_plan", return_value=execution) as mock_run_plan:
                    main_module._handle_natural_language(
                        "find chrome processes", registry, default_cfg, base_dir=tmp_path
                    )

    mock_run_plan.assert_called_once()
    from ohmyshell import audit_log as audit_log_module

    entries = audit_log_module.read_entries(base_dir=tmp_path)
    assert len(entries) == 1
    assert entries[0].status == "done"
    assert entries[0].action == "list_processes"
    assert entries[0].risk == "low"


def test_handle_natural_language_confirmed_never_calls_subprocess_directly(registry, default_cfg, tmp_path):
    """main.py itself must not shell out for NL input -- only run_plan() may."""
    fake_result = _fake_parse_result(action="clean_temp_files", params={"days": 7}, risk="medium")
    fake_plan = _fake_plan(action="clean_temp_files", params={"days": 7}, risk="medium")
    execution = ExecutionResult(
        action="clean_temp_files",
        step_results=[
            StepResult(step_number=1, description="clean", status=StepStatus.DONE, returncode=0, stdout="", stderr="")
        ],
    )

    with patch("ohmyshell.main.parse_intent", return_value=fake_result):
        with patch("ohmyshell.main.generate_plan", return_value=fake_plan):
            with patch("ohmyshell.main.run_discussion", return_value=Confirmed(plan=fake_plan)):
                with patch("ohmyshell.main.run_plan", return_value=execution):
                    with patch("ohmyshell.main.subprocess.run") as mock_run:
                        main_module._handle_natural_language(
                            "clean up temp files", registry, default_cfg, base_dir=tmp_path
                        )
    mock_run.assert_not_called()


def test_handle_natural_language_interrupted_execution_logs_interrupted(registry, default_cfg, tmp_path):
    fake_result = _fake_parse_result(action="clean_temp_files", params={"days": 7}, risk="medium")
    fake_plan = _fake_plan(action="clean_temp_files", params={"days": 7}, risk="medium")
    execution = ExecutionResult(
        action="clean_temp_files",
        step_results=[
            StepResult(
                step_number=1, description="clean", status=StepStatus.INTERRUPTED,
                returncode=None, stdout="", stderr="",
            )
        ],
        interrupted=True,
    )

    with patch("ohmyshell.main.parse_intent", return_value=fake_result):
        with patch("ohmyshell.main.generate_plan", return_value=fake_plan):
            with patch("ohmyshell.main.run_discussion", return_value=Confirmed(plan=fake_plan)):
                with patch("ohmyshell.main.run_plan", return_value=execution):
                    main_module._handle_natural_language(
                        "clean up temp files", registry, default_cfg, base_dir=tmp_path
                    )

    from ohmyshell import audit_log as audit_log_module

    entries = audit_log_module.read_entries(base_dir=tmp_path)
    assert entries[0].status == "interrupted"


def test_handle_natural_language_aborted_for_sudo_logs_cancelled(registry, default_cfg, tmp_path):
    fake_result = _fake_parse_result(action="clean_temp_files", params={"days": 7}, risk="medium")
    fake_plan = _fake_plan(action="clean_temp_files", params={"days": 7}, risk="medium")
    execution = ExecutionResult(
        action="clean_temp_files",
        step_results=[
            StepResult(step_number=1, description="clean", status=StepStatus.INTERRUPTED, returncode=None)
        ],
        aborted_for_sudo=True,
    )

    with patch("ohmyshell.main.parse_intent", return_value=fake_result):
        with patch("ohmyshell.main.generate_plan", return_value=fake_plan):
            with patch("ohmyshell.main.run_discussion", return_value=Confirmed(plan=fake_plan)):
                with patch("ohmyshell.main.run_plan", return_value=execution):
                    main_module._handle_natural_language(
                        "clean up temp files", registry, default_cfg, base_dir=tmp_path
                    )

    from ohmyshell import audit_log as audit_log_module

    entries = audit_log_module.read_entries(base_dir=tmp_path)
    assert entries[0].status == "cancelled"


def test_handle_natural_language_failed_step_logs_failed(registry, default_cfg, tmp_path):
    fake_result = _fake_parse_result(action="clean_temp_files", params={"days": 7}, risk="medium")
    fake_plan = _fake_plan(action="clean_temp_files", params={"days": 7}, risk="medium")
    execution = ExecutionResult(
        action="clean_temp_files",
        step_results=[
            StepResult(
                step_number=1, description="clean", status=StepStatus.FAILED,
                returncode=1, stdout="", stderr="boom",
            )
        ],
    )

    with patch("ohmyshell.main.parse_intent", return_value=fake_result):
        with patch("ohmyshell.main.generate_plan", return_value=fake_plan):
            with patch("ohmyshell.main.run_discussion", return_value=Confirmed(plan=fake_plan)):
                with patch("ohmyshell.main.run_plan", return_value=execution):
                    main_module._handle_natural_language(
                        "clean up temp files", registry, default_cfg, base_dir=tmp_path
                    )

    from ohmyshell import audit_log as audit_log_module

    entries = audit_log_module.read_entries(base_dir=tmp_path)
    assert entries[0].status == "failed"
    assert "boom" in entries[0].detail


def test_handle_natural_language_passes_rich_sudo_prompt_to_run_plan(registry, default_cfg, tmp_path):
    """A step needing sudo must be able to reach the Step 11 boxed sudo prompt."""
    fake_result = _fake_parse_result(action="clean_temp_files", params={"days": 7}, risk="medium")
    fake_plan = _fake_plan(action="clean_temp_files", params={"days": 7}, risk="medium")
    execution = ExecutionResult(
        action="clean_temp_files",
        step_results=[StepResult(step_number=1, description="clean", status=StepStatus.DONE, returncode=0)],
    )

    with patch("ohmyshell.main.parse_intent", return_value=fake_result):
        with patch("ohmyshell.main.generate_plan", return_value=fake_plan):
            with patch("ohmyshell.main.run_discussion", return_value=Confirmed(plan=fake_plan)):
                with patch("ohmyshell.main.run_plan", return_value=execution) as mock_run_plan:
                    main_module._handle_natural_language(
                        "clean up temp files", registry, default_cfg, base_dir=tmp_path
                    )

    _, kwargs = mock_run_plan.call_args
    assert isinstance(kwargs["prompt"], RichSudoPrompt)


def test_handle_natural_language_edit_then_confirm_executes_edited_plan(registry, default_cfg, tmp_path):
    """
    End-to-end regression test for the bug found via manual testing: typing
    [e] Edit, changing a param, then confirming must execute the EDITED
    plan, not the original one. This exercises the real run_discussion (not
    mocked) through _handle_natural_language's own _get_user_choice glue,
    the same path a real terminal session drives.
    """
    fake_result = _fake_parse_result(action="clean_temp_files", params={"days": 7}, risk="medium")
    fake_plan = _fake_plan(action="clean_temp_files", params={"days": 7}, risk="medium", steps=["Scan for files older than 7 days"])

    # Scripted input: [e] edit -> param name "days" -> new value "14" -> then bare Enter to confirm.
    # Split across two fakes since _get_user_choice now reads the plan
    # choice ("e", then "") via its own `choice_read` (see the Esc-bug-fix
    # docstring on _repl_get_user_choice) while the edit sub-flow's own
    # param-name/value prompts still go through the ordinary `read`.
    choice_responses = iter(["e", ""])
    edit_responses = iter(["days", "14"])
    captured_plans = []

    def _fake_run_plan(plan, reg, **kwargs):
        captured_plans.append(plan)
        return ExecutionResult(
            action=plan.action,
            step_results=[StepResult(step_number=1, description="clean", status=StepStatus.DONE, returncode=0)],
        )

    _, console = _buffer_console()
    with patch("ohmyshell.main.parse_intent", return_value=fake_result):
        with patch("ohmyshell.main.generate_plan", return_value=fake_plan):
            with patch("ohmyshell.main.run_plan", side_effect=_fake_run_plan):
                main_module._handle_natural_language(
                    "clean up temp files",
                    registry,
                    default_cfg,
                    read=lambda _: next(edit_responses),
                    choice_read=lambda _: next(choice_responses),
                    console=console,
                    base_dir=tmp_path,
                )

    assert len(captured_plans) == 1
    assert captured_plans[0].params["days"] == "14"


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
    plan = _fake_plan(action="clean_temp_files", params={"days": 7}, risk="medium", steps=["Scan for old files"])
    buffer, console = _buffer_console()
    main_module._repl_get_user_choice(plan, read=lambda _: "", console=console)
    assert "Scan for old files" in buffer.getvalue()
    assert "Confirm" in buffer.getvalue()


def test_repl_edit_flow_applies_edit(registry):
    plan = _fake_plan(action="clean_temp_files", params={"days": 7}, risk="medium")
    responses = iter(["days", "14"])
    updated = main_module._repl_edit_flow(plan, registry, read=lambda _: next(responses))
    assert updated.params["days"] == "14"


def test_repl_edit_flow_unknown_param_leaves_plan_unchanged(registry):
    plan = _fake_plan(action="clean_temp_files", params={"days": 7}, risk="medium")
    responses = iter(["not_a_real_param", "whatever"])
    updated = main_module._repl_edit_flow(plan, registry, read=lambda _: next(responses))
    assert updated == plan


# --- _extract_trash_target --------------------------------------------------------


def test_extract_trash_target_picks_trailing_path():
    assert main_module._extract_trash_target("rm -rf /tmp/build") == "/tmp/build"


def test_extract_trash_target_skips_flags():
    assert main_module._extract_trash_target("rm -rf -v /tmp/build") == "/tmp/build"


def test_extract_trash_target_none_for_empty_text():
    assert main_module._extract_trash_target("") is None


def test_extract_trash_target_none_when_only_flags_after_command():
    # "rm -rf" alone (no path argument) has no real target -- the command
    # word itself ("rm") is deliberately never returned as a fallback (see
    # _extract_trash_target's own docstring for why).
    assert main_module._extract_trash_target("rm -rf") is None


# --- _handle_slash_command --------------------------------------------------------


def test_slash_exit_returns_true(registry, default_cfg):
    assert main_module._handle_slash_command("/exit", registry, default_cfg, 0.0) is True


def test_slash_quit_returns_true(registry, default_cfg):
    assert main_module._handle_slash_command("/quit", registry, default_cfg, 0.0) is True


def test_slash_help_is_now_fully_handled_by_meta_commands(registry, default_cfg):
    # Step 14 wires the full Meta-Command Handler in — /help is a real,
    # recognized command now (not the Step 6-era placeholder), so this
    # returns False (don't exit) and prints the actual command reference.
    buffer, console = _buffer_console()
    result = main_module._handle_slash_command("/help", registry, default_cfg, 0.0, console=console)
    assert result is False
    assert "Command Reference" in buffer.getvalue()


def test_unrecognized_slash_command_prints_error_and_does_not_exit(registry, default_cfg):
    buffer, console = _buffer_console()
    result = main_module._handle_slash_command("/totally-bogus", registry, default_cfg, 0.0, console=console)
    assert result is False
    assert "Unrecognized command" in buffer.getvalue()


# --- run(): REPL loop integration ---------------------------------------------------
#
# run() now reads via a ui.session.ReplSession instance instead of the bare
# `input` builtin, so these tests patch ohmyshell.main.ReplSession to return
# a fake session whose .prompt() is scripted -- the REPL-loop-level
# equivalent of the old `patch("ohmyshell.main.input", side_effect=[...])`.


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
        with patch("ohmyshell.main.load_registry"):
            with patch("ohmyshell.main.wizard_module.should_run_wizard", return_value=False):
                main_module.run()  # must return without raising


def test_run_exits_on_eof():
    with patch("ohmyshell.main.ReplSession", return_value=_FakeSession([])):
        with patch("ohmyshell.main.load_registry"):
            with patch("ohmyshell.main.wizard_module.should_run_wizard", return_value=False):
                main_module.run()


def test_run_dispatches_raw_shell_then_exits(tmp_path, monkeypatch):
    # Isolate audit_log's default base_dir (config_module.CONFIG_DIR) so this
    # doesn't append to the real ~/.oh-my-shell/audit.log.jsonl -- run()
    # itself calls _handle_raw_shell without an explicit base_dir, matching
    # real usage, so the isolation has to happen at the config-dir level.
    monkeypatch.setattr(config_module, "CONFIG_DIR", tmp_path)
    with patch("ohmyshell.main.ReplSession", return_value=_FakeSession(["ls -la", "/exit"])):
        with patch("ohmyshell.main.load_registry"):
            with patch("ohmyshell.main.wizard_module.should_run_wizard", return_value=False):
                with patch("ohmyshell.main.subprocess.run") as mock_run:
                    main_module.run()
    mock_run.assert_called_once_with("ls -la", shell=True)


def test_run_invokes_wizard_on_genuine_first_run(tmp_path, monkeypatch):
    """
    Bug fix (found via manual end-to-end testing, post-Build-Order):
    wizard.py (Build Order Step 13) was fully written and tested but run()
    never actually called it -- it went straight to config_module.load(),
    which silently creates a static-default config.json with no wizard
    involved. wizard.py's own should_run_wizard() docstring already
    documented the expectation this violated. This test confirms run()
    now calls should_run_wizard()/run_wizard() before config_module.load()
    on a genuine first run (no config.json on disk yet).
    """
    monkeypatch.setattr(config_module, "CONFIG_DIR", tmp_path)
    monkeypatch.setattr(config_module, "CONFIG_PATH", tmp_path / "config.json")
    assert not (tmp_path / "config.json").exists()

    with patch("ohmyshell.main.ReplSession", return_value=_FakeSession(["/exit"])):
        with patch("ohmyshell.main.load_registry"):
            with patch("ohmyshell.main.wizard_module.run_wizard") as mock_wizard:
                main_module.run()

    mock_wizard.assert_called_once()


def test_run_skips_wizard_when_config_already_exists(tmp_path, monkeypatch):
    """Complementary case: an existing config.json means this isn't a first
    run, so the wizard must NOT run (it would clobber the user's config)."""
    monkeypatch.setattr(config_module, "CONFIG_DIR", tmp_path)
    config_path = tmp_path / "config.json"
    monkeypatch.setattr(config_module, "CONFIG_PATH", config_path)
    config_path.write_text("{}", encoding="utf-8")

    with patch("ohmyshell.main.ReplSession", return_value=_FakeSession(["/exit"])):
        with patch("ohmyshell.main.load_registry"):
            with patch("ohmyshell.main.wizard_module.run_wizard") as mock_wizard:
                main_module.run()

    mock_wizard.assert_not_called()


def test_run_exits_process_if_registry_fails_to_load(capsys):
    from ohmyshell.registry import RegistryError

    with patch("ohmyshell.main.load_registry", side_effect=RegistryError("missing file")):
        with pytest.raises(SystemExit) as exc_info:
            main_module.run()
    assert exc_info.value.code == 1
    assert "missing file" in capsys.readouterr().out