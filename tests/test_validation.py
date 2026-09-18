"""
Tests for validation.py (Build Order Step 4) — Harness Validation Layer.

Uses the real repo capabilities/capabilities.json (loaded via registry.load())
so these tests exercise the actual registered core-4 capabilities, not a
synthetic stand-in.
"""

import pytest

from ohmyshell import registry as registry_module
from ohmyshell import validation


@pytest.fixture(scope="module")
def registry():
    return registry_module.load()


# --- Happy path --------------------------------------------------------------


def test_valid_intent_passes_and_risk_comes_from_registry(registry):
    raw = {
        "action": "organize_files",
        "params": {"target_dir": "/home/user/Downloads"},
    }

    outcome = validation.validate_intent(raw, registry)

    assert outcome.ok is True
    assert outcome.error is None
    assert outcome.intent is not None
    assert outcome.intent.action == "organize_files"
    assert outcome.intent.params == {"target_dir": "/home/user/Downloads"}
    assert outcome.intent.risk == "low"  # from the registry, per organize_files' static risk


def test_valid_intent_with_reasoning_field_is_accepted(registry):
    """`reasoning` is optional free text the model may include; must not break validation."""
    raw = {
        "action": "list_processes",
        "params": {"filter": "chrome"},
        "reasoning": "The user asked to find chrome-related processes.",
    }

    outcome = validation.validate_intent(raw, registry)

    assert outcome.ok is True
    assert outcome.intent.action == "list_processes"


def test_params_with_only_defaults_used_is_valid(registry):
    """list_processes has no required params — an empty dict must validate."""
    raw = {"action": "list_processes", "params": {}}

    outcome = validation.validate_intent(raw, registry)

    assert outcome.ok is True


# --- The critical safety rule: risk is never taken from the model -----------


def test_risk_is_always_from_registry_never_from_model_output(registry):
    """
    Section 7.4 / line 502: even if a model response includes a risk-looking
    field, the harness must ignore it entirely and use the registry's static
    value. kill_process is registry-hardcoded as "high" risk.
    """
    raw = {
        "action": "kill_process",
        "params": {"target": "1234"},
        "risk": "low",  # model trying to claim this is low-risk — must be ignored
    }

    outcome = validation.validate_intent(raw, registry)

    assert outcome.ok is True
    assert outcome.intent.risk == "high"  # registry's static value wins, not the model's "low"


# --- Pass 1: malformed intent shape ------------------------------------------


def test_missing_action_field_fails(registry):
    outcome = validation.validate_intent({"params": {}}, registry)

    assert outcome.ok is False
    assert outcome.intent is None
    assert "malformed" in outcome.error.lower() or "action" in outcome.error.lower()


def test_empty_action_string_fails(registry):
    outcome = validation.validate_intent({"action": "", "params": {}}, registry)

    assert outcome.ok is False


def test_non_dict_params_fails(registry):
    outcome = validation.validate_intent(
        {"action": "organize_files", "params": "not-a-dict"}, registry
    )

    assert outcome.ok is False


def test_completely_wrong_shape_fails(registry):
    outcome = validation.validate_intent(["not", "an", "object"], registry)

    assert outcome.ok is False


# --- Pass 2: unknown action ---------------------------------------------------


def test_unknown_action_fails(registry):
    outcome = validation.validate_intent(
        {"action": "launch_nuclear_missiles", "params": {}}, registry
    )

    assert outcome.ok is False
    assert outcome.intent is None
    assert "launch_nuclear_missiles" in outcome.error


# --- Pass 3: params fail the action's own params_schema ----------------------


def test_missing_required_param_fails(registry):
    """organize_files requires target_dir."""
    outcome = validation.validate_intent(
        {"action": "organize_files", "params": {}}, registry
    )

    assert outcome.ok is False
    assert "organize_files" in outcome.error


def test_wrong_param_type_fails(registry):
    """clean_temp_files.days must be an integer."""
    outcome = validation.validate_intent(
        {"action": "clean_temp_files", "params": {"days": "a week"}}, registry
    )

    assert outcome.ok is False


def test_param_violating_enum_fails(registry):
    """kill_process.signal must be TERM or KILL."""
    outcome = validation.validate_intent(
        {"action": "kill_process", "params": {"target": "1234", "signal": "NUKE"}},
        registry,
    )

    assert outcome.ok is False


def test_param_violating_minimum_fails(registry):
    """clean_temp_files.days has minimum: 1."""
    outcome = validation.validate_intent(
        {"action": "clean_temp_files", "params": {"days": 0}}, registry
    )

    assert outcome.ok is False


def test_additional_unexpected_param_fails(registry):
    """All four core capabilities set additionalProperties: false."""
    outcome = validation.validate_intent(
        {
            "action": "list_processes",
            "params": {"filter": "x", "unexpected_extra_field": True},
        },
        registry,
    )

    assert outcome.ok is False


# --- RawIntent already-parsed input path -------------------------------------


def test_accepts_already_parsed_raw_intent_instance(registry):
    raw_intent = validation.RawIntent(action="list_processes", params={})

    outcome = validation.validate_intent(raw_intent, registry)

    assert outcome.ok is True
    assert outcome.intent.action == "list_processes"