"""
Tests for main.py (Build Order Step 6, fully wired post-Build-Order to the
Section 4.1 pipeline) — REPL loop dispatch and the NL/raw-shell glue.

`input()`, `subprocess.run`, `intent_parser.parse_intent`, and the
Executor/Audit Log are all mocked or redirected to a tmp_path base_dir so
these tests exercise only main.py's own wiring logic, not real shell
execution, a real Ollama call, or the user's real ~/.oh-my-shell/.
"""

from unittest.mock import MagicMock, patch

import pytest

from ohmyshell import config as config_module
from ohmyshell import main as main_module
from ohmyshell import registry as registry_module
from ohmyshell.discussion import Cancelled, Confirmed
from ohmyshell.executor import ExecutionResult, StepResult, StepStatus
from ohmyshell.intent_parser import IntentParseError, ParseResult
from ohmyshell.plan_generator import Plan
from ohmyshell.validation import ValidatedIntent


@pytest.fixture
def registry():
    return registry_module.load()


@pytest.fixture
def default_cfg():
    return config_module.default_config()


# --- _render_prompt --------------------------------------------------------------


def test_render_prompt_shows_folder_name_only(default_cfg, tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    prompt = main_module._render_prompt(default_cfg)
    assert tmp_path.name in prompt
    assert str(tmp_path) not in prompt  # full path must not appear, only the folder name


def test_render_prompt_no_tag_for_default_model(default_cfg):
    prompt = main_module._render_prompt(default_cfg)
    assert "(" not in prompt


def test_render_prompt_shows_tag_for_non_default_model(default_cfg):
    cfg = dict(default_cfg)
    cfg["model"] = {**default_cfg["model"], "active": "qwen3.5:4b"}
    prompt = main_module._render_prompt(cfg)
    assert "(qwen3.5:4b)" in prompt


def test_render_prompt_ends_with_raw_command_icon(default_cfg):
    """Section 8.3.1: icon-swap-on-AI-request needs live redraw (Step 11) — always ❯ for now."""
    prompt = main_module._render_prompt(default_cfg)
    assert prompt.rstrip().endswith("❯")


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


def test_handle_raw_shell_destructive_verdict_shows_explanation(default_cfg, tmp_path, capsys):
    from ohmyshell.danger_classifier import ClassificationResult, Destructive

    destructive = ClassificationResult(
        verdict=Destructive(explanation="this will delete everything", trash_alternative_possible=True),
        source="regex",
    )
    with patch("ohmyshell.main.classify", return_value=destructive):
        with patch("ohmyshell.main.subprocess.run"):
            main_module._handle_raw_shell("rm -rf /tmp/x", default_cfg, confirm=lambda _: "n", base_dir=tmp_path)
    out = capsys.readouterr().out
    assert "this will delete everything" in out


def test_handle_raw_shell_classifier_error_does_not_run_command(default_cfg, tmp_path, capsys):
    from ohmyshell.danger_classifier import DangerClassifierError

    with patch("ohmyshell.main.classify", side_effect=DangerClassifierError("Ollama unreachable")):
        with patch("ohmyshell.main.subprocess.run") as mock_run:
            main_module._handle_raw_shell("some ambiguous command", default_cfg, base_dir=tmp_path)
    mock_run.assert_not_called()
    out = capsys.readouterr().out
    assert "Ollama unreachable" in out


def test_handle_raw_shell_destructive_verdict_offers_trash_option_when_possible(default_cfg, tmp_path, capsys):
    from ohmyshell.danger_classifier import ClassificationResult, Destructive

    destructive = ClassificationResult(
        verdict=Destructive(explanation="dangerous", trash_alternative_possible=True),
        source="regex",
    )
    with patch("ohmyshell.main.classify", return_value=destructive):
        with patch("ohmyshell.main.subprocess.run"):
            main_module._handle_raw_shell("rm -rf /tmp/x", default_cfg, confirm=lambda _: "n", base_dir=tmp_path)
    out = capsys.readouterr().out
    assert "[t] Move to trash instead" in out


def test_handle_raw_shell_destructive_verdict_omits_trash_option_when_not_possible(default_cfg, tmp_path, capsys):
    from ohmyshell.danger_classifier import ClassificationResult, Destructive

    destructive = ClassificationResult(
        verdict=Destructive(explanation="wipes a device", trash_alternative_possible=False),
        source="regex",
    )
    with patch("ohmyshell.main.classify", return_value=destructive):
        with patch("ohmyshell.main.subprocess.run"):
            main_module._handle_raw_shell("dd if=/dev/zero of=/dev/sda", default_cfg, confirm=lambda _: "n", base_dir=tmp_path)
    out = capsys.readouterr().out
    assert "[t]" not in out


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


def test_handle_natural_language_reports_unmapped(registry, default_cfg, tmp_path, capsys):
    fake_result = ParseResult(action="unmapped", intent=None, attempts=2, last_error="nope")
    with patch("ohmyshell.main.parse_intent", return_value=fake_result):
        main_module._handle_natural_language("do something weird", registry, default_cfg, base_dir=tmp_path)

    out = capsys.readouterr().out
    assert "couldn't map" in out.lower()


def test_handle_natural_language_reports_backend_failure(registry, default_cfg, tmp_path, capsys):
    with patch("ohmyshell.main.parse_intent", side_effect=IntentParseError("Ollama unreachable")):
        main_module._handle_natural_language("clean up temp files", registry, default_cfg, base_dir=tmp_path)

    out = capsys.readouterr().out
    assert "Ollama unreachable" in out


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


def test_handle_natural_language_passes_input_prompt_to_run_plan(registry, default_cfg, tmp_path):
    """A step needing sudo must be able to reach sudo_layer's real REPL prompt."""
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

    from ohmyshell.sudo_layer import InputPrompt

    _, kwargs = mock_run_plan.call_args
    assert isinstance(kwargs["prompt"], InputPrompt)

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
    responses = iter(["e", "days", "14", ""])
    captured_plans = []

    def _fake_run_plan(plan, reg, **kwargs):
        captured_plans.append(plan)
        return ExecutionResult(
            action=plan.action,
            step_results=[StepResult(step_number=1, description="clean", status=StepStatus.DONE, returncode=0)],
        )

    with patch("ohmyshell.main.parse_intent", return_value=fake_result):
        with patch("ohmyshell.main.generate_plan", return_value=fake_plan):
            with patch("ohmyshell.main.run_plan", side_effect=_fake_run_plan):
                main_module._handle_natural_language(
                    "clean up temp files",
                    registry,
                    default_cfg,
                    read=lambda _: next(responses),
                    print_fn=lambda _: None,
                    base_dir=tmp_path,
                )

    assert len(captured_plans) == 1
    assert captured_plans[0].params["days"] == "14"

    from ohmyshell import audit_log as audit_log_module

    entries = audit_log_module.read_entries(base_dir=tmp_path)
    assert entries[0].params["days"] == "14"


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


def test_repl_edit_flow_applies_edit(registry):
    plan = _fake_plan(action="clean_temp_files", params={"days": 7}, risk="medium")
    responses = iter(["days", "14"])
    updated = main_module._repl_edit_flow(plan, registry, read=lambda _: next(responses), print_fn=lambda _: None)
    assert updated.params["days"] == "14"


def test_repl_edit_flow_unknown_param_leaves_plan_unchanged(registry):
    plan = _fake_plan(action="clean_temp_files", params={"days": 7}, risk="medium")
    responses = iter(["not_a_real_param", "whatever"])
    updated = main_module._repl_edit_flow(plan, registry, read=lambda _: next(responses), print_fn=lambda _: None)
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


def test_slash_help_is_now_fully_handled_by_meta_commands(registry, default_cfg, capsys):
    # Step 14 wires the full Meta-Command Handler in — /help is a real,
    # recognized command now (not the Step 6-era placeholder), so this
    # returns False (don't exit) and prints the actual command reference.
    result = main_module._handle_slash_command("/help", registry, default_cfg, 0.0)
    out = capsys.readouterr().out
    assert result is False
    assert "Command Reference" in out


def test_unrecognized_slash_command_prints_error_and_does_not_exit(registry, default_cfg, capsys):
    result = main_module._handle_slash_command("/totally-bogus", registry, default_cfg, 0.0)
    out = capsys.readouterr().out
    assert result is False
    assert "Unrecognized command" in out


# --- run(): REPL loop integration ---------------------------------------------------


def test_run_exits_cleanly_on_slash_exit():
    with patch("ohmyshell.main.input", side_effect=["/exit"]):
        with patch("ohmyshell.main.load_registry"):
            main_module.run()  # must return without raising


def test_run_exits_on_eof():
    with patch("ohmyshell.main.input", side_effect=EOFError):
        with patch("ohmyshell.main.load_registry"):
            main_module.run()


def test_run_dispatches_raw_shell_then_exits(tmp_path, monkeypatch):
    # Isolate audit_log's default base_dir (config_module.CONFIG_DIR) so this
    # doesn't append to the real ~/.oh-my-shell/audit.log.jsonl -- run()
    # itself calls _handle_raw_shell without an explicit base_dir, matching
    # real usage, so the isolation has to happen at the config-dir level.
    monkeypatch.setattr(config_module, "CONFIG_DIR", tmp_path)
    with patch("ohmyshell.main.input", side_effect=["ls -la", "/exit"]):
        with patch("ohmyshell.main.load_registry"):
            with patch("ohmyshell.main.subprocess.run") as mock_run:
                main_module.run()
    mock_run.assert_called_once_with("ls -la", shell=True)


def test_run_exits_process_if_registry_fails_to_load(capsys):
    from ohmyshell.registry import RegistryError

    with patch("ohmyshell.main.load_registry", side_effect=RegistryError("missing file")):
        with pytest.raises(SystemExit) as exc_info:
            main_module.run()
    assert exc_info.value.code == 1
    assert "missing file" in capsys.readouterr().err