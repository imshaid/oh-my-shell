"""
Tests for discussion.py (Build Order Step 7), rewritten for the open-ended
architecture (see discussion.py's own module docstring and
edit_command's docstring). [e] edit now replaces the plan's raw command
TEXT wholesale via edit_command(plan, new_command) instead of editing one
param in a params dict via the removed edit_step_param().

get_user_choice returns a (choice, plan) tuple, not just a choice string
(Section 16 Rule 5 -- see run_discussion's own docstring). For
"confirm"/"cancel"/"chat" the plan half of the tuple is simply whatever
Plan was passed in to get_user_choice.
"""

import pytest

from ohmyshell.discussion import (
    Cancelled,
    Confirmed,
    edit_command,
    render_plan_text,
    run_discussion,
)
from ohmyshell.plan_generator import Plan, generate_plan
from ohmyshell.validation import ValidatedIntent


@pytest.fixture
def clean_temp_plan():
    intent = ValidatedIntent(
        command="find /tmp -mtime +7 -delete",
        risk="medium",
        explanation="Delete files in /tmp older than 7 days.",
    )
    return generate_plan(intent)


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
        get_user_choice=lambda plan: ("confirm", plan),
        get_adjustment_text=lambda: "",
        reparse=lambda text, plan: None,
        print_fn=lambda _: None,
    )

    assert isinstance(outcome, Confirmed)
    assert outcome.plan == clean_temp_plan


def test_cancel_on_first_turn_returns_cancelled(clean_temp_plan):
    outcome = run_discussion(
        clean_temp_plan,
        get_user_choice=lambda plan: ("cancel", plan),
        get_adjustment_text=lambda: "",
        reparse=lambda text, plan: None,
        print_fn=lambda _: None,
    )

    assert isinstance(outcome, Cancelled)


def test_choice_is_case_insensitive(clean_temp_plan):
    outcome = run_discussion(
        clean_temp_plan,
        get_user_choice=lambda plan: ("CONFIRM", plan),
        get_adjustment_text=lambda: "",
        reparse=lambda text, plan: None,
        print_fn=lambda _: None,
    )
    assert isinstance(outcome, Confirmed)


# --- run_discussion: chat-adjust ---------------------------------------------------


def test_chat_adjust_updates_plan_then_confirm(clean_temp_plan):
    adjusted_intent = ValidatedIntent(
        command="find /tmp -mtime +14 -delete",
        risk="medium",
        explanation="Delete files in /tmp older than 14 days.",
    )
    adjusted_plan = generate_plan(adjusted_intent)

    choices = iter(["chat", "confirm"])
    outcome = run_discussion(
        clean_temp_plan,
        get_user_choice=lambda plan: (next(choices), plan),
        get_adjustment_text=lambda: "make it two weeks instead",
        reparse=lambda text, plan: adjusted_plan,
        print_fn=lambda _: None,
    )

    assert isinstance(outcome, Confirmed)
    assert outcome.plan.command == "find /tmp -mtime +14 -delete"


def test_chat_adjust_prints_diff_note(clean_temp_plan):
    adjusted_intent = ValidatedIntent(
        command="find /tmp -mtime +14 -delete",
        risk="medium",
        explanation="Delete files in /tmp older than 14 days.",
    )
    adjusted_plan = generate_plan(adjusted_intent)

    printed = []
    choices = iter(["chat", "confirm"])
    run_discussion(
        clean_temp_plan,
        get_user_choice=lambda plan: (next(choices), plan),
        get_adjustment_text=lambda: "two weeks",
        reparse=lambda text, plan: adjusted_plan,
        print_fn=printed.append,
    )

    assert any(
        "find /tmp -mtime +7 -delete" in line and "find /tmp -mtime +14 -delete" in line
        for line in printed
    )


def test_chat_adjust_returning_none_keeps_plan_unchanged(clean_temp_plan):
    printed = []
    choices = iter(["chat", "confirm"])
    outcome = run_discussion(
        clean_temp_plan,
        get_user_choice=lambda plan: (next(choices), plan),
        get_adjustment_text=lambda: "do something unmappable",
        reparse=lambda text, plan: None,
        print_fn=printed.append,
    )

    assert isinstance(outcome, Confirmed)
    assert outcome.plan == clean_temp_plan  # unchanged
    assert any("couldn't apply" in line.lower() for line in printed)


def test_soft_limit_nudge_appears_after_configured_turns(clean_temp_plan):
    adjusted_intent = ValidatedIntent(command="find /tmp -mtime +8 -delete", risk="medium", explanation="")
    adjusted_plan = generate_plan(adjusted_intent)

    printed = []
    choices = iter(["chat", "chat", "chat", "confirm"])
    run_discussion(
        clean_temp_plan,
        get_user_choice=lambda plan: (next(choices), plan),
        get_adjustment_text=lambda: "tweak it",
        reparse=lambda text, plan: adjusted_plan,
        print_fn=printed.append,
        soft_limit_turns=3,
    )

    assert any("turn 3" in line.lower() for line in printed)


def test_soft_limit_nudge_appears_only_once_not_every_turn_after(clean_temp_plan):
    adjusted_intent = ValidatedIntent(command="find /tmp -mtime +8 -delete", risk="medium", explanation="")
    adjusted_plan = generate_plan(adjusted_intent)

    printed = []
    choices = iter(["chat", "chat", "chat", "chat", "chat", "confirm"])
    run_discussion(
        clean_temp_plan,
        get_user_choice=lambda plan: (next(choices), plan),
        get_adjustment_text=lambda: "tweak it",
        reparse=lambda text, plan: adjusted_plan,
        print_fn=printed.append,
        soft_limit_turns=2,
    )

    nudge_lines = [line for line in printed if "raw command mode" in line.lower()]
    assert len(nudge_lines) == 1
    assert "turn 2" in nudge_lines[0].lower()


def test_no_nudge_before_soft_limit_reached(clean_temp_plan):
    adjusted_intent = ValidatedIntent(command="find /tmp -mtime +8 -delete", risk="medium", explanation="")
    adjusted_plan = generate_plan(adjusted_intent)

    printed = []
    choices = iter(["chat", "confirm"])  # only 1 chat turn, soft limit is 5
    run_discussion(
        clean_temp_plan,
        get_user_choice=lambda plan: (next(choices), plan),
        get_adjustment_text=lambda: "tweak it",
        reparse=lambda text, plan: adjusted_plan,
        print_fn=printed.append,
    )

    assert not any("turn" in line.lower() and "discussion" in line.lower() for line in printed)


def test_soft_limit_nudge_does_not_block_further_turns(clean_temp_plan):
    adjusted_intent = ValidatedIntent(command="find /tmp -mtime +8 -delete", risk="medium", explanation="")
    adjusted_plan = generate_plan(adjusted_intent)

    choices = iter(["chat", "chat", "chat", "chat", "confirm"])
    outcome = run_discussion(
        clean_temp_plan,
        get_user_choice=lambda plan: (next(choices), plan),
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
        get_user_choice=lambda plan: (next(choices), plan),
        get_adjustment_text=lambda: "",
        reparse=lambda text, plan: None,
        print_fn=printed.append,
    )

    assert isinstance(outcome, Confirmed)
    assert any("unrecognized" in line.lower() for line in printed)


# --- run_discussion: edit actually reaches the final plan (bug fix) ---------------


def test_edit_choice_updated_plan_reaches_confirmed_outcome(clean_temp_plan):
    edited_plan = edit_command(clean_temp_plan, "find /tmp -mtime +30 -delete")
    choices = iter(["edit", "confirm"])

    def _get_user_choice(plan):
        choice = next(choices)
        if choice == "edit":
            return choice, edited_plan
        return choice, plan

    outcome = run_discussion(
        clean_temp_plan,
        get_user_choice=_get_user_choice,
        get_adjustment_text=lambda: "",
        reparse=lambda text, plan: None,
        print_fn=lambda _: None,
    )

    assert isinstance(outcome, Confirmed)
    assert outcome.plan.command == "find /tmp -mtime +30 -delete"


def test_edit_choice_replan_is_passed_to_next_get_user_choice_call(clean_temp_plan):
    edited_plan = edit_command(clean_temp_plan, "find /tmp -mtime +21 -delete")
    choices = iter(["edit", "confirm"])
    received_plans = []

    def _get_user_choice(plan):
        received_plans.append(plan)
        choice = next(choices)
        if choice == "edit":
            return choice, edited_plan
        return choice, plan

    run_discussion(
        clean_temp_plan,
        get_user_choice=_get_user_choice,
        get_adjustment_text=lambda: "",
        reparse=lambda text, plan: None,
        print_fn=lambda _: None,
    )

    assert received_plans[0] is clean_temp_plan  # first turn: original plan
    assert received_plans[1] is edited_plan  # second turn: the edited plan


# --- edit_command ---------------------------------------------------------------


def test_edit_command_replaces_command_and_rebuilds_steps(clean_temp_plan):
    updated = edit_command(clean_temp_plan, "find /tmp -mtime +30 -delete")

    assert updated.command == "find /tmp -mtime +30 -delete"
    assert updated.steps == [updated.explanation.strip() or updated.command]


def test_edit_command_raises_for_empty_command(clean_temp_plan):
    with pytest.raises(ValueError):
        edit_command(clean_temp_plan, "")


def test_edit_command_raises_for_blank_whitespace_command(clean_temp_plan):
    with pytest.raises(ValueError):
        edit_command(clean_temp_plan, "   ")


def test_edit_command_does_not_mutate_original_plan(clean_temp_plan):
    edit_command(clean_temp_plan, "find /tmp -mtime +30 -delete")
    assert clean_temp_plan.command == "find /tmp -mtime +7 -delete"  # original untouched (Plan is frozen)


def test_edit_command_preserves_risk_and_explanation(clean_temp_plan):
    updated = edit_command(clean_temp_plan, "find /tmp -mtime +30 -delete")
    assert updated.risk == clean_temp_plan.risk
    assert updated.explanation == clean_temp_plan.explanation


# --- Removed: edit_step_param tests --------------------------------------------
#
# edit_step_param(plan, param_name, new_value, registry) was removed (see
# discussion.py's own module docstring) -- there is no per-action params
# dict to edit any more. test_edit_step_param_raises_for_unknown_param had
# no equivalent (there are no longer named "params" to look up at all), so
# it was translated into edit_command's blank/empty-command ValueError
# tests above instead, which is the new failure mode for a bad [e] edit.