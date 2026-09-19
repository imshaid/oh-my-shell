"""Tests for hardware detection (Build Order Step 12)."""

from __future__ import annotations

from dataclasses import dataclass

import pytest

from ohmyshell import hardware


class TestReadCpu:
    def test_returns_positive_core_count(self):
        info = hardware.read_cpu()
        assert info.core_count >= 1

    def test_usage_percent_is_a_float_in_range(self):
        info = hardware.read_cpu()
        assert 0.0 <= info.usage_percent <= 100.0


class TestReadRam:
    def test_total_is_positive(self):
        info = hardware.read_ram()
        assert info.total_gb > 0

    def test_used_does_not_exceed_total(self):
        info = hardware.read_ram()
        assert info.used_gb <= info.total_gb + 0.5  # small rounding tolerance


class TestReadGpu:
    def test_returns_none_when_nvidia_smi_not_on_path(self, monkeypatch):
        monkeypatch.setattr(hardware.shutil, "which", lambda name: None)
        assert hardware.read_gpu() is None

    def test_returns_gpu_info_when_nvidia_smi_succeeds(self, monkeypatch):
        monkeypatch.setattr(hardware.shutil, "which", lambda name: "/usr/bin/nvidia-smi")

        @dataclass
        class _Completed:
            returncode: int
            stdout: str
            stderr: str = ""

        def fake_runner(cmd, **kwargs):
            return _Completed(returncode=0, stdout="NVIDIA GeForce RTX 4090, 24576, 2048\n")

        gpu = hardware.read_gpu(runner=fake_runner)
        assert gpu is not None
        assert gpu.name == "NVIDIA GeForce RTX 4090"
        assert gpu.vram_total_mb == 24576.0
        assert gpu.vram_used_mb == 2048.0

    def test_returns_none_when_nvidia_smi_errors(self, monkeypatch):
        monkeypatch.setattr(hardware.shutil, "which", lambda name: "/usr/bin/nvidia-smi")

        @dataclass
        class _Completed:
            returncode: int
            stdout: str
            stderr: str = ""

        def fake_runner(cmd, **kwargs):
            return _Completed(returncode=1, stdout="")

        assert hardware.read_gpu(runner=fake_runner) is None

    def test_returns_none_when_nvidia_smi_raises_oserror(self, monkeypatch):
        monkeypatch.setattr(hardware.shutil, "which", lambda name: "/usr/bin/nvidia-smi")

        def fake_runner(cmd, **kwargs):
            raise OSError("no such binary")

        assert hardware.read_gpu(runner=fake_runner) is None

    def test_returns_none_on_malformed_output(self, monkeypatch):
        monkeypatch.setattr(hardware.shutil, "which", lambda name: "/usr/bin/nvidia-smi")

        @dataclass
        class _Completed:
            returncode: int
            stdout: str
            stderr: str = ""

        def fake_runner(cmd, **kwargs):
            return _Completed(returncode=0, stdout="not,enough\n")

        assert hardware.read_gpu(runner=fake_runner) is None

    def test_returns_none_on_empty_stdout(self, monkeypatch):
        monkeypatch.setattr(hardware.shutil, "which", lambda name: "/usr/bin/nvidia-smi")

        @dataclass
        class _Completed:
            returncode: int
            stdout: str
            stderr: str = ""

        def fake_runner(cmd, **kwargs):
            return _Completed(returncode=0, stdout="")

        assert hardware.read_gpu(runner=fake_runner) is None


class TestReadSnapshot:
    def test_combines_cpu_ram_and_gpu(self, monkeypatch):
        monkeypatch.setattr(hardware.shutil, "which", lambda name: None)  # no GPU
        snapshot = hardware.read_snapshot()
        assert snapshot.cpu.core_count >= 1
        assert snapshot.ram.total_gb > 0
        assert snapshot.gpu is None


class TestRenderSystemLine:
    def _snapshot(self, gpu=None):
        return hardware.HardwareSnapshot(
            cpu=hardware.CpuInfo(core_count=12, usage_percent=34.0),
            ram=hardware.RamInfo(total_gb=15.6, used_gb=6.1),
            gpu=gpu,
        )

    def test_matches_blueprint_cpu_ram_shape(self):
        text = hardware.render_system_line(self._snapshot())
        assert "CPU: 12 cores, 34% used" in text
        assert "RAM: 15.6GB total, 6.1GB used" in text

    def test_omits_gpu_when_none(self):
        text = hardware.render_system_line(self._snapshot(gpu=None))
        assert "GPU" not in text

    def test_includes_gpu_when_present(self):
        gpu = hardware.GpuInfo(name="NVIDIA RTX 4090", vram_total_mb=24576.0, vram_used_mb=2048.0)
        text = hardware.render_system_line(self._snapshot(gpu=gpu))
        assert "NVIDIA RTX 4090" in text
        assert "2048MB/24576MB" in text