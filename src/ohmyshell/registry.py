"""
Capability Registry (Build Order Step 3).

Loads capabilities/capabilities.json and validates it against a fixed
meta-schema at load time (Section 5.2/5.4).

Not currently wired into the app — the Intent Parser now generates
open-ended commands directly rather than mapping to a fixed capability set
(see intent_parser.py). Kept for the earlier fixed-action architecture it
was built for.

This is deliberately separate from the per-call constrained-decoding step —
that would validate one model response against one capability's
params_schema at request time. This module instead validates the registry
file itself, once, at load time: every entry must be structurally
well-formed (required keys present, risk one of the known levels, its own
params_schema a syntactically valid JSON Schema, etc.) before anything else
is allowed to trust it.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import jsonschema

# Default location of the registry file, relative to the installed package.
# capabilities/ sits at the repo root next to src/, not inside the package
# itself (Section 5.2's file tree) — this resolves that path from this
# module's own location so it works both from a source checkout and once
# packaged.
DEFAULT_CAPABILITIES_PATH = (
    Path(__file__).resolve().parents[2] / "capabilities" / "capabilities.json"
)

KNOWN_RISK_LEVELS = ("low", "medium", "high")

# Meta-schema: validates the *shape of capabilities.json itself*, not any
# one capability's params against a user/model request. Each capability's
# own "params_schema" only needs to be a syntactically valid JSON Schema
# object here — its keywords are checked against actual params later, by
# whichever module does the per-call constrained-decoding/validation
# (Build Order Step 4/5), not here.
REGISTRY_META_SCHEMA: dict[str, Any] = {
    "type": "object",
    "required": ["version", "capabilities"],
    "properties": {
        "$schema": {"type": "string"},
        "version": {"type": "string"},
        "capabilities": {
            "type": "array",
            "minItems": 1,
            "items": {
                "type": "object",
                "required": [
                    "action",
                    "description",
                    "risk",
                    "params_schema",
                    "command_template",
                ],
                "properties": {
                    "action": {"type": "string", "minLength": 1},
                    "description": {"type": "string", "minLength": 1},
                    "risk": {"type": "string", "enum": list(KNOWN_RISK_LEVELS)},
                    "params_schema": {"type": "object"},
                    "command_template": {"type": "string", "minLength": 1},
                    "few_shot_examples": {
                        "type": "array",
                        "items": {"type": "string"},
                    },
                    # Optional (Build Order Step 7): human-readable plan step
                    # templates for the Plan Generator (Section 8.3.3), one
                    # string per step, with {param} placeholders formatted
                    # against the validated intent's params — e.g.
                    # "Scan {paths} for files older than {days} days".
                    # A capability with no plan_steps falls back to a
                    # single generic step built from its description.
                    "plan_steps": {
                        "type": "array",
                        "items": {"type": "string", "minLength": 1},
                        "minItems": 1,
                    },
                },
                "additionalProperties": True,
            },
        },
    },
    "additionalProperties": True,
}


class RegistryError(Exception):
    """Raised when capabilities.json is missing, unreadable, or fails validation."""


class Registry:
    """
    A loaded, validated capability registry.

    Construct via `load()` rather than directly — that's what runs the
    integrity check and duplicate-action check before handing back an
    instance callers can trust.
    """

    def __init__(self, raw: dict[str, Any]):
        self._raw = raw
        self._by_action: dict[str, dict[str, Any]] = {
            entry["action"]: entry for entry in raw["capabilities"]
        }

    @property
    def version(self) -> str:
        return self._raw["version"]

    def actions(self) -> list[str]:
        """All registered action names, in registry file order."""
        return [entry["action"] for entry in self._raw["capabilities"]]

    def all_capabilities(self) -> list[dict[str, Any]]:
        """All capability entries, in registry file order."""
        return list(self._raw["capabilities"])

    def get(self, action: str) -> dict[str, Any]:
        """
        Look up a single capability entry by action name.

        Raises:
            KeyError: if `action` is not a registered capability.
        """
        try:
            return self._by_action[action]
        except KeyError:
            raise KeyError(f"Unknown capability action: {action!r}") from None

    def risk_for(self, action: str) -> str:
        """
        The static, hardcoded risk level for `action` ("low" | "medium" | "high").

        This is the *only* place risk should be read from for an action —
        never from model output, per Section 7.4.

        Raises:
            KeyError: if `action` is not a registered capability.
        """
        return self.get(action)["risk"]

    def params_schema_for(self, action: str) -> dict[str, Any]:
        """The JSON Schema describing this action's expected parameters."""
        return self.get(action)["params_schema"]

    def command_template_for(self, action: str) -> str:
        """The shell command template for this action, with `{param}` placeholders."""
        return self.get(action)["command_template"]

    def plan_steps_for(self, action: str) -> list[str] | None:
        """
        The registered plan-step templates for this action (Section 8.3.3),
        or None if the capability doesn't define any — callers (plan_generator.py)
        are expected to fall back to a generic single-step plan in that case.
        """
        return self.get(action).get("plan_steps")

    def __contains__(self, action: str) -> bool:
        return action in self._by_action

    def __len__(self) -> int:
        return len(self._by_action)


def load(path: Path | str | None = None) -> Registry:
    """
    Load and validate capabilities.json, returning a Registry.

    Args:
        path: override path to a capabilities.json file. Defaults to
            capabilities/capabilities.json at the repo root.

    Raises:
        RegistryError: if the file is missing, isn't valid JSON, fails the
            meta-schema check, or contains duplicate action names.
    """
    resolved = Path(path) if path is not None else DEFAULT_CAPABILITIES_PATH

    if not resolved.exists():
        raise RegistryError(f"Capability registry not found at {resolved}")

    try:
        raw_text = resolved.read_text(encoding="utf-8")
        data = json.loads(raw_text)
    except (OSError, json.JSONDecodeError) as exc:
        raise RegistryError(f"Could not read {resolved}: {exc}") from exc

    try:
        jsonschema.validate(instance=data, schema=REGISTRY_META_SCHEMA)
    except jsonschema.ValidationError as exc:
        raise RegistryError(
            f"{resolved} failed registry integrity check: {exc.message} "
            f"(at {'/'.join(str(p) for p in exc.absolute_path) or '<root>'})"
        ) from exc

    # jsonschema doesn't check uniqueness across a specific field on its own;
    # duplicate action names would otherwise silently shadow each other.
    seen: set[str] = set()
    duplicates: set[str] = set()
    for entry in data["capabilities"]:
        action = entry["action"]
        if action in seen:
            duplicates.add(action)
        seen.add(action)
    if duplicates:
        raise RegistryError(
            f"{resolved} has duplicate capability action name(s): {sorted(duplicates)}"
        )

    # Each entry's own params_schema must itself be a syntactically valid
    # JSON Schema (not validated against any instance yet — just that it
    # could be used as one later, by Step 4/5's harness validation).
    invalid_param_schemas: list[str] = []
    for entry in data["capabilities"]:
        try:
            jsonschema.Draft202012Validator.check_schema(entry["params_schema"])
        except jsonschema.SchemaError:
            invalid_param_schemas.append(entry["action"])
    if invalid_param_schemas:
        raise RegistryError(
            f"{resolved} has invalid params_schema for action(s): {invalid_param_schemas}"
        )

    return Registry(data)