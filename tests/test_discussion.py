"""
Tests for discussion.py (Build Order Step 7).

get_user_choice / get_adjustment_text / reparse are all fakes here — this
module is deliberately UI-framework-agnostic (see its docstring), so tests
drive it exactly the way a real terminal loop (Step 11/main.py) would.

get_user_choice returns a (choice, plan) tuple, not just a choice string
(Section 16 Rule 5 -- this contract was fixed post-Build-Order after manual
end-to-end testing found that [e] Edit silently did nothing: the original
contract had no way for a caller's edited Plan to reach this loop's own
`plan` variable at all. See run_discussion's own docstring for the full
story). For "confirm"/"cancel"/"chat" the plan half of the tuple is simply
whatever Plan was passed in to get_user_choice.
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


def test_chat_adjust_updates_plan_then_confirm(clean_temp_plan, registry):
    adjusted_intent = ValidatedIntent(
        action="clean_temp_files", params={"days": 14}, risk="medium"
    )
    adjusted_plan = generate_plan(adjusted_intent, registry)

    choices = iter(["chat", "confirm"])
    outcome = run_discussion(
        clean_temp_plan,
        get_user_choice=lambda plan: (next(choices), plan),
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
        get_user_choice=lambda plan: (next(choices), plan),
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
        get_user_choice=lambda plan: (next(choices), plan),
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
        get_user_choice=lambda plan: (next(choices), plan),
        get_adjustment_text=lambda: "tweak it",
        reparse=lambda text, plan: adjusted_plan,
        print_fn=printed.append,
        soft_limit_turns=3,
    )

    assert any("turn 3" in line.lower() for line in printed)


def test_soft_limit_nudge_appears_only_once_not_every_turn_after(clean_temp_plan, registry):
    """
    Regression test (found via manual end-to-end testing): the nudge
    condition used to be `chat_turns >= soft_limit_turns`, which re-printed
    the reminder on EVERY chat turn once the threshold was crossed, not
    just once. Section 8.3.3 says "একটা gentle reminder" (a/one reminder,
    singular) after the soft limit, matching the blueprint's own mockup
    where it appears exactly once -- not once per turn thereafter.
    """
    adjusted_intent = ValidatedIntent(action="clean_temp_files", params={"days": 8}, risk="medium")
    adjusted_plan = generate_plan(adjusted_intent, registry)

    printed = []
    # 5 chat turns then confirm, with soft_limit_turns=2 -> nudge should
    # fire exactly once, on turn 2, and never again on turns 3-5.
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


def test_no_nudge_before_soft_limit_reached(clean_temp_plan, registry):
    adjusted_intent = ValidatedIntent(action="clean_temp_files", params={"days": 8}, risk="medium")
    adjusted_plan = generate_plan(adjusted_intent, registry)

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


def test_soft_limit_nudge_does_not_block_further_turns(clean_temp_plan, registry):
    """Section 8.3.3: nudge is a gentle reminder, never a hard block."""
    adjusted_intent = ValidatedIntent(action="clean_temp_files", params={"days": 8}, risk="medium")
    adjusted_plan = generate_plan(adjusted_intent, registry)

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


def test_edit_choice_updated_plan_reaches_confirmed_outcome(clean_temp_plan, registry):
    """
    Bug found via manual end-to-end testing (post-Build-Order): [e] Edit in
    a real session updated the param but the CONFIRMED plan still showed
    the old value -- get_user_choice's edited Plan never reached this
    loop's own state. This is the regression test: a get_user_choice fake
    that performs an "edit" (via edit_step_param, exactly like main.py's
    real _repl_edit_flow does) and returns the updated plan on the "edit"
    turn must have that update show up in the final Confirmed(plan).
    """
    edited_plan = edit_step_param(clean_temp_plan, "days", 14, registry)
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
    assert outcome.plan.params["days"] == 14


def test_edit_choice_replan_is_passed_to_next_get_user_choice_call(clean_temp_plan, registry):
    """
    After an edit, the loop must call get_user_choice again with the
    UPDATED plan (not the stale one) on its next turn.

    Note: this used to assert against `print_fn` output, back when
    run_discussion re-rendered the plan itself every turn via
    `print_fn(render_plan_text(plan))`. That per-turn render was removed
    (see run_discussion's own in-loop comment) because every real caller
    (main.py's _repl_get_user_choice) already renders the plan itself
    right before reading the user's choice -- the old code showed the
    plan twice, once plain (from here) and once boxed (from the caller).
    Plan-rendering is now entirely get_user_choice's responsibility, so
    this test asserts the actual contract that matters: get_user_choice
    receives the edited plan on its next call, which is what lets a real
    caller re-render the CORRECT (updated) plan.
    """
    edited_plan = edit_step_param(clean_temp_plan, "days", 21, registry)
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