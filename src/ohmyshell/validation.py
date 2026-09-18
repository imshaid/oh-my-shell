"""
Harness Validation Layer (Build Order Step 4).

Section 4.2: "Model output-কে Pydantic schema দিয়ে double-check করে; ব্যর্থ হলে
এক-বার retry, তারপর `unmapped`-এ fallback." Section 7.4/502: risk is always a
static registry lookup, never taken from model output, even if the model
happens to include a risk-looking field.

Scope note (implementation decision, not a blueprint open question — logged
here rather than in Section 15 since it's a routine code-structure call, not
something needing external/team confirmation): this module is a *pure*
validator. It checks one Intent Parser response against the registry and
reports pass/fail — it does not itself call Ollama again. "One retry, then
unmapped" is an orchestration behavior that belongs to whichever module
calls the LLM (intent_parser.py, Build Order Step 5): that module is
expected to call `validate_intent()` here, and on failure, re-prompt the
model once and call `validate_intent()` again before giving up and treating
the request as unmapped. Keeping retry out of this module keeps it a small,
dependency-free, easily-unit-tested pass/fail check.

Flow this module sits in (Section 4.3):
    Intent Parser (schema-constrained LLM output)
        -> Harness Validation (this module)
        -> Capability Registry lookup (risk/params from static registry)
        -> Plan Generator
"""

from __future__ import annotations

from typing import Any

import jsonschema
from pydantic import BaseModel, Field, ValidationError

from ohmyshell.registry import Registry


class RawIntent(BaseModel):
    """
    Shape the Intent Parser (Step 5) is expected to produce, before any
    registry-aware checking happens. This is the *first* Pydantic pass:
    "is this even a well-formed intent object at all" — independent of
    whether `action` names a real capability or `params` matches that
    capability's own params_schema (that's checked separately below,
    against the registry, since params_schema differs per action and
    can't be expressed as one static Pydantic model).
    """

    action: str = Field(min_length=1)
    params: dict[str, Any] = Field(default_factory=dict)
    # Optional free-text the model may return alongside its structured pick
    # (e.g. a short restatement of what it understood) — never used for any
    # safety-relevant decision; purely informational if present.
    reasoning: str | None = None


class ValidatedIntent(BaseModel):
    """
    A RawIntent that has passed both Pydantic shape-checking and
    registry-based checking (action exists, params match that action's
    params_schema). `risk` here is *always* the static registry value for
    `action` — never anything the model might have supplied.
    """

    action: str
    params: dict[str, Any]
    risk: str


class ValidationOutcome(BaseModel):
    """
    Result of a single validation attempt. Exactly one of `intent` /
    `error` is set, matching `ok`.

    This does not decide whether to retry or fall back to `unmapped` —
    per the scope note above, that's the caller's job. This just reports
    what happened on this one attempt.
    """

    ok: bool
    intent: ValidatedIntent | None = None
    error: str | None = None


def validate_intent(raw: Any, registry: Registry) -> ValidationOutcome:
    """
    Validate a single Intent Parser response against the registry.

    Args:
        raw: the model's response, either already a dict (parsed JSON) or
            a RawIntent. Accepting a plain dict here means intent_parser.py
            can hand this function the raw Ollama JSON output directly.
        registry: the loaded, validated Registry (Build Order Step 3) to
            check `action`/`params` against.

    Returns:
        ValidationOutcome with ok=True and a ValidatedIntent (risk filled
        in from the registry, never from `raw`), or ok=False and a
        human-readable `error` describing what failed.
    """
    # Pass 1: is this even a well-formed intent object?
    if isinstance(raw, RawIntent):
        parsed = raw
    else:
        try:
            parsed = RawIntent.model_validate(raw)
        except ValidationError as exc:
            return ValidationOutcome(ok=False, error=f"Malformed intent shape: {exc}")

    # Pass 2: does `action` name a real, registered capability?
    if parsed.action not in registry:
        return ValidationOutcome(
            ok=False,
            error=f"Unknown action {parsed.action!r} — not in capability registry.",
        )

    # Pass 3: do `params` satisfy that action's own params_schema?
    # (Registry integrity — i.e. that params_schema is itself well-formed —
    # was already checked once at registry load time, Build Order Step 3.
    # This is the per-call check against this specific model response,
    # distinct from that one-time integrity check per Section 5.1's table.)
    params_schema = registry.params_schema_for(parsed.action)
    try:
        jsonschema.validate(instance=parsed.params, schema=params_schema)
    except jsonschema.ValidationError as exc:
        return ValidationOutcome(
            ok=False,
            error=(
                f"Params for action {parsed.action!r} failed schema check: "
                f"{exc.message} (at {'/'.join(str(p) for p in exc.absolute_path) or '<root>'})"
            ),
        )

    # risk is looked up from the registry — never taken from `raw`, even if
    # the model's response happened to include a risk-like field.
    return ValidationOutcome(
        ok=True,
        intent=ValidatedIntent(
            action=parsed.action,
            params=parsed.params,
            risk=registry.risk_for(parsed.action),
        ),
    )