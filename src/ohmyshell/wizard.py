"""
First-run setup wizard (Build Order Step 13, half of "wizard.py + install.sh").

Runs the first time Oh My Shell starts with no config.json yet: detects
hardware (shown to the user, informational only — no model tier to pick any
more, since Google AI Studio is the only provider), collects and verifies a
Google AI Studio API key, saves it to .env, writes a default config.json,
and prints a short welcome summary before main.py drops into the normal
REPL.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Callable

from ohmyshell import config as config_module
from ohmyshell import hardware as hardware_module

ENV_PATH = Path.home() / ".oh-my-shell" / ".env"

DEFAULT_MODEL = "gemini-3.5-flash-lite"


class ApiKeyVerificationError(Exception):
    """Raised when a given API key fails a live verification call."""


def verify_api_key(api_key: str, *, verify_fn: Callable[[str], bool] | None = None) -> bool:
    """
    Verify an API key with a real (minimal) call to Google AI Studio.

    `verify_fn` overrides the real network call for testing; defaults to a
    small live request against Gemini.
    """
    if verify_fn is not None:
        return verify_fn(api_key)

    from google import genai
    from google.genai import types

    try:
        client = genai.Client(api_key=api_key)
        client.models.generate_content(
            model=DEFAULT_MODEL,
            contents="ping",
            config=types.GenerateContentConfig(
                max_output_tokens=1,
                automatic_function_calling=types.AutomaticFunctionCallingConfig(disable=True),
            ),
        )
    except Exception as exc:
        raise ApiKeyVerificationError(str(exc)) from exc
    return True


def save_api_key(api_key: str, *, env_path: Path | None = None) -> None:
    """Write GOOGLE_AI_STUDIO_API_KEY to .env (creating its parent dir if needed)."""
    path = env_path if env_path is not None else ENV_PATH
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(f"GOOGLE_AI_STUDIO_API_KEY={api_key}\n", encoding="utf-8")


@dataclass(frozen=True)
class WizardResult:
    snapshot: hardware_module.HardwareSnapshot
    api_key_saved: bool
    config: dict


def render_welcome_text(result: WizardResult) -> str:
    """Plain-text first-run summary (the `rich` version is ui/panels.py's territory)."""
    snap = result.snapshot
    lines = [
        "Welcome to Oh My Shell — first-run setup",
        "",
        f"Detected: {snap.cpu.core_count} CPU cores, {snap.ram.total_gb}GB RAM"
        + (f", GPU: {snap.gpu.name}" if snap.gpu is not None else ""),
        f"Model: {DEFAULT_MODEL}",
        "",
        "You can change the active model any time with `/config set model.active <name>`.",
    ]
    return "\n".join(lines)


def run_wizard(
    *,
    read_hardware: Callable[[], hardware_module.HardwareSnapshot] = hardware_module.read_snapshot,
    read_api_key: Callable[[], str] = lambda: input("Paste your Google AI Studio API key: "),
    verify_fn: Callable[[str], bool] | None = None,
    print_fn: Callable[[str], None] = print,
    save_config: bool = True,
    env_path: Path | None = None,
) -> WizardResult:
    """
    Run the first-run wizard: detect hardware (informational), collect and
    verify a Google AI Studio API key, save it to .env, write a fresh
    config.json (via config.py, the single source of truth for that file),
    and print a short welcome summary.

    `save_config=False` lets a caller preview the wizard's choice without
    touching disk (e.g. a test, or a future `/wizard --dry-run` meta-command).
    """
    snapshot = read_hardware()

    api_key = read_api_key()
    verify_api_key(api_key, verify_fn=verify_fn)

    api_key_saved = False
    if save_config:
        save_api_key(api_key, env_path=env_path)
        api_key_saved = True

    cfg = config_module.default_config()
    if save_config:
        config_module.save(cfg)

    result = WizardResult(snapshot=snapshot, api_key_saved=api_key_saved, config=cfg)
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