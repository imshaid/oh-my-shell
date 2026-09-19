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

--- Disclosed gap (Section 16 Rule 5) ---
The hardware-tiering table (which tier -> which specs -> which recommended
local models, needed by the wizard in Step 13) was never captured in this
project's transcript -- it falls in the same 501-699 gap documented
elsewhere (the user's own guess that it was "Section 9" was also checked
and is incorrect; the real Section 9 is "Team Structure", unrelated). This
module does NOT implement tiering or model recommendation -- it only
reports raw hardware facts (CPU cores/usage, RAM total/used, GPU presence/
kind/VRAM if detectable). Step 13's wizard will need the real tiering table
from the blueprint before it can turn these facts into a tier/model choice;
implementing that classification now would mean inventing thresholds the
blueprint doesn't state, which Section 16 Rule 5 says to avoid guessing on
without documenting it as an assumption -- here there isn't even enough
signal to make a defensible default, so it's left undone rather than faked.

GPU detection approach (undocumented in the retrieved blueprint, therefore
my own design, per Rule 5): a discrete NVIDIA GPU is detected by shelling
out to `nvidia-smi` (the standard, dependency-free way to query NVIDIA GPU
name/VRAM without pulling in a GPU vendor SDK); when `nvidia-smi` isn't on
PATH or errors, this is treated as "no discrete GPU detected" -- exactly
the "integrated GPU-তে চুপচাপ hide" case, so `read_gpu()` returns None
rather than raising, and callers (the /system renderer, the live indicator)
simply omit the GPU line when it's None. AMD/Intel discrete GPU detection
is out of scope here (no tool/library was specified for it either) -- the
same "don't guess a mechanism the blueprint never named" reasoning applies.
"""

from __future__ import annotations

import shutil
import subprocess
from dataclasses import dataclass

import psutil


@dataclass(frozen=True)
class CpuInfo:
    core_count: int
    usage_percent: float


@dataclass(frozen=True)
class RamInfo:
    total_gb: float
    used_gb: float


@dataclass(frozen=True)
class GpuInfo:
    name: str
    vram_total_mb: float
    vram_used_mb: float


def read_cpu() -> CpuInfo:
    """
    Current CPU core count and utilization.

    `psutil.cpu_percent(interval=...)` blocks for `interval` seconds to
    measure usage over that window; a short, fixed interval keeps `/system`
    responsive while still giving a meaningful (non-zero-on-first-call)
    reading, matching the blueprint's example ("34% used") being a real
    instantaneous load figure, not a static/cached one.
    """
    return CpuInfo(
        core_count=psutil.cpu_count(logical=True) or 1,
        usage_percent=psutil.cpu_percent(interval=0.1),
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
    Discrete NVIDIA GPU name + VRAM usage, or None if no discrete GPU is
    detected (integrated-GPU-or-none case -- silently hidden per Section 6's
    Core Feature #14, not an error).

    `runner` is injectable for testing without a real `nvidia-smi` binary.
    """
    if not _nvidia_smi_available():
        return None

    try:
        completed = runner(
            [
                "nvidia-smi",
                "--query-gpu=name,memory.total,memory.used",
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
    if len(parts) != 3:
        return None

    name, total_str, used_str = parts
    try:
        return GpuInfo(name=name, vram_total_mb=float(total_str), vram_used_mb=float(used_str))
    except ValueError:
        return None


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