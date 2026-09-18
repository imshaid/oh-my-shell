"""Tests for plan_generator.py (Build Order Step 7)."""

import pytest

from ohmyshell import registry as registry_module
from ohmyshell.plan_generator import Plan, generate_plan
from ohmyshell.validation import ValidatedIntent


@pytest.fixture(scope="module")
def registry():
    return registry_module.load()


def test_clean_temp_files_plan_matches_section_8_3_3_example(registry):
    """The blueprint's own example plan (Section 8.3.3), reproduced via generate_plan."""
    intent = ValidatedIntent(
        action="clean_temp_files",
        params={"days": 7, "paths": ["/tmp", "~/.cache"]},
        risk="medium",
    )

    plan = generate_plan(intent, registry)

    assert plan.steps == [
        "Scan ['/tmp', '~/.cache'] for files older than 7 days",
        "Calculate total reclaimable space",
        "Move matched files to .trash/ (recoverable)",
    ]
    assert plan.risk == "medium"
    assert plan.action == "clean_temp_files"
    assert plan.estimate is None  # Executor's job, not this step's — see module docstring


def test_organize_files_plan_uses_registered_steps(registry):
    intent = ValidatedIntent(
        action="organize_files", params={"target_dir": "/home/user/Downloads"}, risk="low"
    )

    plan = generate_plan(intent, registry)

    assert plan.steps == [
        "Scan /home/user/Downloads for files",
        "Group files by extension",
        "Move each file into a subfolder named after its extension",
    ]


def test_list_processes_plan_single_step(registry):
    intent = ValidatedIntent(
        action="list_processes", params={"filter": "chrome", "sort_by": "cpu"}, risk="low"
    )

    plan = generate_plan(intent, registry)

    assert plan.steps == ['List running processes matching "chrome", sorted by cpu']


def test_kill_process_plan_uses_registered_steps(registry):
    intent = ValidatedIntent(
        action="kill_process", params={"target": "1234", "signal": "TERM"}, risk="high"
    )

    plan = generate_plan(intent, registry)

    assert plan.steps == [
        "Locate process matching 1234",
        "Send TERM signal to terminate it",
    ]
    assert plan.risk == "high"


def test_plan_params_and_action_carried_through_unchanged(registry):
    intent = ValidatedIntent(
        action="organize_files", params={"target_dir": "/tmp/x"}, risk="low"
    )

    plan = generate_plan(intent, registry)

    assert plan.action == intent.action
    assert plan.params == intent.params
    assert plan.risk == intent.risk


# --- Fallback: capability with no registered plan_steps -----------------------


def test_generic_fallback_for_capability_without_plan_steps():
    class FakeRegistry:
        def get(self, action):
            return {"action": action, "description": "does a thing"}

        def plan_steps_for(self, action):
            return None

    intent = ValidatedIntent(action="do_thing", params={"x": 1, "y": "z"}, risk="low")

    plan = generate_plan(intent, FakeRegistry())

    assert plan.steps == ["does a thing (x=1, y=z)"]


def test_generic_fallback_with_no_params():
    class FakeRegistry:
        def get(self, action):
            return {"action": action, "description": "does a thing"}

        def plan_steps_for(self, action):
            return None

    intent = ValidatedIntent(action="do_thing", params={}, risk="low")

    plan = generate_plan(intent, FakeRegistry())

    assert plan.steps == ["does a thing"]


# --- Template formatting edge cases --------------------------------------------


def test_missing_placeholder_param_leaves_template_unfilled():
    class FakeRegistry:
        def get(self, action):
            return {"action": action, "description": "does a thing"}

        def plan_steps_for(self, action):
            return ["Do the thing with {missing_param}"]

    intent = ValidatedIntent(action="do_thing", params={}, risk="low")

    plan = generate_plan(intent, FakeRegistry())

    # Left visible rather than raising — see _format_step's docstring.
    assert plan.steps == ["Do the thing with {missing_param}"]


def test_plan_is_a_frozen_dataclass_instance(registry):
    intent = ValidatedIntent(action="list_processes", params={}, risk="low")
    plan = generate_plan(intent, registry)
    assert isinstance(plan, Plan)
    with pytest.raises(Exception):
        plan.risk = "high"  # frozen — must not be mutable after creation