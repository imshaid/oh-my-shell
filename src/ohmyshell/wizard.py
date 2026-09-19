"""
First-run setup wizard (Build Order Step 13, half of "wizard.py + install.sh").

Section 10.1's own line: "wizard.py + install.sh — first-run experience" --
this module is what runs the first time Oh My Shell starts with no
config.json yet, picking a sensible starting model for the user's hardware
before dropping them into the normal REPL (main.py, Step 6).

--- Disclosed gap (Section 16 Rule 5) ---
The blueprint's hardware-tier -> recommended-model table was never captured
in this project's transcript (the same 501-699 document gap already
documented in audit_log.py/hardware.py/ui/panels.py/ui/streaming.py -- the
user independently checked and confirmed this gap before this step began).
Rather than block indefinitely on text that isn't available, the user
confirmed proceeding with a documented default tier table (Section 16 Rule
5), to be swapped for the real blueprint table once it's available -- nothing
downstream depends on the exact thresholds, only on `choose_model()`'s
signature (HardwareSnapshot -> one of config.py's four known model names).

Documented default tier table (this module's own choice, not from the
blueprint): built from config.py's own DEFAULT_CONFIG["model"]["available"]
list (["qwen3:8b", "qwen3.5:4b", "phi4-mini", "lfm2.5-8b-a1b"]) -- so the
wizard never recommends a model config.py doesn't already know about --
ordered from lightest to heaviest hardware requirement and chosen by a
simple RAM (and, where available, discrete-GPU) threshold:

    RAM < 8 GB                          -> phi4-mini      (smallest, CPU-only)
    RAM 8-16 GB, no discrete GPU         -> qwen3.5:4b     (mid-size, CPU-only)
    RAM 8-16 GB, discrete GPU present    -> lfm2.5-8b-a1b  (mid-size, GPU-accelerated)
    RAM >= 16 GB                         -> qwen3:8b       (largest, this project's
                                                             own documented default
                                                             active model in config.py)

The presence of a discrete GPU (hardware.read_gpu() returning non-None) is
used only as a tie-breaker in the middle band, not to jump straight to the
biggest model -- VRAM capacity isn't validated against the model's actual
size requirement anywhere (no such table exists to validate against), so
this stays conservative rather than potentially recommending something too
large for the detected GPU's memory.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Callable

from ohmyshell import config as config_module
from ohmyshell import hardware as hardware_module

# Tier thresholds, in GB of total system RAM (see module docstring for the
# full documented-default table and its rationale).
_LOW_RAM_THRESHOLD_GB = 8.0
_MID_RAM_THRESHOLD_GB = 16.0


def choose_model(snapshot: hardware_module.HardwareSnapshot) -> str:
    """
    Pick one of config.py's four known models for this hardware snapshot,
    per the documented default tier table (module docstring).
    """
    available = config_module.DEFAULT_CONFIG["model"]["available"]
    ram_gb = snapshot.ram.total_gb
    has_gpu = snapshot.gpu is not None

    if ram_gb < _LOW_RAM_THRESHOLD_GB:
        choice = "phi4-mini"
    elif ram_gb < _MID_RAM_THRESHOLD_GB:
        choice = "lfm2.5-8b-a1b" if has_gpu else "qwen3.5:4b"
    else:
        choice = "qwen3:8b"

    # Defensive: never recommend a model config.py doesn't list as available.
    return choice if choice in available else available[0]


@dataclass(frozen=True)
class WizardResult:
    snapshot: hardware_module.HardwareSnapshot
    chosen_model: str
    config: dict


def render_welcome_text(result: WizardResult) -> str:
    """
    Plain-text first-run summary (the `rich` version is ui/panels.py's
    territory, same split used throughout this codebase; Step 11 predates
    this step but nothing there currently renders a wizard-specific panel,
    so this stays plain text until that's added).
    """
    snap = result.snapshot
    lines = [
        "Welcome to Oh My Shell — first-run setup",
        "",
        f"Detected: {snap.cpu.core_count} CPU cores, {snap.ram.total_gb}GB RAM"
        + (f", GPU: {snap.gpu.name}" if snap.gpu is not None else ""),
        f"Recommended model: {result.chosen_model}",
        "",
        "You can change this any time with `/model` or `/config set model.active <name>`.",
    ]
    return "\n".join(lines)


def run_wizard(
    *,
    read_hardware: Callable[[], hardware_module.HardwareSnapshot] = hardware_module.read_snapshot,
    print_fn: Callable[[str], None] = print,
    save_config: bool = True,
) -> WizardResult:
    """
    Run the first-run wizard: detect hardware, choose a starting model,
    write it into a fresh config.json (via config.py, the single source of
    truth for that file -- this function never writes config.json directly),
    and print a short welcome summary.

    `save_config=False` lets a caller preview the wizard's choice without
    touching disk (e.g. a test, or a future `/wizard --dry-run` meta-command).
    """
    snapshot = read_hardware()
    chosen_model = choose_model(snapshot)

    cfg = config_module.default_config()
    config_module.set_value(cfg, "model.active", chosen_model)

    if save_config:
        config_module.save(cfg)

    result = WizardResult(snapshot=snapshot, chosen_model=chosen_model, config=cfg)
    print_fn(render_welcome_text(result))
    return result


def should_run_wizard(*, config_path: "config_module.Path | None" = None) -> bool:
    """
    True if this looks like a genuine first run -- no config.json exists
    yet. main.py (Step 6/14) is expected to call this before config.load()
    and run_wizard() first if it's True, since config.load() itself would
    otherwise silently create a default config file without ever asking
    the user anything (Step 2's own documented behavior).
    """
    path = config_path if config_path is not None else config_module.CONFIG_PATH
    return not path.exists()