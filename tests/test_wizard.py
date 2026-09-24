"""Tests for the first-run wizard (Build Order Step 13)."""

from __future__ import annotations

import pytest

from ohmyshell import config as config_module
from ohmyshell import hardware as hardware_module
from ohmyshell import wizard


def _snapshot(*, ram_gb: float, has_gpu: bool = False, core_count: int = 8) -> hardware_module.HardwareSnapshot:
    gpu = hardware_module.GpuInfo(name="NVIDIA Test GPU", vram_total_mb=8192.0, vram_used_mb=100.0) if has_gpu else None
    return hardware_module.HardwareSnapshot(
        cpu=hardware_module.CpuInfo(core_count=core_count, usage_percent=10.0),
        ram=hardware_module.RamInfo(total_gb=ram_gb, used_gb=1.0),
        gpu=gpu,
    )


class TestVerifyApiKey:
    def test_returns_true_when_verify_fn_succeeds(self):
        assert wizard.verify_api_key("some-key", verify_fn=lambda key: True) is True

    def test_raises_when_verify_fn_raises(self):
        def _fail(key):
            raise wizard.ApiKeyVerificationError("bad key")

        with pytest.raises(wizard.ApiKeyVerificationError):
            wizard.verify_api_key("bad-key", verify_fn=_fail)


class TestSaveApiKey:
    def test_writes_key_to_env_file(self, tmp_path):
        env_path = tmp_path / ".oh-my-shell" / ".env"
        wizard.save_api_key("my-real-key", env_path=env_path)

        assert env_path.exists()
        assert env_path.read_text(encoding="utf-8") == "GOOGLE_AI_STUDIO_API_KEY=my-real-key\n"

    def test_creates_parent_directory_if_missing(self, tmp_path):
        env_path = tmp_path / "nested" / ".oh-my-shell" / ".env"
        wizard.save_api_key("key", env_path=env_path)
        assert env_path.exists()


class TestRenderWelcomeText:
    def test_includes_cpu_and_ram(self):
        result = wizard.WizardResult(
            snapshot=_snapshot(ram_gb=16.0, core_count=12),
            api_key_saved=True,
            config=config_module.default_config(),
        )
        text = wizard.render_welcome_text(result)
        assert "12 CPU cores" in text
        assert "16.0GB RAM" in text

    def test_includes_model_name(self):
        result = wizard.WizardResult(
            snapshot=_snapshot(ram_gb=16.0), api_key_saved=True, config=config_module.default_config(),
        )
        text = wizard.render_welcome_text(result)
        assert wizard.DEFAULT_MODEL in text

    def test_includes_gpu_name_when_present(self):
        result = wizard.WizardResult(
            snapshot=_snapshot(ram_gb=16.0, has_gpu=True), api_key_saved=True, config=config_module.default_config(),
        )
        text = wizard.render_welcome_text(result)
        assert "NVIDIA Test GPU" in text

    def test_omits_gpu_line_when_absent(self):
        result = wizard.WizardResult(
            snapshot=_snapshot(ram_gb=16.0, has_gpu=False), api_key_saved=True, config=config_module.default_config(),
        )
        text = wizard.render_welcome_text(result)
        assert "GPU:" not in text

    def test_mentions_how_to_change_model_later(self):
        result = wizard.WizardResult(
            snapshot=_snapshot(ram_gb=16.0), api_key_saved=True, config=config_module.default_config(),
        )
        text = wizard.render_welcome_text(result)
        assert "model.active" in text


class TestRunWizard:
    def _run(self, tmp_path, monkeypatch, **overrides):
        monkeypatch.setattr(config_module, "CONFIG_DIR", tmp_path)
        monkeypatch.setattr(config_module, "CONFIG_PATH", tmp_path / "config.json")
        kwargs = dict(
            read_hardware=lambda: _snapshot(ram_gb=32.0),
            read_api_key=lambda: "test-api-key",
            verify_fn=lambda key: True,
            print_fn=lambda _: None,
            env_path=tmp_path / ".env",
        )
        kwargs.update(overrides)
        return wizard.run_wizard(**kwargs)

    def test_writes_default_config_file(self, tmp_path, monkeypatch):
        self._run(tmp_path, monkeypatch)

        loaded = config_module.load()
        assert config_module.get(loaded, "model.active") == wizard.DEFAULT_MODEL

    def test_saves_api_key_to_env_file(self, tmp_path, monkeypatch):
        self._run(tmp_path, monkeypatch)

        env_path = tmp_path / ".env"
        assert env_path.exists()
        assert "test-api-key" in env_path.read_text(encoding="utf-8")

    def test_raises_when_key_verification_fails(self, tmp_path, monkeypatch):
        def _fail(key):
            raise wizard.ApiKeyVerificationError("invalid key")

        with pytest.raises(wizard.ApiKeyVerificationError):
            self._run(tmp_path, monkeypatch, verify_fn=_fail)

    def test_save_config_false_does_not_write_files(self, tmp_path, monkeypatch):
        self._run(tmp_path, monkeypatch, save_config=False)

        assert not (tmp_path / "config.json").exists()
        assert not (tmp_path / ".env").exists()

    def test_prints_welcome_text(self, tmp_path, monkeypatch):
        printed = []
        self._run(tmp_path, monkeypatch, print_fn=printed.append)
        assert len(printed) == 1
        assert "Welcome to Oh My Shell" in printed[0]

    def test_returned_config_has_other_defaults_intact(self, tmp_path, monkeypatch):
        result = self._run(tmp_path, monkeypatch)
        assert result.config["trash"]["retention_days"] == 8


class TestShouldRunWizard:
    def test_true_when_config_file_does_not_exist(self, tmp_path):
        assert wizard.should_run_wizard(config_path=tmp_path / "config.json") is True

    def test_false_when_config_file_exists(self, tmp_path):
        path = tmp_path / "config.json"
        path.write_text("{}")
        assert wizard.should_run_wizard(config_path=path) is False

    def test_defaults_to_real_config_path_when_not_given(self, tmp_path, monkeypatch):
        monkeypatch.setattr(config_module, "CONFIG_PATH", tmp_path / "config.json")
        assert wizard.should_run_wizard() is True