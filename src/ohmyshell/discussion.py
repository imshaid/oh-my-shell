"""
Confirmation + Discussion Loop (Build Order Step 7).

Section 8.3.3: shows the plan, offers [Enter] confirm / [e] edit / [c]
chat-adjust / [Esc] cancel, nudges (not blocks) after a soft limit of
discuss turns, and reports an inline diff-note describing what changed
after a chat-adjust. Plain-text I/O in this step — the boxed panel/live
token-count rendering is ui/panels.py's job.

- "Chat/adjust" ([c]) re-runs the Intent Parser with the user's free-text
  adjustment, then regenerates the plan. This module doesn't call the model
  backend directly — it takes a `reparse` callback (wrapping
  intent_parser.parse_intent + plan_generator.generate_plan) so it stays
  testable without a real model.
- "Direct edit" ([e]) lets the user edit the plan's raw command text
  directly — no model call, instant.
- The diff-note after a chat-adjust is a mechanical comparison of the old
  and new command strings, not a semantic explanation of *why* — that would
  need the model to explain its own edit, which isn't data this module has.
- The soft-limit nudge (Section 8.3.3: 5 turns) counts chat-adjust turns
  only, not edit turns.
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
    [e] direct-edit: replace the plan's raw command text outright and
    rebuild the plan around it — no model call, instant.

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
            "chat", "cancel" (case-insensitive; the raw keypress-to-choice
            mapping is the real CLI's job, so this loop stays
            UI-framework-agnostic). `plan` is the Plan to continue the loop
            with — for "confirm"/"cancel"/"chat" this is just the same
            `current_plan` it was called with, but for "edit" it is the
            caller's updated Plan (after running its own edit sub-flow),
            which is how an edit reaches the next iteration and the final
            Confirmed(plan) result.
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
        # Plan-rendering is get_user_choice's job (the REPL layer shows the
        # boxed panel before reading the keypress) — this loop only needs
        # the decision, not to also render the plan itself. render_plan_text/
        # print_fn are still used for every other message this loop prints
        # (diff-notes, soft-limit nudge, "couldn't apply", "unrecognized
        # choice").
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
            # `==` (not `>=`) fires the nudge exactly once, on the turn the
            # threshold is first reached — Section 8.3.3's mockup shows the
            # reminder appearing once, not on every subsequent turn.
            if chat_turns == soft_limit_turns:
                print_fn(SOFT_LIMIT_NUDGE.format(turn=chat_turns))
            continue

        if choice == "edit":
            # get_user_choice already ran its own edit sub-flow and
            # returned the updated plan above; nothing left to do here but
            # loop and re-show it.
            continue

        print_fn(f"  Unrecognized choice {choice!r}. Use confirm/edit/chat/cancel.")