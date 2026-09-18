"""
Tests for main.py (Build Order Step 6) — basic REPL loop dispatch.

`input()`, `subprocess.run`, and `intent_parser.parse_intent` are all mocked
so these tests exercise only the REPL's own dispatch logic, not real shell
execution or a real Ollama call.
"""

from unittest.mock import MagicMock, patch

import pytest

from ohmyshell import config as config_module
from ohmyshell import main as main_module
from ohmyshell import registry as registry_module
from ohmyshell.intent_parser import IntentParseError, ParseResult
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


def test_handle_raw_shell_calls_subprocess_run_with_shell_true(default_cfg):
    with patch("ohmyshell.main.classify") as mock_classify:
        from ohmyshell.danger_classifier import ClassificationResult, Safe

        mock_classify.return_value = ClassificationResult(verdict=Safe(), source="regex")
        with patch("ohmyshell.main.subprocess.run") as mock_run:
            main_module._handle_raw_shell("ls -la", default_cfg)
    mock_run.assert_called_once_with("ls -la", shell=True)


def test_handle_raw_shell_reports_os_error_without_raising(default_cfg, capsys):
    with patch("ohmyshell.main.classify") as mock_classify:
        from ohmyshell.danger_classifier import ClassificationResult, Safe

        mock_classify.return_value = ClassificationResult(verdict=Safe(), source="regex")
        with patch("ohmyshell.main.subprocess.run", side_effect=OSError("boom")):
            main_module._handle_raw_shell("whatever", default_cfg)  # must not raise
    captured = capsys.readouterr()
    assert "boom" in captured.err


def test_handle_raw_shell_safe_verdict_never_prompts(default_cfg):
    from ohmyshell.danger_classifier import ClassificationResult, Safe

    with patch("ohmyshell.main.classify", return_value=ClassificationResult(verdict=Safe(), source="regex")):
        with patch("ohmyshell.main.subprocess.run"):
            confirm_fn = MagicMock()
            main_module._handle_raw_shell("ls -la", default_cfg, confirm=confirm_fn)
    confirm_fn.assert_not_called()


def test_handle_raw_shell_destructive_verdict_prompts_and_runs_on_yes(default_cfg):
    from ohmyshell.danger_classifier import ClassificationResult, Destructive

    destructive = ClassificationResult(
        verdict=Destructive(explanation="dangerous", trash_alternative_possible=True),
        source="regex",
    )
    with patch("ohmyshell.main.classify", return_value=destructive):
        with patch("ohmyshell.main.subprocess.run") as mock_run:
            main_module._handle_raw_shell("rm -rf /tmp/x", default_cfg, confirm=lambda _: "y")
    mock_run.assert_called_once_with("rm -rf /tmp/x", shell=True)


def test_handle_raw_shell_destructive_verdict_cancelled_on_no(default_cfg):
    from ohmyshell.danger_classifier import ClassificationResult, Destructive

    destructive = ClassificationResult(
        verdict=Destructive(explanation="dangerous", trash_alternative_possible=True),
        source="regex",
    )
    with patch("ohmyshell.main.classify", return_value=destructive):
        with patch("ohmyshell.main.subprocess.run") as mock_run:
            main_module._handle_raw_shell("rm -rf /tmp/x", default_cfg, confirm=lambda _: "n")
    mock_run.assert_not_called()


def test_handle_raw_shell_destructive_verdict_shows_explanation(default_cfg, capsys):
    from ohmyshell.danger_classifier import ClassificationResult, Destructive

    destructive = ClassificationResult(
        verdict=Destructive(explanation="this will delete everything", trash_alternative_possible=True),
        source="regex",
    )
    with patch("ohmyshell.main.classify", return_value=destructive):
        with patch("ohmyshell.main.subprocess.run"):
            main_module._handle_raw_shell("rm -rf /tmp/x", default_cfg, confirm=lambda _: "n")
    out = capsys.readouterr().out
    assert "this will delete everything" in out


def test_handle_raw_shell_classifier_error_does_not_run_command(default_cfg, capsys):
    from ohmyshell.danger_classifier import DangerClassifierError

    with patch("ohmyshell.main.classify", side_effect=DangerClassifierError("Ollama unreachable")):
        with patch("ohmyshell.main.subprocess.run") as mock_run:
            main_module._handle_raw_shell("some ambiguous command", default_cfg)
    mock_run.assert_not_called()
    out = capsys.readouterr().out
    assert "Ollama unreachable" in out


# --- _handle_natural_language --------------------------------------------------------


def test_handle_natural_language_prints_action_and_risk(registry, default_cfg, capsys):
    fake_result = ParseResult(
        action="list_processes",
        intent=ValidatedIntent(action="list_processes", params={"filter": "chrome"}, risk="low"),
        attempts=1,
    )
    with patch("ohmyshell.main.parse_intent", return_value=fake_result):
        main_module._handle_natural_language("find chrome processes", registry, default_cfg)

    out = capsys.readouterr().out
    assert "list_processes" in out
    assert "low" in out
    assert "chrome" in out
    assert "not wired up yet" not in out or "Execution isn't wired up yet" in out


def test_handle_natural_language_reports_unmapped(registry, default_cfg, capsys):
    fake_result = ParseResult(action="unmapped", intent=None, attempts=2, last_error="nope")
    with patch("ohmyshell.main.parse_intent", return_value=fake_result):
        main_module._handle_natural_language("do something weird", registry, default_cfg)

    out = capsys.readouterr().out
    assert "couldn't map" in out.lower()


def test_handle_natural_language_reports_backend_failure(registry, default_cfg, capsys):
    with patch(
        "ohmyshell.main.parse_intent", side_effect=IntentParseError("Ollama unreachable")
    ):
        main_module._handle_natural_language("clean up temp files", registry, default_cfg)

    out = capsys.readouterr().out
    assert "Ollama unreachable" in out


def test_handle_natural_language_never_executes_anything(registry, default_cfg):
    """This step must not touch subprocess at all for NL input — preview only."""
    fake_result = ParseResult(
        action="clean_temp_files",
        intent=ValidatedIntent(action="clean_temp_files", params={"days": 7}, risk="medium"),
        attempts=1,
    )
    with patch("ohmyshell.main.parse_intent", return_value=fake_result):
        with patch("ohmyshell.main.subprocess.run") as mock_run:
            main_module._handle_natural_language("clean up temp files", registry, default_cfg)
    mock_run.assert_not_called()


# --- _handle_slash_command --------------------------------------------------------


def test_slash_exit_returns_true():
    assert main_module._handle_slash_command("/exit") is True


def test_slash_quit_returns_true():
    assert main_module._handle_slash_command("/quit") is True


def test_unknown_slash_command_returns_false_and_reports(capsys):
    result = main_module._handle_slash_command("/help")
    out = capsys.readouterr().out
    assert result is False
    assert "wired up" in out.lower()


# --- run(): REPL loop integration ---------------------------------------------------


def test_run_exits_cleanly_on_slash_exit():
    with patch("ohmyshell.main.input", side_effect=["/exit"]):
        with patch("ohmyshell.main.load_registry"):
            main_module.run()  # must return without raising


def test_run_exits_on_eof():
    with patch("ohmyshell.main.input", side_effect=EOFError):
        with patch("ohmyshell.main.load_registry"):
            main_module.run()


def test_run_dispatches_raw_shell_then_exits():
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