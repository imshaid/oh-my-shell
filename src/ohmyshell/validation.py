"""
Harness Validation Layer (Build Order Step 4; rewritten for the open-ended
architecture — see Oh-My-Shell-Blueprint-FINAL.md Section 7.4's updated
note).

--- Architecture change (explicitly authorized by the project owner) -------
The original Section 7.4 design mapped a natural-language request to one of
a fixed, small set of registry actions, with `risk` always a static,
hardcoded lookup from capabilities.json — never taken from the model.

That was deliberately chosen because the blueprint's own early model
testing found risk-assessment consistency unreliable across models
("Kill-process risk-consistency সব মডেলেই কমবেশি অস্থির"). This session's own
empirical testing (qwen3.5:4b calling a real, catastrophic
`rm -rf /home/*/.cache/` "low"; both tested Gemini models under-risking
port-opening and passwordless-user-creation) reproduced that exact failure
pattern independently, on different models, years apart — so the concern
was real and remains real.

The project owner explicitly chose to move to open-ended AI command
generation anyway (the fixed 4-action registry was too narrow for a
natural-language shell), and explicitly authorized updating this locked
decision. To keep the original safety rationale intact under the new
architecture, `risk` is no longer a registry lookup (there is no registry
mapping a free-form command to a risk level) — instead:

  1. The model is asked for its own honest risk assessment, same as before.
  2. danger_classifier.py's regex tier (existing, unchanged) still runs on
     the final command, independent of what the model said.
  3. A NEW independent override layer in danger_classifier.py (see that
     module) specifically re-checks for the failure patterns actually
     observed in testing (port-opening, passwordless user creation, and
     other classically under-risked operations) and force-escalates risk
     regardless of the model's own answer.

So "risk is never blindly trusted from the model" is preserved — it's just
enforced by a different, complementary layer (danger_classifier.py) instead
of a static per-action table, because there is no longer a fixed action set
to hold that table.

This module is still a *pure* validator: it checks one Intent Parser
response's SHAPE (well-formed command/risk/explanation) and does not call
any model itself. Retry orchestration still lives in intent_parser.py.
"""

from __future__ import annotations

from typing import Any

from pydantic import BaseModel, Field, ValidationError

KNOWN_RISK_LEVELS = ("low", "medium", "high")


class RawIntent(BaseModel):
    """
    Shape the Intent Parser is expected to produce: one real, runnable shell
    command, plus the model's own risk assessment and a short explanation.

    This is the *first* Pydantic pass: "is this even a well-formed response
    at all" — independent of whether `risk` is one of the recognized levels
    (checked separately below) or whether the command itself is dangerous
    (checked downstream by danger_classifier.py, not here).
    """

    command: str = Field(min_length=1)
    risk: str
    explanation: str = ""


class ValidatedIntent(BaseModel):
    """
    A RawIntent that has passed shape-checking. `risk` here is the model's
    own assessment, normalized to one of the known levels — NOT yet
    cross-checked against danger_classifier.py's independent rules; callers
    (intent_parser.py / main.py) are expected to run classify() on `command`
    afterward and take the more severe of the two verdicts, exactly as a raw
    shell command already does today.
    """

    command: str
    risk: str
    explanation: str = ""


class ValidationOutcome(BaseModel):
    """
    Result of a single validation attempt. Exactly one of `intent` / `error`
    is set, matching `ok`. Retry/fallback-to-unmapped orchestration is the
    caller's job (intent_parser.py), same as before.
    """

    ok: bool
    intent: ValidatedIntent | None = None
    error: str | None = None


def validate_intent(raw: Any) -> ValidationOutcome:
    """
    Validate a single Intent Parser response.

    Args:
        raw: the model's response, either already a dict (parsed JSON) or a
            RawIntent.

    Returns:
        ValidationOutcome with ok=True and a ValidatedIntent, or ok=False
        and a human-readable `error` describing what failed.
    """
    if isinstance(raw, RawIntent):
        parsed = raw
    else:
        try:
            parsed = RawIntent.model_validate(raw)
        except ValidationError as exc:
            return ValidationOutcome(ok=False, error=f"Malformed intent shape: {exc}")

    if not parsed.command.strip():
        return ValidationOutcome(ok=False, error="command is empty or blank.")

    risk = parsed.risk.strip().lower()
    if risk not in KNOWN_RISK_LEVELS:
        return ValidationOutcome(
            ok=False,
            error=f"risk {parsed.risk!r} is not one of {KNOWN_RISK_LEVELS}.",
        )

    return ValidationOutcome(
        ok=True,
        intent=ValidatedIntent(command=parsed.command.strip(), risk=risk, explanation=parsed.explanation),
    )