"""
Plan Generator (Build Order Step 7).

Turns a ValidatedIntent (validation.py, Step 4) into a human-readable,
step-by-step Plan (Section 8.3.3) — plain text in this step; the `rich`-based
boxed-panel rendering shown in the blueprint's example is Step 11's job
(ui/panels.py). This module only builds the Plan data; it doesn't print it.

Step-text source (implementation decision, confirmed before writing this
module): capabilities.json now carries an optional `plan_steps` field — a
list of step-text templates with `{param}` placeholders, filled in from the
validated intent's params (Build Order Step 3's registry.py and the
capabilities.json schema were both updated to add this). A capability with
no `plan_steps` registered falls back to one generic step built from its
`description` and raw params, so this module never fails outright on a
capability someone forgot to add step templates for — it just produces a
less detailed plan for that one action.

Scope note: Section 8.3.3's example plan also shows an estimate line
("Risk: Medium · Est. 340 files · ~1.2 GB"). Producing a real estimate
would mean either a dry-run or scanning the filesystem before executing —
that's Executor territory (Step 10), not something this module can know
just from an intent. `Plan.estimate` is included as an optional field for
the Executor (or a later step) to fill in later; this module always leaves
it as None.
"""

from __future__ import annotations

from dataclasses import dataclass, field

from ohmyshell.validation import ValidatedIntent


@dataclass(frozen=True)
class Plan:
    """
    A step-by-step plan ready for the Confirmation + Discussion Loop
    (discussion.py) to show the user and for the Executor (Step 10) to
    eventually run.

    `action`/`params`/`risk` are carried straight through from the
    ValidatedIntent that produced this plan — nothing here re-derives or
    second-guesses risk; it's still the registry's static value.
    """

    action: str
    params: dict
    risk: str
    steps: list[str]
    estimate: str | None = field(default=None)


class _LeavePlaceholder(dict):
    """
    Used with str.format_map so any {param} the intent doesn't have is left
    visible in the output (e.g. "{paths}") instead of raising — and, unlike
    a bare try/except around str.format(**params), this fills in every
    placeholder that *does* have a value even when a sibling placeholder in
    the same template is missing. A plan the user can still read and edit
    beats either a crash or an entirely-unfilled template mid-preview.
    """

    def __missing__(self, key):
        return "{" + key + "}"


def _format_step(template: str, params: dict) -> str:
    """Fill a plan-step template's {param} placeholders from params."""
    return template.format_map(_LeavePlaceholder(params))


def _generic_single_step(action: str, description: str, params: dict) -> list[str]:
    """Fallback for a capability with no registered plan_steps."""
    if not params:
        return [description]
    param_summary = ", ".join(f"{key}={value}" for key, value in params.items())
    return [f"{description} ({param_summary})"]


def generate_plan(intent: ValidatedIntent, registry) -> Plan:
    """
    Build a Plan from a validated intent.

    Args:
        intent: the ValidatedIntent to plan for (action/params/risk already
            harness-validated — this function trusts them as-is).
        registry: the loaded Registry, used to look up plan_steps templates
            (or description, for the fallback case) for `intent.action`.
    """
    capability = registry.get(intent.action)
    templates = registry.plan_steps_for(intent.action)

    if templates is not None:
        steps = [_format_step(template, intent.params) for template in templates]
    else:
        steps = _generic_single_step(intent.action, capability["description"], intent.params)

    return Plan(
        action=intent.action,
        params=intent.params,
        risk=intent.risk,
        steps=steps,
    )