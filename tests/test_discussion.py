"""
Tests for discussion.py (Build Order Step 7).

get_user_choice / get_adjustment_text / reparse are all fakes here — this
module is deliberately UI-framework-agnostic (see its docstring), so tests
drive it exactly the way a real terminal loop (Step 11) would.
"""

import pytest

from ohmyshell import registry as registry_module
from ohmyshell.discussion import (
    Cancelled,
    Confirmed,
    edit_step_param,
    render_plan_text,
    run_discussion,
)
from ohmyshell.plan_generator import Plan, generate_plan
from ohmyshell.validation import ValidatedIntent


@pytest.fixture(scope="module")
def registry():
    return registry_module.load()


@pytest.fixture
def clean_temp_plan(registry):
    intent = ValidatedIntent(action="clean_temp_files", params={"days": 7}, risk="medium")
    return generate_plan(intent, registry)


# --- render_plan_text ------------------------------------------------------------


def test_render_plan_text_includes_steps_and_risk(clean_temp_plan):
    text = render_plan_text(clean_temp_plan)
    assert "1." in text
    assert "Medium" in text
    for step in clean_temp_plan.steps:
        assert step in text


# --- run_discussion: confirm / cancel immediately ---------------------------------


def test_confirm_on_first_turn_returns_confirmed(clean_temp_plan):
    outcome = run_discussion(
        clean_temp_plan,
        get_user_choice=lambda plan: "confirm",
        get_adjustment_text=lambda: "",
        reparse=lambda text, plan: None,
        print_fn=lambda _: None,
    )

    assert isinstance(outcome, Confirmed)
    assert outcome.plan == clean_temp_plan


def test_cancel_on_first_turn_returns_cancelled(clean_temp_plan):
    outcome = run_discussion(
        clean_temp_plan,
        get_user_choice=lambda plan: "cancel",
        get_adjustment_text=lambda: "",
        reparse=lambda text, plan: None,
        print_fn=lambda _: None,
    )

    assert isinstance(outcome, Cancelled)


def test_choice_is_case_insensitive(clean_temp_plan):
    outcome = run_discussion(
        clean_temp_plan,
        get_user_choice=lambda plan: "CONFIRM",
        get_adjustment_text=lambda: "",
        reparse=lambda text, plan: None,
        print_fn=lambda _: None,
    )
    assert isinstance(outcome, Confirmed)


# --- run_discussion: chat-adjust ---------------------------------------------------


def test_chat_adjust_updates_plan_then_confirm(clean_temp_plan, registry):
    adjusted_intent = ValidatedIntent(
        action="clean_temp_files", params={"days": 14}, risk="medium"
    )
    adjusted_plan = generate_plan(adjusted_intent, registry)

    choices = iter(["chat", "confirm"])
    outcome = run_discussion(
        clean_temp_plan,
        get_user_choice=lambda plan: next(choices),
        get_adjustment_text=lambda: "make it two weeks instead",
        reparse=lambda text, plan: adjusted_plan,
        print_fn=lambda _: None,
    )

    assert isinstance(outcome, Confirmed)
    assert outcome.plan.params["days"] == 14


def test_chat_adjust_prints_diff_note(clean_temp_plan, registry, capsys):
    adjusted_intent = ValidatedIntent(
        action="clean_temp_files", params={"days": 14}, risk="medium"
    )
    adjusted_plan = generate_plan(adjusted_intent, registry)

    printed = []
    choices = iter(["chat", "confirm"])
    run_discussion(
        clean_temp_plan,
        get_user_choice=lambda plan: next(choices),
        get_adjustment_text=lambda: "two weeks",
        reparse=lambda text, plan: adjusted_plan,
        print_fn=printed.append,
    )

    assert any("days" in line and "7" in line and "14" in line for line in printed)


def test_chat_adjust_returning_none_keeps_plan_unchanged(clean_temp_plan):
    printed = []
    choices = iter(["chat", "confirm"])
    outcome = run_discussion(
        clean_temp_plan,
        get_user_choice=lambda plan: next(choices),
        get_adjustment_text=lambda: "do something unmappable",
        reparse=lambda text, plan: None,
        print_fn=printed.append,
    )

    assert isinstance(outcome, Confirmed)
    assert outcome.plan == clean_temp_plan  # unchanged
    assert any("couldn't apply" in line.lower() for line in printed)


def test_soft_limit_nudge_appears_after_configured_turns(clean_temp_plan, registry):
    adjusted_intent = ValidatedIntent(action="clean_temp_files", params={"days": 8}, risk="medium")
    adjusted_plan = generate_plan(adjusted_intent, registry)

    printed = []
    # 3 chat turns then confirm, with soft_limit_turns=3 -> nudge on the 3rd chat turn.
    choices = iter(["chat", "chat", "chat", "confirm"])
    run_discussion(
        clean_temp_plan,
        get_user_choice=lambda plan: next(choices),
        get_adjustment_text=lambda: "tweak it",
        reparse=lambda text, plan: adjusted_plan,
        print_fn=printed.append,
        soft_limit_turns=3,
    )

    assert any("turn 3" in line.lower() for line in printed)


def test_no_nudge_before_soft_limit_reached(clean_temp_plan, registry):
    adjusted_intent = ValidatedIntent(action="clean_temp_files", params={"days": 8}, risk="medium")
    adjusted_plan = generate_plan(adjusted_intent, registry)

    printed = []
    choices = iter(["chat", "confirm"])  # only 1 chat turn, soft limit is 5
    run_discussion(
        clean_temp_plan,
        get_user_choice=lambda plan: next(choices),
        get_adjustment_text=lambda: "tweak it",
        reparse=lambda text, plan: adjusted_plan,
        print_fn=printed.append,
    )

    assert not any("turn" in line.lower() and "discussion" in line.lower() for line in printed)


def test_soft_limit_nudge_does_not_block_further_turns(clean_temp_plan, registry):
    """Section 8.3.3: nudge is a gentle reminder, never a hard block."""
    adjusted_intent = ValidatedIntent(action="clean_temp_files", params={"days": 8}, risk="medium")
    adjusted_plan = generate_plan(adjusted_intent, registry)

    choices = iter(["chat", "chat", "chat", "chat", "confirm"])
    outcome = run_discussion(
        clean_temp_plan,
        get_user_choice=lambda plan: next(choices),
        get_adjustment_text=lambda: "tweak it",
        reparse=lambda text, plan: adjusted_plan,
        print_fn=lambda _: None,
        soft_limit_turns=2,
    )

    assert isinstance(outcome, Confirmed)  # loop kept going past the nudge


# --- run_discussion: unrecognized choice -------------------------------------------


def test_unrecognized_choice_reprompts(clean_temp_plan):
    printed = []
    choices = iter(["garbage", "confirm"])
    outcome = run_discussion(
        clean_temp_plan,
        get_user_choice=lambda plan: next(choices),
        get_adjustment_text=lambda: "",
        reparse=lambda text, plan: None,
        print_fn=printed.append,
    )

    assert isinstance(outcome, Confirmed)
    assert any("unrecognized" in line.lower() for line in printed)


# --- edit_step_param ---------------------------------------------------------------


def test_edit_step_param_updates_value_and_rebuilds_steps(clean_temp_plan, registry):
    updated = edit_step_param(clean_temp_plan, "days", 30, registry)

    assert updated.params["days"] == 30
    assert "30" in updated.steps[0]


def test_edit_step_param_raises_for_unknown_param(clean_temp_plan, registry):
    with pytest.raises(KeyError):
        edit_step_param(clean_temp_plan, "not_a_real_param", 5, registry)


def test_edit_step_param_does_not_mutate_original_plan(clean_temp_plan, registry):
    edit_step_param(clean_temp_plan, "days", 30, registry)
    assert clean_temp_plan.params["days"] == 7  # original untouched (Plan is frozen)