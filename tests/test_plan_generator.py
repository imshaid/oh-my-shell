"""
Tests for plan_generator.py (Build Order Step 7), rewritten for the
open-ended architecture (see plan_generator.py's own module docstring).

There is no registry/plan_steps template lookup any more -- generate_plan()
takes only the ValidatedIntent and builds a single-entry `steps` list
directly from intent.explanation (falling back to intent.command when no
explanation was given).
"""

import pytest

from ohmyshell.plan_generator import Plan, generate_plan
from ohmyshell.validation import ValidatedIntent


def test_plan_uses_explanation_as_its_single_step():
    intent = ValidatedIntent(
        command="find /tmp -mtime +7 -delete",
        risk="medium",
        explanation="Delete files in /tmp older than 7 days.",
    )

    plan = generate_plan(intent)

    assert plan.steps == ["Delete files in /tmp older than 7 days."]
    assert plan.command == "find /tmp -mtime +7 -delete"
    assert plan.risk == "medium"
    assert plan.explanation == "Delete files in /tmp older than 7 days."
    assert plan.estimate is None  # Executor's job, not this step's


def test_plan_falls_back_to_command_when_explanation_is_empty():
    intent = ValidatedIntent(command="ls -la", risk="low", explanation="")

    plan = generate_plan(intent)

    assert plan.steps == ["ls -la"]


def test_plan_falls_back_to_command_when_explanation_is_only_whitespace():
    intent = ValidatedIntent(command="ls -la", risk="low", explanation="   ")

    plan = generate_plan(intent)

    assert plan.steps == ["ls -la"]


def test_plan_strips_explanation_whitespace():
    intent = ValidatedIntent(command="ls -la", risk="low", explanation="  List files.  ")

    plan = generate_plan(intent)

    assert plan.steps == ["List files."]


def test_plan_always_has_exactly_one_step():
    intent = ValidatedIntent(command="whoami", risk="low", explanation="Show the current user.")

    plan = generate_plan(intent)

    assert len(plan.steps) == 1


def test_plan_command_risk_explanation_carried_through_unchanged():
    intent = ValidatedIntent(command="uptime", risk="low", explanation="Show system uptime.")

    plan = generate_plan(intent)

    assert plan.command == intent.command
    assert plan.risk == intent.risk
    assert plan.explanation == intent.explanation


def test_plan_is_a_frozen_dataclass_instance():
    intent = ValidatedIntent(command="ls", risk="low", explanation="")
    plan = generate_plan(intent)
    assert isinstance(plan, Plan)
    with pytest.raises(Exception):
        plan.risk = "high"  # frozen — must not be mutable after creation


# --- Removed tests (no longer applicable) -------------------------------------
#
# test_clean_temp_files_plan_matches_section_8_3_3_example,
# test_organize_files_plan_uses_registered_steps,
# test_list_processes_plan_single_step, test_kill_process_plan_uses_
# registered_steps, test_plan_params_and_action_carried_through_unchanged,
# test_generic_fallback_for_capability_without_plan_steps,
# test_generic_fallback_with_no_params, and
# test_missing_placeholder_param_leaves_template_unfilled all tested
# registry-driven plan_steps template rendering (generate_plan(intent,
# registry)) -- plan_generator.py no longer takes a registry at all and has
# no template-rendering logic left (see its own module docstring: "the
# plan" is now just the model's own explanation, or the command itself).