"""
Hardware detection (Build Order Step 12, half of "audit_log.py + hardware.py").

Feeds the Live Hardware Load Indicator (Section 6, Core Feature #14,
verbatim): "**Live Hardware Load Indicator** — শুধু AI-thinking-window-এ
(CPU/RAM সবসময়, GPU tiered-detection সাপেক্ষে, integrated GPU-তে চুপচাপ
hide)" -- CPU/RAM are always read; GPU is read subject to tiered detection
and silently hidden (not shown as an error) when only an integrated GPU is
present. Also backs `/system`'s confirmed verbatim output (Section 8.3.8):

    OS: Ubuntu 24.04 LTS  ·  CPU: 12 cores, 34% used  ·  RAM: 15.6GB total, 6.1GB used

Where this indicator is actually rendered ("thinking-window-only") is a
ui/streaming.py (Step 11) concern once that module is wired into main.py's
natural-language path -- this module only provides the read_* functions;
it does not decide when they're called or how the result is displayed.

This module reports raw hardware facts only (CPU cores/usage, RAM
total/used, GPU presence/kind/VRAM if detectable) -- it does not implement
hardware-tiering or model recommendation; that is the wizard's job.

GPU detection: a discrete NVIDIA GPU is detected by shelling out to
`nvidia-smi`, the standard, dependency-free way to query GPU name/VRAM
without a vendor SDK. When `nvidia-smi` isn't on PATH or errors, this is
treated as "no discrete GPU detected" -- the integrated-GPU-or-none case --
so `read_gpu()` returns None rather than raising, and callers (the
/system renderer, the live indicator) simply omit the GPU line. AMD/Intel
discrete GPU detection is out of scope.

RAM has no model/part-name field: the only Linux mechanism for it
(`dmidecode -t memory`) requires root and fails with a permission error
for a normal user, with no unprivileged equivalent, so it is left out
entirely rather than shown only some of the time.
"""

from __future__ import annotations

import shutil
import subprocess
from dataclasses import dataclass
from pathlib import Path

import psutil

CPUINFO_PATH = Path("/proc/cpuinfo")


@dataclass(frozen=True)
class CpuInfo:
    core_count: int
    usage_percent: float
    temperature_celsius: float | None = None
    model_name: str | None = None
    fan_rpm: int | None = None


@dataclass(frozen=True)
class RamInfo:
    total_gb: float
    used_gb: float


@dataclass(frozen=True)
class GpuInfo:
    name: str
    vram_total_mb: float
    vram_used_mb: float
    temperature_celsius: float | None = None


def _read_cpu_temperature() -> float | None:
    """
    Best-effort CPU package temperature, for the Live Hardware Load
    Indicator's "CPU ▓▓▓▓▓▓▓░░░ 82% · 58°C" mockup (Section 8.3.3).

    `psutil.sensors_temperatures()` is Linux-only and depends on the
    kernel exposing `/sys/class/hwmon` sensors -- inside a container, a VM,
    or on hardware without exposed sensors, it legitimately returns `{}`
    (not an error). No sensor data means this returns None, and every
    caller (the live indicator, /system) omits the temperature rather than
    showing a fake or zero value. Picks the first "coretemp"/"k10temp"/
    "cpu_thermal"-style entry's first reading when present; exact
    sensor/label naming varies by CPU vendor, so a "first available
    CPU-like sensor" heuristic is used rather than an exhaustive table.
    """
    try:
        all_temps = psutil.sensors_temperatures()
    except (AttributeError, OSError):
        # Platform without sensors_temperatures at all (e.g. Windows/macOS).
        return None
    for label in ("coretemp", "k10temp", "cpu_thermal", "cpu-thermal"):
        entries = all_temps.get(label)
        if entries:
            return entries[0].current
    # Fall back to the first reported sensor group rather than reporting
    # nothing just because this machine's driver used an unlisted label.
    for entries in all_temps.values():
        if entries:
            return entries[0].current
    return None


def _read_cpu_model_name(*, cpuinfo_path: Path = CPUINFO_PATH) -> str | None:
    """
    CPU model name (e.g. "13th Gen Intel(R) Core(TM) i7-13650HX"), for the
    live indicator's trailing "· <model>" segment -- shown once per line
    rather than repeated per-refresh data, since unlike usage/temperature
    this never changes during a session.

    `/proc/cpuinfo`'s "model name" field is Linux-specific, read directly
    rather than through psutil, which has no cross-platform CPU-model API
    of its own. Missing file, missing field, or any read error all fall
    back to None, matching every other optional hardware field in this
    module (temperature, GPU, fan).
    """
    try:
        text = cpuinfo_path.read_text(encoding="utf-8")
    except OSError:
        return None
    for line in text.splitlines():
        if line.lower().startswith("model name"):
            _, _, value = line.partition(":")
            name = value.strip()
            return name or None
    return None


def _read_fan_rpm() -> int | None:
    """
    Best-effort primary fan speed (RPM), for the live indicator's
    "... · 2100 RPM" segment.

    `psutil.sensors_fans()` is Linux-only and, like the temperature
    sensors, legitimately returns `{}` on hardware/VMs/containers with no
    exposed fan sensor -- not an error, so this returns None and callers
    omit the segment rather than showing "0 RPM". Takes the first reported
    fan's first reading; a machine with several fans only shows the first,
    which is enough for "is the fan spinning and roughly how fast."
    """
    try:
        all_fans = psutil.sensors_fans()
    except (AttributeError, OSError):
        return None
    for entries in all_fans.values():
        if entries:
            return entries[0].current
    return None


def read_cpu() -> CpuInfo:
    """
    Current CPU core count, utilization, model name, and (best-effort)
    temperature/fan speed.

    `psutil.cpu_percent(interval=...)` blocks for `interval` seconds to
    measure usage over that window; a short, fixed interval keeps `/system`
    responsive while still giving a meaningful (non-zero-on-first-call)
    reading, matching the blueprint's example ("34% used") being a real
    instantaneous load figure, not a static/cached one.
    """
    return CpuInfo(
        core_count=psutil.cpu_count(logical=True) or 1,
        usage_percent=psutil.cpu_percent(interval=0.1),
        temperature_celsius=_read_cpu_temperature(),
        model_name=_read_cpu_model_name(),
        fan_rpm=_read_fan_rpm(),
    )


def read_ram() -> RamInfo:
    """Current total/used system RAM, in GB (matching /system's "15.6GB total, 6.1GB used")."""
    mem = psutil.virtual_memory()
    bytes_per_gb = 1024**3
    return RamInfo(
        total_gb=round(mem.total / bytes_per_gb, 1),
        used_gb=round(mem.used / bytes_per_gb, 1),
    )


def _nvidia_smi_available() -> bool:
    return shutil.which("nvidia-smi") is not None


def read_gpu(*, runner=subprocess.run) -> GpuInfo | None:
    """
    Discrete NVIDIA GPU name + VRAM usage + (best-effort) temperature, or
    None if no discrete GPU is detected (integrated-GPU-or-none case --
    silently hidden per Section 6's Core Feature #14, not an error).

    `runner` is injectable for testing without a real `nvidia-smi` binary.

    The query asks for a 4th field (`temperature.gpu`) alongside name/
    memory.total/memory.used, reported in the same call as one more
    comma-separated value. A malformed/short response (only 3 fields, e.g.
    an older nvidia-smi or a driver that doesn't report temperature)
    degrades to a GpuInfo with `temperature_celsius=None` rather than
    being rejected outright, since name/total/used are the fields this
    module has always depended on and temperature is optional.
    """
    if not _nvidia_smi_available():
        return None

    try:
        completed = runner(
            [
                "nvidia-smi",
                "--query-gpu=name,memory.total,memory.used,temperature.gpu",
                "--format=csv,noheader,nounits",
            ],
            capture_output=True,
            text=True,
            timeout=2,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None

    if completed.returncode != 0 or not completed.stdout.strip():
        return None

    first_line = completed.stdout.strip().splitlines()[0]
    parts = [p.strip() for p in first_line.split(",")]
    if len(parts) < 3:
        return None

    name, total_str, used_str = parts[0], parts[1], parts[2]
    temperature_str = parts[3] if len(parts) >= 4 else None
    try:
        vram_total_mb = float(total_str)
        vram_used_mb = float(used_str)
    except ValueError:
        return None

    temperature_celsius: float | None = None
    if temperature_str:
        try:
            temperature_celsius = float(temperature_str)
        except ValueError:
            temperature_celsius = None  # malformed temp field -- omit, don't reject the reading

    return GpuInfo(
        name=name,
        vram_total_mb=vram_total_mb,
        vram_used_mb=vram_used_mb,
        temperature_celsius=temperature_celsius,
    )


@dataclass(frozen=True)
class HardwareSnapshot:
    cpu: CpuInfo
    ram: RamInfo
    gpu: GpuInfo | None


def read_snapshot(*, runner=subprocess.run) -> HardwareSnapshot:
    """One combined read of everything /system and the live indicator need."""
    return HardwareSnapshot(cpu=read_cpu(), ram=read_ram(), gpu=read_gpu(runner=runner))


def render_system_line(snapshot: HardwareSnapshot) -> str:
    """
    Plain-text rendering matching /system's verbatim CPU/RAM portion
    (Section 8.3.8): "CPU: 12 cores, 34% used  ·  RAM: 15.6GB total, 6.1GB used"
    -- GPU appended only when present, since it's silently omitted otherwise.
    """
    parts = [
        f"CPU: {snapshot.cpu.core_count} cores, {snapshot.cpu.usage_percent:.0f}% used",
        f"RAM: {snapshot.ram.total_gb}GB total, {snapshot.ram.used_gb}GB used",
    ]
    if snapshot.gpu is not None:
        parts.append(
            f"GPU: {snapshot.gpu.name}, {snapshot.gpu.vram_used_mb:.0f}MB/{snapshot.gpu.vram_total_mb:.0f}MB VRAM"
        )
    return "  ·  ".join(parts)