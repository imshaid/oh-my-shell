"""Tests for the Meta-Command Handler (Build Order Step 14)."""

from __future__ import annotations

from dataclasses import dataclass

import pytest

from ohmyshell import audit_log as audit_log_module
from ohmyshell import config as config_module
from ohmyshell import meta_commands
from ohmyshell import trash as trash_module
from ohmyshell.registry import load as load_registry


@pytest.fixture
def cfg():
    return config_module.default_config()


@pytest.fixture
def registry():
    return load_registry()


@pytest.fixture
def base_dir(tmp_path):
    return tmp_path / "oh-my-shell-test"


def _dispatch(text, cfg, registry, base_dir, session_start=0.0, tokens_used=None):
    return meta_commands.dispatch(
        text, cfg=cfg, registry=registry, session_start=session_start, base_dir=base_dir, tokens_used=tokens_used
    )


class TestParsing:
    def test_empty_input_returns_empty_outcome(self, cfg, registry, base_dir):
        outcome = _dispatch("", cfg, registry, base_dir)
        assert outcome.text == ""
        assert outcome.should_exit is False

    def test_unrecognized_command_raises(self, cfg, registry, base_dir):
        with pytest.raises(meta_commands.MetaCommandError, match="Unrecognized command"):
            _dispatch("/bogus", cfg, registry, base_dir)


class TestExit:
    @pytest.mark.parametrize("text", ["/exit", "/quit"])
    def test_exit_commands_set_should_exit(self, text, cfg, registry, base_dir):
        outcome = _dispatch(text, cfg, registry, base_dir)
        assert outcome.should_exit is True


class TestClear:
    def test_clear_returns_ansi_sequence(self, cfg, registry, base_dir):
        outcome = _dispatch("/clear", cfg, registry, base_dir)
        assert "\033[2J" in outcome.text


class TestHelp:
    def test_bare_help_matches_blueprint_shape(self, cfg, registry, base_dir):
        outcome = _dispatch("/help", cfg, registry, base_dir)
        assert "Oh My Shell — Command Reference" in outcome.text
        assert "Just type naturally:" in outcome.text
        assert "/model" in outcome.text
        assert "Type /help <command> for details." in outcome.text

    def test_help_with_known_topic_gives_detail(self, cfg, registry, base_dir):
        outcome = _dispatch("/help model", cfg, registry, base_dir)
        assert "/model list" in outcome.text

    def test_help_with_unknown_topic_gives_fallback(self, cfg, registry, base_dir):
        outcome = _dispatch("/help bogus", cfg, registry, base_dir)
        assert "No detailed help" in outcome.text


class TestModel:
    def test_bare_model_lists_available_with_active_marked(self, cfg, registry, base_dir):
        outcome = _dispatch("/model", cfg, registry, base_dir)
        assert cfg["model"]["active"] in outcome.text
        for name in cfg["model"]["available"]:
            assert name in outcome.text

    def test_model_list_same_as_bare(self, cfg, registry, base_dir):
        bare = _dispatch("/model", cfg, registry, base_dir).text
        listed = _dispatch("/model list", cfg, registry, base_dir).text
        assert bare == listed

    def test_model_switch_changes_active_model(self, cfg, registry, base_dir, monkeypatch):
        monkeypatch.setattr(config_module, "CONFIG_DIR", base_dir)
        monkeypatch.setattr(config_module, "CONFIG_PATH", base_dir / "config.json")
        outcome = _dispatch("/model switch phi4-mini", cfg, registry, base_dir)
        assert "Switched active model to phi4-mini" in outcome.text
        assert cfg["model"]["active"] == "phi4-mini"

    def test_model_switch_unknown_model_raises(self, cfg, registry, base_dir):
        with pytest.raises(meta_commands.MetaCommandError, match="Unknown model"):
            _dispatch("/model switch not-a-real-model", cfg, registry, base_dir)

    def test_model_switch_no_name_raises(self, cfg, registry, base_dir):
        with pytest.raises(meta_commands.MetaCommandError, match="Usage"):
            _dispatch("/model switch", cfg, registry, base_dir)


class TestHistory:
    def test_no_requests_yet(self, cfg, registry, base_dir):
        outcome = _dispatch("/history", cfg, registry, base_dir, session_start=0.0)
        assert "No requests yet" in outcome.text

    def test_lists_session_requests(self, cfg, registry, base_dir):
        audit_log_module.record_action(action="clean_temp_files", source="natural_language", status="done", base_dir=base_dir, now=10.0)
        outcome = _dispatch("/history", cfg, registry, base_dir, session_start=5.0)
        assert "clean_temp_files" in outcome.text


class TestUndo:
    def test_nothing_to_undo(self, cfg, registry, base_dir):
        outcome = _dispatch("/undo", cfg, registry, base_dir)
        assert "Nothing to undo" in outcome.text

    def test_restores_trashed_file(self, cfg, registry, base_dir, tmp_path):
        source = tmp_path / "f.txt"
        source.write_text("x")
        trash_module.move_to_trash(source, base_dir=base_dir)
        outcome = _dispatch("/undo", cfg, registry, base_dir)
        assert "Restored 1 file" in outcome.text
        assert source.exists()


class TestTrash:
    def test_status_empty(self, cfg, registry, base_dir):
        outcome = _dispatch("/trash status", cfg, registry, base_dir)
        assert ".trash/ is empty" in outcome.text

    def test_status_with_items(self, cfg, registry, base_dir, tmp_path):
        source = tmp_path / "f.txt"
        source.write_text("x")
        trash_module.move_to_trash(source, base_dir=base_dir)
        outcome = _dispatch("/trash status", cfg, registry, base_dir)
        assert "1 item" in outcome.text

    def test_clear_removes_everything(self, cfg, registry, base_dir, tmp_path):
        source = tmp_path / "f.txt"
        source.write_text("x")
        trash_module.move_to_trash(source, base_dir=base_dir)
        outcome = _dispatch("/trash clear", cfg, registry, base_dir)
        assert "Permanently deleted 1" in outcome.text
        assert trash_module.list_trash(base_dir) == []

    def test_bare_trash_defaults_to_status(self, cfg, registry, base_dir):
        outcome = _dispatch("/trash", cfg, registry, base_dir)
        assert "empty" in outcome.text

    def test_unknown_subcommand_raises(self, cfg, registry, base_dir):
        with pytest.raises(meta_commands.MetaCommandError, match="Usage"):
            _dispatch("/trash bogus", cfg, registry, base_dir)

    def test_keep_empty_trash(self, cfg, registry, base_dir):
        outcome = _dispatch("/trash keep", cfg, registry, base_dir)
        assert "nothing to keep" in outcome.text

    def test_keep_resets_retention_timer(self, cfg, registry, base_dir, tmp_path):
        source = tmp_path / "f.txt"
        source.write_text("x")
        trash_module.move_to_trash(source, base_dir=base_dir, now=0.0)
        outcome = _dispatch("/trash keep", cfg, registry, base_dir)
        assert "Reset the retention timer for 1 item" in outcome.text
        assert f"another {cfg['trash']['retention_days']} days" in outcome.text
        entry = trash_module.list_trash(base_dir)[0]
        assert entry.trashed_at > 0.0

    def test_keep_pluralizes_for_multiple_items(self, cfg, registry, base_dir, tmp_path):
        a = tmp_path / "a.txt"
        b = tmp_path / "b.txt"
        a.write_text("x")
        b.write_text("y")
        trash_module.move_to_trash(a, base_dir=base_dir, now=0.0)
        trash_module.move_to_trash(b, base_dir=base_dir, now=0.0)
        outcome = _dispatch("/trash keep", cfg, registry, base_dir)
        assert "2 items" in outcome.text


class TestLog:
    def test_empty_log(self, cfg, registry, base_dir):
        outcome = _dispatch("/log", cfg, registry, base_dir)
        assert "Audit log is empty" in outcome.text

    def test_shows_recent_entries(self, cfg, registry, base_dir):
        audit_log_module.record_action(action="kill_process", source="natural_language", status="done", risk="high", base_dir=base_dir)
        outcome = _dispatch("/log", cfg, registry, base_dir)
        assert "kill_process" in outcome.text
        assert "high" in outcome.text

    def test_export_produces_valid_jsonl(self, cfg, registry, base_dir):
        import json

        audit_log_module.record_action(action="a", source="raw_shell", status="done", base_dir=base_dir)
        outcome = _dispatch("/log export", cfg, registry, base_dir)
        for line in outcome.text.splitlines():
            json.loads(line)  # must not raise


class TestCapabilities:
    def test_lists_all_registered_actions(self, cfg, registry, base_dir):
        outcome = _dispatch("/capabilities", cfg, registry, base_dir)
        for action in registry.actions():
            assert action in outcome.text

    def test_includes_risk_levels(self, cfg, registry, base_dir):
        outcome = _dispatch("/capabilities", cfg, registry, base_dir)
        assert "risk: high" in outcome.text  # kill_process


class TestExplain:
    def test_no_decisions_yet(self, cfg, registry, base_dir):
        outcome = _dispatch("/explain", cfg, registry, base_dir)
        assert "No AI decisions recorded" in outcome.text

    def test_explains_most_recent_entry(self, cfg, registry, base_dir):
        audit_log_module.record_action(
            action="clean_temp_files", source="natural_language", status="done",
            risk="medium", params={"days": 7}, base_dir=base_dir,
        )
        outcome = _dispatch("/explain", cfg, registry, base_dir)
        assert "clean_temp_files" in outcome.text
        assert "days=7" in outcome.text
        assert "medium" in outcome.text


class TestStats:
    def test_shows_session_summary(self, cfg, registry, base_dir):
        audit_log_module.record_action(action="a", source="raw_shell", status="done", base_dir=base_dir, now=10.0)
        outcome = _dispatch("/stats", cfg, registry, base_dir, session_start=5.0)
        assert "1 requests processed" in outcome.text

    def test_includes_tokens_when_provided(self, cfg, registry, base_dir):
        outcome = _dispatch("/stats", cfg, registry, base_dir, session_start=0.0, tokens_used=500)
        assert "500 tokens used" in outcome.text


class TestSystem:
    @dataclass
    class _FakeCompleted:
        returncode: int
        stdout: str = ""
        stderr: str = ""

    def _no_gpu_runner(self, cmd, **kwargs):
        return self._FakeCompleted(returncode=1)

    def test_matches_blueprint_shape(self, cfg, registry, base_dir, monkeypatch):
        monkeypatch.setattr("ohmyshell.hardware.shutil.which", lambda name: None)
        outcome = meta_commands._handle_system(cfg, session_start=0.0, base_dir=base_dir)
        assert "System Info" in outcome
        assert "Oh My Shell" in outcome
        assert "CPU:" in outcome
        assert "RAM:" in outcome
        assert "Active model:" in outcome
        assert "Provider: local (Ollama)" in outcome

    def test_via_dispatch(self, cfg, registry, base_dir, monkeypatch):
        monkeypatch.setattr("ohmyshell.hardware.shutil.which", lambda name: None)
        outcome = _dispatch("/system", cfg, registry, base_dir)
        assert "System Info" in outcome.text


class TestConfig:
    def test_bare_config_shows_json(self, cfg, registry, base_dir):
        outcome = _dispatch("/config", cfg, registry, base_dir)
        assert "trash" in outcome.text
        assert "retention_days" in outcome.text

    def test_config_set_updates_value(self, cfg, registry, base_dir, monkeypatch):
        monkeypatch.setattr(config_module, "CONFIG_DIR", base_dir)
        monkeypatch.setattr(config_module, "CONFIG_PATH", base_dir / "config.json")
        outcome = _dispatch("/config set trash.retention_days 14", cfg, registry, base_dir)
        assert "trash.retention_days" in outcome.text
        assert cfg["trash"]["retention_days"] == 14

    def test_config_set_coerces_bool(self, cfg, registry, base_dir, monkeypatch):
        monkeypatch.setattr(config_module, "CONFIG_DIR", base_dir)
        monkeypatch.setattr(config_module, "CONFIG_PATH", base_dir / "config.json")
        _dispatch("/config set safety.safe_mode true", cfg, registry, base_dir)
        assert cfg["safety"]["safe_mode"] is True

    def test_config_set_unknown_key_raises(self, cfg, registry, base_dir):
        with pytest.raises(meta_commands.MetaCommandError, match="Unknown config key"):
            _dispatch("/config set bogus.key 1", cfg, registry, base_dir)

    def test_config_set_missing_args_raises(self, cfg, registry, base_dir):
        with pytest.raises(meta_commands.MetaCommandError, match="Usage"):
            _dispatch("/config set trash.retention_days", cfg, registry, base_dir)


class TestCoerceConfigValue:
    def test_true_false(self):
        assert meta_commands._coerce_config_value("true") is True
        assert meta_commands._coerce_config_value("false") is False

    def test_int(self):
        assert meta_commands._coerce_config_value("14") == 14

    def test_float(self):
        assert meta_commands._coerce_config_value("1.5") == 1.5

    def test_string_fallback(self):
        assert meta_commands._coerce_config_value("normal") == "normal"