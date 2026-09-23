"""
Tests for validation.py (Build Order Step 4) — Harness Validation Layer,
rewritten for the open-ended architecture (see validation.py's own module
docstring for the full rationale).

validate_intent() no longer takes a registry argument -- there is no
registry mapping a free-form command to anything any more. It is a pure
shape-checker: well-formed command/risk/explanation, nothing else. Risk is
no longer "always from the registry" (there is no registry risk lookup);
instead it's the model's own self-reported risk, normalized -- the
independent double-check now lives in danger_classifier.override_risk()
(covered in test_danger_classifier.py), not here.
"""

import pytest

from ohmyshell import validation


# --- Happy path --------------------------------------------------------------


def test_valid_intent_passes_and_fields_carry_through():
    raw = {
        "command": "ls -la ~/Downloads",
        "risk": "low",
        "explanation": "List files in Downloads.",
    }

    outcome = validation.validate_intent(raw)

    assert outcome.ok is True
    assert outcome.error is None
    assert outcome.intent is not None
    assert outcome.intent.command == "ls -la ~/Downloads"
    assert outcome.intent.risk == "low"
    assert outcome.intent.explanation == "List files in Downloads."


def test_risk_is_normalized_to_lowercase():
    raw = {"command": "ls", "risk": "LOW", "explanation": ""}

    outcome = validation.validate_intent(raw)

    assert outcome.ok is True
    assert outcome.intent.risk == "low"


def test_explanation_defaults_to_empty_string_when_omitted():
    raw = {"command": "ls", "risk": "low"}

    outcome = validation.validate_intent(raw)

    assert outcome.ok is True
    assert outcome.intent.explanation == ""


def test_command_is_stripped_of_surrounding_whitespace():
    raw = {"command": "  ls -la  ", "risk": "low", "explanation": ""}

    outcome = validation.validate_intent(raw)

    assert outcome.ok is True
    assert outcome.intent.command == "ls -la"


# --- The model's own risk field is accepted, not overridden here ------------
#
# Under the open-ended architecture there is no registry to override risk
# with -- validate_intent() is a pure shape-checker, so it must pass the
# model's own risk straight through (still normalized). The independent
# "never trust the model's own risk blindly" safety property is preserved
# by a DIFFERENT layer -- danger_classifier.override_risk() -- exercised in
# test_danger_classifier.py, not here.


def test_model_supplied_risk_is_carried_through_unmodified():
    raw = {"command": "rm -rf /home/user/.cache", "risk": "low", "explanation": "Clear cache."}

    outcome = validation.validate_intent(raw)

    assert outcome.ok is True
    assert outcome.intent.risk == "low"  # this module does not second-guess it


# --- Malformed intent shape ---------------------------------------------------


def test_missing_command_field_fails():
    outcome = validation.validate_intent({"risk": "low"})

    assert outcome.ok is False
    assert outcome.intent is None
    assert "malformed" in outcome.error.lower()


def test_empty_command_string_fails():
    outcome = validation.validate_intent({"command": "", "risk": "low"})

    assert outcome.ok is False


def test_blank_whitespace_only_command_fails():
    outcome = validation.validate_intent({"command": "   ", "risk": "low"})

    assert outcome.ok is False
    assert "empty" in outcome.error.lower() or "blank" in outcome.error.lower()


def test_missing_risk_field_fails():
    outcome = validation.validate_intent({"command": "ls"})

    assert outcome.ok is False


def test_completely_wrong_shape_fails():
    outcome = validation.validate_intent(["not", "an", "object"])

    assert outcome.ok is False


# --- Invalid risk enum ---------------------------------------------------------


def test_risk_not_in_known_levels_fails():
    outcome = validation.validate_intent({"command": "ls", "risk": "catastrophic", "explanation": ""})

    assert outcome.ok is False
    assert outcome.intent is None
    assert "catastrophic" in outcome.error


def test_all_three_known_risk_levels_are_accepted():
    for level in ("low", "medium", "high"):
        outcome = validation.validate_intent({"command": "ls", "risk": level, "explanation": ""})
        assert outcome.ok is True
        assert outcome.intent.risk == level


# --- RawIntent already-parsed input path -------------------------------------


def test_accepts_already_parsed_raw_intent_instance():
    raw_intent = validation.RawIntent(command="ls -la", risk="low")

    outcome = validation.validate_intent(raw_intent)

    assert outcome.ok is True
    assert outcome.intent.command == "ls -la"


def test_raw_intent_instance_with_invalid_risk_still_fails():
    raw_intent = validation.RawIntent(command="ls", risk="extreme")

    outcome = validation.validate_intent(raw_intent)

    assert outcome.ok is False


# --- Params-shaped tests no longer applicable ---------------------------------
#
# Removed (no longer applicable under the open-ended architecture, no
# per-action params_schema exists to violate any more):
#   - test_non_dict_params_fails / test_unknown_action_fails /
#     test_missing_required_param_fails / test_wrong_param_type_fails /
#     test_param_violating_enum_fails / test_param_violating_minimum_fails /
#     test_additional_unexpected_param_fails / test_params_missing_optional_
#     field_gets_schema_default / test_params_all_defaults_used_fills_every_
#     default / test_params_explicit_value_is_never_overwritten_by_default /
#     test_params_property_with_no_schema_default_stays_absent
#   These all tested registry-driven params_schema validation, which
#   validate_intent() no longer performs (RawIntent/ValidatedIntent have no
#   `params` field at all). Their equivalent "empty command fails" /
#   "invalid risk enum fails" coverage is above.