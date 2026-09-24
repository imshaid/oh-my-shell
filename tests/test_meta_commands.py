"""
Tests for the Meta-Command Handler (Build Order Step 14), rewritten for the
open-ended architecture (see meta_commands.py's own module docstring).
dispatch() no longer takes a registry argument; /capabilities returns
static explanatory text instead of listing a capability registry, and
/explain no longer references entry.params (audit_log.AuditEntry still has
that field, just unused now).
"""

from __future__ import annotations

from dataclasses import dataclass

import pytest
from rich.text import Text

from ohmyshell import audit_log as audit_log_module
from ohmyshell import config as config_module
from ohmyshell import meta_commands
from ohmyshell import trash as trash_module


@pytest.fixture
def cfg():
    return config_module.default_config()


@pytest.fixture
def base_dir(tmp_path):
    return tmp_path / "oh-my-shell-test"


def _dispatch(text, cfg, base_dir, session_start=0.0, tokens_used=None):
    return meta_commands.dispatch(
        text, cfg=cfg, session_start=session_start, base_dir=base_dir, tokens_used=tokens_used
    )


def _plain(markup_text: str) -> str:
    """Strips rich markup tags (e.g. "[omsh.accent]...[/omsh.accent]") down
    to the plain rendered text, so tests can assert on content without
    caring about which omsh.* styling tags wrap it. meta_commands.py now
    returns strings with rich markup throughout (Build Order color-audit
    pass); Text.from_markup() parses and discards the tags without needing
    a real theme/console (it only needs style *names* to be syntactically
    valid, not registered).
    """
    return Text.from_markup(markup_text).plain


class TestParsing:
    def test_empty_input_returns_empty_outcome(self, cfg, base_dir):
        outcome = _dispatch("", cfg, base_dir)
        assert outcome.text == ""
        assert outcome.should_exit is False

    def test_unrecognized_command_raises(self, cfg, base_dir):
        with pytest.raises(meta_commands.MetaCommandError, match="Unrecognized command"):
            _dispatch("/bogus", cfg, base_dir)


class TestExit:
    @pytest.mark.parametrize("text", ["/exit", "/quit"])
    def test_exit_commands_set_should_exit(self, text, cfg, base_dir):
        outcome = _dispatch(text, cfg, base_dir)
        assert outcome.should_exit is True


class TestClear:
    def test_clear_returns_ansi_sequence(self, cfg, base_dir):
        outcome = _dispatch("/clear", cfg, base_dir)
        assert "\033[2J" in outcome.text


class TestHelp:
    def test_bare_help_matches_blueprint_shape(self, cfg, base_dir):
        outcome = _dispatch("/help", cfg, base_dir)
        plain = _plain(outcome.text)
        assert "Oh My Shell — Command Reference" in plain
        assert "Just type naturally:" in plain
        assert "/model" in plain
        assert "Type /help <command> for details." in plain

    def test_help_with_known_topic_gives_detail(self, cfg, base_dir):
        outcome = _dispatch("/help model", cfg, base_dir)
        assert "/model list" in outcome.text

    def test_help_with_unknown_topic_gives_fallback(self, cfg, base_dir):
        outcome = _dispatch("/help bogus", cfg, base_dir)
        assert "No detailed help" in outcome.text


class TestModel:
    def test_bare_model_lists_available_with_active_marked(self, cfg, base_dir):
        outcome = _dispatch("/model", cfg, base_dir)
        assert cfg["model"]["active"] in outcome.text
        for name in cfg["model"]["available"]:
            assert name in outcome.text

    def test_model_list_same_as_bare(self, cfg, base_dir):
        bare = _dispatch("/model", cfg, base_dir).text
        listed = _dispatch("/model list", cfg, base_dir).text
        assert bare == listed

    def test_model_switch_changes_active_model(self, cfg, base_dir, monkeypatch):
        monkeypatch.setattr(config_module, "CONFIG_DIR", base_dir)
        monkeypatch.setattr(config_module, "CONFIG_PATH", base_dir / "config.json")
        outcome = _dispatch("/model switch phi4-mini", cfg, base_dir)
        assert "Switched active model to phi4-mini" in outcome.text
        assert cfg["model"]["active"] == "phi4-mini"

    def test_model_switch_unknown_model_raises(self, cfg, base_dir):
        with pytest.raises(meta_commands.MetaCommandError, match="Unknown model"):
            _dispatch("/model switch not-a-real-model", cfg, base_dir)

    def test_model_switch_no_name_raises(self, cfg, base_dir):
        with pytest.raises(meta_commands.MetaCommandError, match="Usage"):
            _dispatch("/model switch", cfg, base_dir)


class TestHistory:
    def test_no_requests_yet(self, cfg, base_dir):
        outcome = _dispatch("/history", cfg, base_dir, session_start=0.0)
        assert "No requests yet" in outcome.text

    def test_lists_session_requests(self, cfg, base_dir):
        audit_log_module.record_action(
            action="find /tmp -mtime +7 -delete", source="natural_language", status="done", base_dir=base_dir, now=10.0
        )
        outcome = _dispatch("/history", cfg, base_dir, session_start=5.0)
        assert "find /tmp -mtime +7 -delete" in outcome.text


class TestUndo:
    def test_nothing_to_undo(self, cfg, base_dir):
        outcome = _dispatch("/undo", cfg, base_dir)
        assert "Nothing to undo" in outcome.text

    def test_restores_trashed_file(self, cfg, base_dir, tmp_path):
        source = tmp_path / "f.txt"
        source.write_text("x")
        trash_module.move_to_trash(source, base_dir=base_dir)
        outcome = _dispatch("/undo", cfg, base_dir)
        assert "Restored 1 file" in outcome.text
        assert source.exists()


class TestTrash:
    def test_status_empty(self, cfg, base_dir):
        outcome = _dispatch("/trash status", cfg, base_dir)
        assert ".trash/ is empty" in outcome.text

    def test_status_with_items(self, cfg, base_dir, tmp_path):
        source = tmp_path / "f.txt"
        source.write_text("x")
        trash_module.move_to_trash(source, base_dir=base_dir)
        outcome = _dispatch("/trash status", cfg, base_dir)
        assert "1 item" in _plain(outcome.text)

    def test_clear_removes_everything(self, cfg, base_dir, tmp_path):
        source = tmp_path / "f.txt"
        source.write_text("x")
        trash_module.move_to_trash(source, base_dir=base_dir)
        outcome = _dispatch("/trash clear", cfg, base_dir)
        assert "Permanently deleted 1" in outcome.text
        assert trash_module.list_trash(base_dir) == []

    def test_bare_trash_defaults_to_status(self, cfg, base_dir):
        outcome = _dispatch("/trash", cfg, base_dir)
        assert "empty" in outcome.text

    def test_unknown_subcommand_raises(self, cfg, base_dir):
        with pytest.raises(meta_commands.MetaCommandError, match="Usage"):
            _dispatch("/trash bogus", cfg, base_dir)

    def test_keep_empty_trash(self, cfg, base_dir):
        outcome = _dispatch("/trash keep", cfg, base_dir)
        assert "nothing to keep" in outcome.text

    def test_keep_resets_retention_timer(self, cfg, base_dir, tmp_path):
        source = tmp_path / "f.txt"
        source.write_text("x")
        trash_module.move_to_trash(source, base_dir=base_dir, now=0.0)
        outcome = _dispatch("/trash keep", cfg, base_dir)
        assert "Reset the retention timer for 1 item" in outcome.text
        assert f"another {cfg['trash']['retention_days']} days" in outcome.text
        entry = trash_module.list_trash(base_dir)[0]
        assert entry.trashed_at > 0.0

    def test_keep_pluralizes_for_multiple_items(self, cfg, base_dir, tmp_path):
        a = tmp_path / "a.txt"
        b = tmp_path / "b.txt"
        a.write_text("x")
        b.write_text("y")
        trash_module.move_to_trash(a, base_dir=base_dir, now=0.0)
        trash_module.move_to_trash(b, base_dir=base_dir, now=0.0)
        outcome = _dispatch("/trash keep", cfg, base_dir)
        assert "2 items" in outcome.text


class TestLog:
    def test_empty_log(self, cfg, base_dir):
        outcome = _dispatch("/log", cfg, base_dir)
        assert "Audit log is empty" in outcome.text

    def test_shows_recent_entries(self, cfg, base_dir):
        audit_log_module.record_action(
            action="kill -9 1234", source="natural_language", status="done", risk="high", base_dir=base_dir
        )
        outcome = _dispatch("/log", cfg, base_dir)
        assert "kill -9 1234" in outcome.text
        assert "high" in outcome.text

    def test_export_produces_valid_jsonl(self, cfg, base_dir):
        import json

        audit_log_module.record_action(action="a", source="raw_shell", status="done", base_dir=base_dir)
        outcome = _dispatch("/log export", cfg, base_dir)
        for line in outcome.text.splitlines():
            json.loads(line)  # must not raise


class TestCapabilities:
    """
    Rewritten for the open-ended architecture: there's no registry to list
    any more, so /capabilities returns static explanatory text about how
    the AI generates open-ended commands instead of enumerating a fixed
    action list (see meta_commands._handle_capabilities' own docstring).
    """

    def test_explains_open_ended_architecture(self, cfg, base_dir):
        outcome = _dispatch("/capabilities", cfg, base_dir)
        assert "fixed list" in outcome.text.lower()
        assert "shell command" in outcome.text.lower()

    def test_mentions_raw_shell_passthrough(self, cfg, base_dir):
        outcome = _dispatch("/capabilities", cfg, base_dir)
        assert "raw shell" in outcome.text.lower()

    def test_does_not_crash_without_a_registry(self, cfg, base_dir):
        # Regression guard: dispatch() must not require a registry kwarg at
        # all any more -- this call itself is the assertion (it would raise
        # TypeError if a registry were still required).
        outcome = _dispatch("/capabilities", cfg, base_dir)
        assert outcome.text


class TestExplain:
    def test_no_decisions_yet(self, cfg, base_dir):
        outcome = _dispatch("/explain", cfg, base_dir)
        assert "No AI decisions recorded" in outcome.text

    def test_explains_most_recent_entry(self, cfg, base_dir):
        audit_log_module.record_action(
            action="find /tmp -mtime +7 -delete", source="natural_language", status="done",
            risk="medium", base_dir=base_dir,
        )
        outcome = _dispatch("/explain", cfg, base_dir)
        assert "find /tmp -mtime +7 -delete" in outcome.text
        assert "medium" in outcome.text
        assert "done" in outcome.text

    def test_explain_does_not_reference_params(self, cfg, base_dir):
        """
        _handle_explain no longer references entry.params (unused now) --
        confirmed here by recording an entry with no params at all and
        making sure /explain still works and shows only
        action/risk/status.
        """
        audit_log_module.record_action(
            action="uptime", source="natural_language", status="done", risk="low", base_dir=base_dir,
        )
        outcome = _dispatch("/explain", cfg, base_dir)
        assert "uptime" in outcome.text
        assert "low" in outcome.text


class TestStats:
    def test_shows_session_summary(self, cfg, base_dir):
        audit_log_module.record_action(action="a", source="raw_shell", status="done", base_dir=base_dir, now=10.0)
        outcome = _dispatch("/stats", cfg, base_dir, session_start=5.0)
        assert "1 requests processed" in outcome.text

    def test_includes_tokens_when_provided(self, cfg, base_dir):
        outcome = _dispatch("/stats", cfg, base_dir, session_start=0.0, tokens_used=500)
        assert "500 tokens used" in outcome.text


class TestSystem:
    @dataclass
    class _FakeCompleted:
        returncode: int
        stdout: str = ""
        stderr: str = ""

    def _no_gpu_runner(self, cmd, **kwargs):
        return self._FakeCompleted(returncode=1)

    def test_matches_blueprint_shape(self, cfg, base_dir, monkeypatch):
        monkeypatch.setattr("ohmyshell.hardware.shutil.which", lambda name: None)
        outcome = meta_commands._handle_system(cfg, session_start=0.0, base_dir=base_dir)
        assert "System Info" in outcome
        assert "Oh My Shell" in outcome
        assert "CPU:" in outcome
        assert "RAM:" in outcome
        assert "Active model:" in outcome

    def test_shows_google_ai_studio_provider_label_by_default(self, cfg, base_dir, monkeypatch):
        monkeypatch.setattr("ohmyshell.hardware.shutil.which", lambda name: None)
        outcome = meta_commands._handle_system(cfg, session_start=0.0, base_dir=base_dir)
        assert "Provider: Google AI Studio (Gemini)" in _plain(outcome)

    def test_shows_local_ollama_provider_label_when_configured(self, cfg, base_dir, monkeypatch):
        monkeypatch.setattr("ohmyshell.hardware.shutil.which", lambda name: None)
        cfg["model"]["provider"] = "ollama"
        outcome = meta_commands._handle_system(cfg, session_start=0.0, base_dir=base_dir)
        assert "Provider: local (Ollama)" in _plain(outcome)

    def test_via_dispatch(self, cfg, base_dir, monkeypatch):
        monkeypatch.setattr("ohmyshell.hardware.shutil.which", lambda name: None)
        outcome = _dispatch("/system", cfg, base_dir)
        assert "System Info" in outcome.text


class TestConfig:
    def test_bare_config_shows_json(self, cfg, base_dir):
        outcome = _dispatch("/config", cfg, base_dir)
        assert "trash" in outcome.text
        assert "retention_days" in outcome.text

    def test_config_set_updates_value(self, cfg, base_dir, monkeypatch):
        monkeypatch.setattr(config_module, "CONFIG_DIR", base_dir)
        monkeypatch.setattr(config_module, "CONFIG_PATH", base_dir / "config.json")
        outcome = _dispatch("/config set trash.retention_days 14", cfg, base_dir)
        assert "trash.retention_days" in outcome.text
        assert cfg["trash"]["retention_days"] == 14

    def test_config_set_coerces_bool(self, cfg, base_dir, monkeypatch):
        monkeypatch.setattr(config_module, "CONFIG_DIR", base_dir)
        monkeypatch.setattr(config_module, "CONFIG_PATH", base_dir / "config.json")
        _dispatch("/config set safety.safe_mode true", cfg, base_dir)
        assert cfg["safety"]["safe_mode"] is True

    def test_config_set_provider_key(self, cfg, base_dir, monkeypatch):
        """model.provider is the explicit one-line rollback switch (ollama <->
        google_ai_studio) — confirm /config set can reach it."""
        monkeypatch.setattr(config_module, "CONFIG_DIR", base_dir)
        monkeypatch.setattr(config_module, "CONFIG_PATH", base_dir / "config.json")
        _dispatch("/config set model.provider ollama", cfg, base_dir)
        assert cfg["model"]["provider"] == "ollama"

    def test_config_set_unknown_key_raises(self, cfg, base_dir):
        with pytest.raises(meta_commands.MetaCommandError, match="Unknown config key"):
            _dispatch("/config set bogus.key 1", cfg, base_dir)

    def test_config_set_missing_args_raises(self, cfg, base_dir):
        with pytest.raises(meta_commands.MetaCommandError, match="Usage"):
            _dispatch("/config set trash.retention_days", cfg, base_dir)


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