"""
Confirmation + Discussion Loop (Build Order Step 7).

Section 8.3.3: shows the plan, offers [Enter] confirm / [e] edit / [c]
chat-adjust / [Esc] cancel, nudges (not blocks) after a soft limit of
discuss turns, and reports an inline diff-note describing what changed
after a chat-adjust. Plain-text I/O in this step — the boxed panel/live
token-count rendering is Step 11 (ui/panels.py).

Design notes (implementation decisions, logged here as routine code
structure rather than blueprint open questions):

- "Chat/adjust" ([c]) means re-running the Intent Parser with the user's
  free-text adjustment appended as extra context, then regenerating the
  plan from whatever validated intent comes back. This module doesn't
  call Ollama directly — it takes a `reparse` callback (expected to wrap
  intent_parser.parse_intent + plan_generator.generate_plan) so this
  module stays testable without a real model and doesn't need to import
  intent_parser itself.
- "Direct edit" ([e]) only ever changes one param's value in place and
  rebuilds the plan's step text from the registry's templates — no model
  call, matching the blueprint's "AI বাইপাস করে, তাই instant" description.
- The diff-note after a chat-adjust is a plain "list of params that
  changed" summary (old -> new), not a semantic explanation of *why* —
  producing a truly semantic diff-note ("Downloads excluded per your
  request") would need the model to explain its own edit, which isn't
  data this module has; a mechanical params diff is the honest version of
  that feature at this step.
- The soft-limit nudge (Section 8.3.3: 5 turns) counts chat-adjust turns
  only, not edit turns — edits are instant/deterministic and don't carry
  the same "going in circles with the model" risk the nudge is warning
  about.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Callable, Protocol

from ohmyshell.plan_generator import Plan

SOFT_LIMIT_TURNS_DEFAULT = 5

SOFT_LIMIT_NUDGE = (
    "  (This is turn {turn} of this discussion. If you know exactly what you "
    "want, raw command mode might be faster than continuing to adjust the plan.)"
)


class DiscussionOutcome:
    """Sentinel-style result of one full discussion session."""


@dataclass(frozen=True)
class Confirmed(DiscussionOutcome):
    plan: Plan


@dataclass(frozen=True)
class Cancelled(DiscussionOutcome):
    pass


class Reparser(Protocol):
    """
    Callback signature for [c] chat-adjust: takes the user's adjustment text
    and the current plan, returns a freshly generated Plan (or None if the
    adjustment couldn't be mapped to anything — same "unmapped" idea as
    intent_parser, surfaced back up to the discussion loop rather than
    crashing it).
    """

    def __call__(self, adjustment_text: str, current_plan: Plan) -> Plan | None: ...


def _diff_note(old_params: dict, new_params: dict) -> str:
    """
    Mechanical, plain-text description of what changed between two params
    dicts — added/removed/changed keys. See the module docstring for why
    this isn't a semantic explanation.
    """
    changes: list[str] = []
    for key in sorted(set(old_params) | set(new_params)):
        old_value = old_params.get(key)
        new_value = new_params.get(key)
        if key not in old_params:
            changes.append(f"{key} added ({new_value!r})")
        elif key not in new_params:
            changes.append(f"{key} removed")
        elif old_value != new_value:
            changes.append(f"{key}: {old_value!r} -> {new_value!r}")
    if not changes:
        return "No changes."
    return "; ".join(changes)


def render_plan_text(plan: Plan) -> str:
    """Plain-text plan rendering (Step 11 replaces this with a rich panel)."""
    lines = ["Plan:"]
    for i, step in enumerate(plan.steps, start=1):
        lines.append(f"  {i}. {step}")
    lines.append(f"Risk: {plan.risk.capitalize()}")
    return "\n".join(lines)


def edit_step_param(plan: Plan, param_name: str, new_value, registry) -> Plan:
    """
    [e] direct-edit: change one param's value and rebuild the plan's step
    text from the registry's templates — no model call (Section 8.3.3:
    "AI বাইপাস করে, তাই instant, কোনো model-call লাগে না").

    Raises:
        KeyError: if `param_name` isn't a key in the plan's current params
            (editing a param that was never set isn't what "direct edit an
            existing value" means here — callers should offer only the
            plan's existing param names for editing).
    """
    if param_name not in plan.params:
        raise KeyError(param_name)

    from ohmyshell.plan_generator import generate_plan
    from ohmyshell.validation import ValidatedIntent

    new_params = dict(plan.params)
    new_params[param_name] = new_value

    updated_intent = ValidatedIntent(action=plan.action, params=new_params, risk=plan.risk)
    return generate_plan(updated_intent, registry)


def run_discussion(
    initial_plan: Plan,
    *,
    get_user_choice: Callable[[Plan], tuple[str, Plan]],
    get_adjustment_text: Callable[[], str],
    reparse: Reparser,
    print_fn: Callable[[str], None] = print,
    soft_limit_turns: int = SOFT_LIMIT_TURNS_DEFAULT,
) -> DiscussionOutcome:
    """
    Run the confirm/edit/chat-adjust/cancel loop until the user confirms or
    cancels.

    Args:
        initial_plan: the first Plan to show.
        get_user_choice: called with the current Plan, must return a
            (choice, plan) tuple. `choice` is one of "confirm", "edit",
            "chat", "cancel" (case-insensitive; this function does the raw
            keypress-to-choice mapping in the real CLI — Step 11 — so this
            loop stays UI-framework-agnostic). `plan` is the Plan to
            continue the loop with -- for "confirm"/"cancel"/"chat" this is
            just the same `current_plan` it was called with, but for "edit"
            it is the caller's updated Plan (after running its own edit
            sub-flow, e.g. via edit_step_param) -- this is how an edit
            actually reaches the next iteration of this loop and the final
            Confirmed(plan) result.

            Bug fix (found via manual end-to-end testing, post-Build-Order):
            an earlier version of this contract had get_user_choice return
            only the choice string, with a comment saying callers were
            "expected to... pass the resulting Plan back in via
            get_user_choice's next call" -- but nothing in this loop ever
            read a plan back out of get_user_choice, so a caller's edited
            plan never actually reached this loop's own `plan` variable,
            and [e] Edit silently kept confirming/executing the OLD,
            unedited plan. Returning the plan alongside the choice closes
            that gap directly, without a caller needing a side channel or
            a mutable Plan (Plan is and stays a frozen dataclass).
        get_adjustment_text: called with no args when the user picks
            "chat", must return their free-text adjustment.
        reparse: callback that turns adjustment text + the current plan
            into a new Plan (or None if it couldn't).
        print_fn: injected for testability; defaults to builtin print.
        soft_limit_turns: chat-adjust turns before the nudge (Section 8.3.3
            default: 5, sourced from config's discussion.soft_limit_turns
            by the caller — this function just takes the number).

    Returns:
        Confirmed(plan) or Cancelled().
    """
    plan = initial_plan
    chat_turns = 0

    while True:
        # Bug fix (found via manual end-to-end testing, post-Build-Order,
        # Step 11 UI-polish pass): this loop used to call
        # `print_fn(render_plan_text(plan))` here on every turn, rendering
        # the plan in plain text -- then `get_user_choice(plan)` was called
        # right after, and every real caller (main.py's
        # _repl_get_user_choice) ALSO renders the plan itself (now as a
        # boxed rich.Panel, via ui/panels.render_plan_panel), specifically
        # so it can show the panel immediately before reading the user's
        # keypress. The result was the plan appearing twice per turn: once
        # plain, once boxed. Plan-rendering is get_user_choice's job (its
        # own docstring already says the raw-keypress-to-choice mapping,
        # and by extension what's shown right before it, belongs to the
        # REPL layer) -- this loop only needs the *decision*, not to also
        # render the thing the decision is about. render_plan_text/
        # print_fn are still used for every other message this loop prints
        # (diff-notes, soft-limit nudge, "couldn't apply", "unrecognized
        # choice") -- only the per-turn plan echo was removed.
        choice, plan = get_user_choice(plan)
        choice = choice.strip().lower()

        if choice == "confirm":
            return Confirmed(plan=plan)

        if choice == "cancel":
            return Cancelled()

        if choice == "chat":
            chat_turns += 1
            adjustment_text = get_adjustment_text()
            new_plan = reparse(adjustment_text, plan)
            if new_plan is None:
                print_fn("  Couldn't apply that adjustment — plan unchanged.")
                continue
            print_fn(f"  {_diff_note(plan.params, new_plan.params)}")
            plan = new_plan
            if chat_turns >= soft_limit_turns:
                print_fn(SOFT_LIMIT_NUDGE.format(turn=chat_turns))
            continue

        if choice == "edit":
            # get_user_choice already ran its own edit sub-flow (which
            # param, what new value -- a Step 11 UI concern, Section 8.3.3's
            # "Edit which step?" prompt) and returned the updated plan
            # above; nothing left to do here but loop and re-show it.
            continue

        print_fn(f"  Unrecognized choice {choice!r}. Use confirm/edit/chat/cancel.")