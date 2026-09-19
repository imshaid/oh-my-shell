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


class TestChooseModel:
    def test_low_ram_chooses_smallest_model(self):
        assert wizard.choose_model(_snapshot(ram_gb=4.0)) == "phi4-mini"

    def test_mid_ram_no_gpu_chooses_mid_cpu_model(self):
        assert wizard.choose_model(_snapshot(ram_gb=12.0, has_gpu=False)) == "qwen3.5:4b"

    def test_mid_ram_with_gpu_chooses_gpu_model(self):
        assert wizard.choose_model(_snapshot(ram_gb=12.0, has_gpu=True)) == "lfm2.5-8b-a1b"

    def test_high_ram_chooses_largest_model(self):
        assert wizard.choose_model(_snapshot(ram_gb=32.0)) == "qwen3:8b"

    def test_high_ram_with_gpu_still_chooses_largest_model(self):
        # documented default: GPU is only a tie-breaker in the middle band
        assert wizard.choose_model(_snapshot(ram_gb=32.0, has_gpu=True)) == "qwen3:8b"

    def test_boundary_at_8gb_is_mid_tier_not_low(self):
        assert wizard.choose_model(_snapshot(ram_gb=8.0)) == "qwen3.5:4b"

    def test_boundary_at_16gb_is_high_tier_not_mid(self):
        assert wizard.choose_model(_snapshot(ram_gb=16.0)) == "qwen3:8b"

    def test_chosen_model_is_always_in_config_available_list(self):
        available = config_module.DEFAULT_CONFIG["model"]["available"]
        for ram in (2.0, 8.0, 12.0, 16.0, 64.0):
            for has_gpu in (True, False):
                assert wizard.choose_model(_snapshot(ram_gb=ram, has_gpu=has_gpu)) in available


class TestRenderWelcomeText:
    def test_includes_cpu_and_ram(self):
        result = wizard.WizardResult(
            snapshot=_snapshot(ram_gb=16.0, core_count=12),
            chosen_model="qwen3:8b",
            config=config_module.default_config(),
        )
        text = wizard.render_welcome_text(result)
        assert "12 CPU cores" in text
        assert "16.0GB RAM" in text

    def test_includes_chosen_model(self):
        result = wizard.WizardResult(
            snapshot=_snapshot(ram_gb=16.0), chosen_model="qwen3:8b", config=config_module.default_config(),
        )
        text = wizard.render_welcome_text(result)
        assert "qwen3:8b" in text

    def test_includes_gpu_name_when_present(self):
        result = wizard.WizardResult(
            snapshot=_snapshot(ram_gb=16.0, has_gpu=True), chosen_model="qwen3:8b", config=config_module.default_config(),
        )
        text = wizard.render_welcome_text(result)
        assert "NVIDIA Test GPU" in text

    def test_omits_gpu_line_when_absent(self):
        result = wizard.WizardResult(
            snapshot=_snapshot(ram_gb=16.0, has_gpu=False), chosen_model="qwen3:8b", config=config_module.default_config(),
        )
        text = wizard.render_welcome_text(result)
        assert "GPU:" not in text

    def test_mentions_how_to_change_model_later(self):
        result = wizard.WizardResult(
            snapshot=_snapshot(ram_gb=16.0), chosen_model="qwen3:8b", config=config_module.default_config(),
        )
        text = wizard.render_welcome_text(result)
        assert "/model" in text


class TestRunWizard:
    def test_writes_chosen_model_to_config_file(self, tmp_path, monkeypatch):
        monkeypatch.setattr(config_module, "CONFIG_DIR", tmp_path)
        monkeypatch.setattr(config_module, "CONFIG_PATH", tmp_path / "config.json")

        result = wizard.run_wizard(read_hardware=lambda: _snapshot(ram_gb=32.0), print_fn=lambda _: None)

        assert result.chosen_model == "qwen3:8b"
        loaded = config_module.load()
        assert config_module.get(loaded, "model.active") == "qwen3:8b"

    def test_save_config_false_does_not_write_file(self, tmp_path, monkeypatch):
        monkeypatch.setattr(config_module, "CONFIG_DIR", tmp_path)
        monkeypatch.setattr(config_module, "CONFIG_PATH", tmp_path / "config.json")

        wizard.run_wizard(read_hardware=lambda: _snapshot(ram_gb=32.0), print_fn=lambda _: None, save_config=False)

        assert not (tmp_path / "config.json").exists()

    def test_prints_welcome_text(self, tmp_path, monkeypatch):
        monkeypatch.setattr(config_module, "CONFIG_DIR", tmp_path)
        monkeypatch.setattr(config_module, "CONFIG_PATH", tmp_path / "config.json")

        printed = []
        wizard.run_wizard(read_hardware=lambda: _snapshot(ram_gb=32.0), print_fn=printed.append)
        assert len(printed) == 1
        assert "Welcome to Oh My Shell" in printed[0]

    def test_returned_config_has_other_defaults_intact(self, tmp_path, monkeypatch):
        monkeypatch.setattr(config_module, "CONFIG_DIR", tmp_path)
        monkeypatch.setattr(config_module, "CONFIG_PATH", tmp_path / "config.json")

        result = wizard.run_wizard(read_hardware=lambda: _snapshot(ram_gb=4.0), print_fn=lambda _: None)
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