"""
Harness Validation Layer (Build Order Step 4).

Validates the SHAPE of one Intent Parser response (well-formed
command/risk/explanation) — a pure validator, it does not call any model
itself. Retry orchestration lives in intent_parser.py.

`risk` here is the model's own self-assessment and is never blindly
trusted downstream: danger_classifier.py's regex tier and its independent
override layer re-check every command regardless of what the model says
(see that module).
"""

from __future__ import annotations

from typing import Any

from pydantic import BaseModel, Field, ValidationError

KNOWN_RISK_LEVELS = ("low", "medium", "high")


class RawIntent(BaseModel):
    """
    Shape the Intent Parser is expected to produce: one real, runnable shell
    command, plus the model's own risk assessment and a short explanation.

    First-pass shape check only — whether `risk` is a recognized level is
    checked separately below, and whether the command itself is dangerous
    is checked downstream by danger_classifier.py.
    """

    command: str = Field(min_length=1)
    risk: str
    explanation: str = ""


class ValidatedIntent(BaseModel):
    """
    A RawIntent that has passed shape-checking. `risk` here is the model's
    own assessment, normalized to one of the known levels — not yet
    cross-checked against danger_classifier.py's independent rules; callers
    (intent_parser.py / main.py) run classify() on `command` afterward and
    take the more severe of the two verdicts.
    """

    command: str
    risk: str
    explanation: str = ""


class ValidationOutcome(BaseModel):
    """
    Result of a single validation attempt. Exactly one of `intent` / `error`
    is set, matching `ok`. Retry/fallback-to-unmapped orchestration is the
    caller's job (intent_parser.py).
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