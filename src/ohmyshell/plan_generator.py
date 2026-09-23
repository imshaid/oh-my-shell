"""
Plan Generator (Build Order Step 7; rewritten for the open-ended
architecture — see validation.py's module docstring for the full
rationale).

Turns a ValidatedIntent (validation.py) into a human-readable Plan
(Section 8.3.3). Under the open-ended architecture there is no registry of
plan_steps templates to fill in — the Intent Parser's own model call
already produced the one real, runnable command and its explanation, so
"the plan" is that single command plus a one-line human-readable
description built directly from the model's own explanation. This module
no longer imports registry.py at all.
"""

from __future__ import annotations

from dataclasses import dataclass, field

from ohmyshell.validation import ValidatedIntent


@dataclass(frozen=True)
class Plan:
    """
    A ready-to-run plan for the Confirmation + Discussion Loop
    (discussion.py) to show the user and for the Executor (executor.py) to
    eventually run.

    `command` is the raw, directly-runnable shell command the Intent Parser
    produced. `risk`/`explanation` are carried straight through from the
    ValidatedIntent that produced this plan. `steps` stays a list (rather
    than a bare string) so existing renderers (ui/panels.render_plan_panel,
    discussion.render_plan_text) that iterate `plan.steps` keep working
    unchanged — for the open-ended architecture it always holds exactly one
    entry: the explanation line (or, if the model gave none, the command
    itself).
    """

    command: str
    risk: str
    explanation: str
    steps: list[str]
    estimate: str | None = field(default=None)


def generate_plan(intent: ValidatedIntent) -> Plan:
    """
    Build a Plan from a validated intent.

    Args:
        intent: the ValidatedIntent to plan for (command/risk/explanation
            already harness-validated — this function trusts them as-is;
            it does not itself re-run danger_classifier.py — see that
            module and intent_parser.py for where the independent risk
            override happens).
    """
    step_text = intent.explanation.strip() or intent.command
    return Plan(
        command=intent.command,
        risk=intent.risk,
        explanation=intent.explanation,
        steps=[step_text],
    )