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
- "Direct edit" ([e]) — rewritten for the open-ended architecture (see
  validation.py's module docstring): there is no per-action params dict to
  edit any more, so [e] now lets the user directly edit the plan's raw
  command TEXT itself (replacing plan.command wholesale) — still no model
  call, still instant, matching the blueprint's "AI বাইপাস করে, তাই instant"
  description, just operating on the command string instead of a params
  dict.
- The diff-note after a chat-adjust now compares the old and new command
  strings directly (not a params dict) — still a mechanical diff, not a
  semantic explanation of *why*, for the same reason as before: producing
  a truly semantic diff-note would need the model to explain its own edit,
  which isn't data this module has.
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


def _diff_note(old_command: str, new_command: str) -> str:
    """
    Mechanical, plain-text description of what changed between two command
    strings. See the module docstring for why this isn't a semantic
    explanation.
    """
    if old_command == new_command:
        return "No changes."
    return f"command: {old_command!r} -> {new_command!r}"


def render_plan_text(plan: Plan) -> str:
    """Plain-text plan rendering (Step 11 replaces this with a rich panel)."""
    lines = ["Plan:"]
    for i, step in enumerate(plan.steps, start=1):
        lines.append(f"  {i}. {step}")
    lines.append(f"Risk: {plan.risk.capitalize()}")
    return "\n".join(lines)


def edit_command(plan: Plan, new_command: str) -> Plan:
    """
    [e] direct-edit, rewritten for the open-ended architecture: replace the
    plan's raw command text outright and rebuild the plan around it — no
    model call (Section 8.3.3: "AI বাইপাস করে, তাই instant, কোনো model-call
    লাগে না"), same as before, just operating on a command string instead
    of a params dict (there is no registry/params_schema to rebuild step
    text from any more — see plan_generator.py).

    Raises:
        ValueError: if `new_command` is empty/blank — an edit must still
            leave the plan with something runnable.
    """
    if not new_command.strip():
        raise ValueError("command cannot be empty")

    from ohmyshell.plan_generator import generate_plan
    from ohmyshell.validation import ValidatedIntent

    updated_intent = ValidatedIntent(
        command=new_command.strip(), risk=plan.risk, explanation=plan.explanation
    )
    return generate_plan(updated_intent)


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
            print_fn(f"  {_diff_note(plan.command, new_plan.command)}")
            plan = new_plan
            # Bug fix (found via manual end-to-end testing, post-Build-Order):
            # this used to compare with `>=`, so the nudge re-printed on
            # EVERY chat turn once the threshold was crossed (turn 5, 6, 7,
            # ...), not just once. Section 8.3.3 says "৫ discuss-turn-এর পর
            # একটা gentle reminder" -- "একটা" (a/one), singular -- matching
            # the blueprint's own mockup, which shows the reminder appearing
            # exactly once. `==` fires the nudge only on the exact turn the
            # threshold is first reached.
            if chat_turns == soft_limit_turns:
                print_fn(SOFT_LIMIT_NUDGE.format(turn=chat_turns))
            continue

        if choice == "edit":
            # get_user_choice already ran its own edit sub-flow (which
            # param, what new value -- a Step 11 UI concern, Section 8.3.3's
            # "Edit which step?" prompt) and returned the updated plan
            # above; nothing left to do here but loop and re-show it.
            continue

        print_fn(f"  Unrecognized choice {choice!r}. Use confirm/edit/chat/cancel.")