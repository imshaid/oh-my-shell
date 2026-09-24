"""
Plan Generator (Build Order Step 7).

Turns a ValidatedIntent (validation.py) into a human-readable Plan
(Section 8.3.3): the Intent Parser's model call already produced one real,
runnable command and its explanation, so "the plan" is that command plus a
one-line description built from the explanation.
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
    ValidatedIntent that produced this plan. `steps` is a list of exactly
    one entry (the explanation, or the command itself if none was given) so
    renderers that iterate `plan.steps` (ui/panels.render_plan_panel,
    discussion.render_plan_text) don't need a separate single-string path.
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
            already harness-validated — this function trusts them as-is
            and does not itself run the independent risk override; see
            danger_classifier.py and intent_parser.py for that).
    """
    step_text = intent.explanation.strip() or intent.command
    return Plan(
        command=intent.command,
        risk=intent.risk,
        explanation=intent.explanation,
        steps=[step_text],
    )