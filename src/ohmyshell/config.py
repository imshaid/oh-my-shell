"""
Config file load/save (Build Order Step 2).

Path: ~/.oh-my-shell/config.json  (Section 5.5)

Responsibilities:
- Know the default config shape (used on first run / missing keys).
- Load config.json from disk, creating it with defaults if it doesn't exist
  yet (the wizard, Build Order Step 13, is what normally does the *first*
  creation with hardware-aware model choices — this module's default here
  is the static fallback shape, not a hardware-tiered pick).
- Save config back to disk.
- Dot-notation get/set for `/config set <k> <v>` (Section 8.4), e.g.
  `trash.retention_days` -> config["trash"]["retention_days"].

No other module should read/write ~/.oh-my-shell/config.json directly —
everything goes through this module (single source of truth for the file).
"""

from __future__ import annotations

import copy
import json
from pathlib import Path
from typing import Any

CONFIG_DIR = Path.home() / ".oh-my-shell"
CONFIG_PATH = CONFIG_DIR / "config.json"

# Exact default shape from Section 5.5. This is a *shape* reference —
# the wizard (Step 13) may write a different `model.active` / `model.available`
# pair depending on detected hardware tier (Section 9's tiering), but every
# other default here is the literal blueprint value.
DEFAULT_CONFIG: dict[str, Any] = {
    "model": {
        "active": "gemini-3.5-flash-lite",
        "available": ["gemini-3.5-flash-lite", "gemini-3.1-flash-lite"],
    },
    "safety": {
        "safe_mode": False,
        "danger_classifier_sensitivity": "normal",
    },
    "trash": {
        "retention_days": 8,
    },
    "ui": {
        "quiet": False,
        "verbose": False,
    },
    "log": {
        "level": "normal",
    },
    "discussion": {
        "soft_limit_turns": 5,
    },
}


class ConfigError(Exception):
    """Raised when config.json exists but is unreadable or malformed."""


def default_config() -> dict[str, Any]:
    """Return a fresh deep copy of the default config shape."""
    return copy.deepcopy(DEFAULT_CONFIG)


def _deep_merge_defaults(config: dict[str, Any], defaults: dict[str, Any]) -> dict[str, Any]:
    """
    Fill in any top-level/nested keys missing from `config` using `defaults`,
    without touching keys the user has already set. This lets older
    config.json files on disk gain new default fields after an update
    without the user having to regenerate the whole file.
    """
    merged = copy.deepcopy(config)
    for key, default_value in defaults.items():
        if key not in merged:
            merged[key] = copy.deepcopy(default_value)
        elif isinstance(default_value, dict) and isinstance(merged.get(key), dict):
            merged[key] = _deep_merge_defaults(merged[key], default_value)
    return merged


def load() -> dict[str, Any]:
    """
    Load config.json, creating it with defaults on first run.

    Raises:
        ConfigError: if the file exists but contains invalid JSON or its
            top level isn't a JSON object. Callers (e.g. the wizard or
            main.py startup) decide whether to surface this to the user
            or offer to reset to defaults — this function does not silently
            overwrite a broken file.
    """
    if not CONFIG_PATH.exists():
        config = default_config()
        save(config)
        return config

    try:
        raw = CONFIG_PATH.read_text(encoding="utf-8")
        loaded = json.loads(raw)
    except (OSError, json.JSONDecodeError) as exc:
        raise ConfigError(f"Could not read {CONFIG_PATH}: {exc}") from exc

    if not isinstance(loaded, dict):
        raise ConfigError(f"{CONFIG_PATH} does not contain a JSON object at its top level.")

    return _deep_merge_defaults(loaded, DEFAULT_CONFIG)


def save(config: dict[str, Any]) -> None:
    """Write `config` to config.json, creating ~/.oh-my-shell/ if needed."""
    CONFIG_DIR.mkdir(parents=True, exist_ok=True)
    CONFIG_PATH.write_text(json.dumps(config, indent=2) + "\n", encoding="utf-8")


def get(config: dict[str, Any], dotted_key: str) -> Any:
    """
    Read a value by dot-notation key, e.g. get(config, "trash.retention_days").

    Raises:
        KeyError: if any segment of the path doesn't exist.
    """
    node: Any = config
    for part in dotted_key.split("."):
        if not isinstance(node, dict) or part not in node:
            raise KeyError(dotted_key)
        node = node[part]
    return node


def set_value(config: dict[str, Any], dotted_key: str, value: Any) -> dict[str, Any]:
    """
    Set a value by dot-notation key, e.g.
    set_value(config, "trash.retention_days", 14).

    Mutates and returns `config`. Intermediate segments must already exist
    as dicts (this sets leaf values in the known schema; it does not
    fabricate new nested structure) — callers such as the `/config set`
    meta-command (Section 8.4) are expected to validate the key against
    the known schema before calling this, and to parse `value` to the
    right type (this function stores whatever it's given as-is).

    Raises:
        KeyError: if an intermediate segment doesn't exist or isn't a dict.
    """
    parts = dotted_key.split(".")
    node = config
    for part in parts[:-1]:
        if not isinstance(node, dict) or part not in node:
            raise KeyError(dotted_key)
        node = node[part]
    if not isinstance(node, dict):
        raise KeyError(dotted_key)
    node[parts[-1]] = value
    return config