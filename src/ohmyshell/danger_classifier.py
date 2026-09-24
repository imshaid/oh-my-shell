"""
Danger Classifier (Build Order Step 8).

Flags raw shell commands as potentially destructive before they run
unattended (Section 8.3.5). Regex rules catch the clear, well-known
destructive patterns instantly and cheaply; anything the regex rules don't
recognize one way or the other falls back to an LLM judgment call — the
Build Order's own phrasing ("regex rules প্রথমে, LLM fallback পরে") is
followed literally here: regex is checked first and short-circuits when it
has an answer, the LLM is only consulted for the ambiguous middle ground.

Hard constraint (Section 8.4, line 853): a `--yes`/`-y` flag on the raw
command text must NEVER suppress this classifier or auto-confirm a
high-risk verdict — that's enforced by main.py (Step 6) simply always
calling this classifier and always showing the confirmation prompt on a
destructive verdict, regardless of what flags are in the command text; this
module itself has no special-case for `--yes`/`-y` because it has no
concept of "confirmation" to bypass in the first place — it only classifies.

Scope note (implementation decision): the regex rule set below is not from
the blueprint (which specifies the two-tier approach but not the specific
patterns) — these are the well-known classically-destructive Linux command
patterns (recursive force-delete, disk-level writes, filesystem creation
over an existing device, fork bombs, permission changes on system-critical
paths). This list is deliberately conservative and will under-classify
some destructive commands as SAFE (routed to the LLM fallback, or missed
entirely if the LLM also doesn't flag it) rather than trying to enumerate
every possible dangerous invocation — false negatives here are mitigated
by the trash/undo system elsewhere (a mistakenly-unflagged `rm` still goes
through a real shell and cannot be undone by this module regardless, since
raw commands bypass the plan/trash pipeline entirely per Section 8.3.5;
this classifier's job is only to add a confirmation step in front of
commands recognizable as dangerous, not to guarantee safety for all of
them).

--- Independent risk-override layer (added for the open-ended architecture,
Section 7.4's updated note; see validation.py's module docstring for the
full rationale) ---

Under the open-ended architecture, a command's `risk` starts as the
Intent Parser model's own self-assessment (validation.py's
ValidatedIntent.risk) rather than a static per-action registry lookup —
there is no registry mapping a free-form, model-generated command to a
risk level any more. This session's own empirical testing (an 84-prompt
battery against Gemini 3.1 Flash Lite and Gemini 3.5 Flash Lite, both
candidate providers) found BOTH models reproducibly under-risked two
specific classes of command: opening a network port / disabling a
firewall, and creating a passwordless or otherwise under-secured user
account. This reproduces, on different models, the exact failure pattern
the blueprint's original static-registry design was built to avoid
("Kill-process risk-consistency সব মডেলেই কমবেশি অস্থির").

`override_risk()` below is a small, independent, regex-based check —
deliberately narrow (it only targets the two specific blind spots actually
observed in testing, not a general risk re-assessment) — that force-
escalates risk to at least "high" for a command matching one of these
patterns, regardless of what the model itself said. It is independent of
both the LLM-based classify() fallback above and of the model that
produced the command in the first place, so a model that is wrong about
its own command's risk cannot suppress this check by simply saying "low"
more convincingly. Callers (intent_parser.py's consumers, i.e. main.py) are
expected to call this on every AI-generated command's (command, risk) pair
before showing the confirmation plan, exactly as they already run
classify() on every raw-shell command.
"""

from __future__ import annotations

import json
import os
import re
from dataclasses import dataclass
from typing import Protocol

# --- Regex rules: patterns confidently classified without any model call ------

# Each tuple: (compiled pattern, explanation, trash_alternative_possible).
# trash_alternative_possible marks whether the [t] "move to trash instead"
# option (Section 8.3.5) makes sense for this pattern — only true for
# file-deletion patterns, not for e.g. disk-wipe or fork-bomb patterns
# where there's nothing sensible to move to trash.
_DESTRUCTIVE_PATTERNS: list[tuple[re.Pattern, str, bool]] = [
    (
        re.compile(r"\brm\s+.*-[a-zA-Z]*r[a-zA-Z]*f|\brm\s+.*-[a-zA-Z]*f[a-zA-Z]*r"),
        "This recursively force-deletes files/directories with no confirmation "
        "and no way to recover them through normal means.",
        True,
    ),
    (
        re.compile(r"\bdd\s+.*of=/dev/"),
        "This writes raw data directly to a block device, which can destroy "
        "partition tables or all data on that device.",
        False,
    ),
    (
        re.compile(r"\bmkfs(\.\w+)?\s+/dev/"),
        "This creates a new filesystem on a device, erasing all existing data on it.",
        False,
    ),
    (
        re.compile(r":\(\)\s*\{\s*:\s*\|\s*:\s*&\s*\}\s*;\s*:"),
        "This is a fork bomb — it will spawn processes exponentially until the "
        "system runs out of resources and becomes unresponsive.",
        False,
    ),
    (
        re.compile(r"\bchmod\s+.*-R.*\b(000|777)\b.*(/etc|/usr|/bin|/boot|/sys|/proc)\b"),
        "This recursively changes permissions on a system-critical directory, "
        "which can break the system or expose it to security risks.",
        False,
    ),
    (
        re.compile(r"\bchown\s+.*-R.*\s+/(etc|usr|bin|boot)(\s|/|$)"),
        "This recursively changes ownership of a system-critical directory, "
        "which can break package management and system services.",
        False,
    ),
    (
        re.compile(r">\s*/dev/sd[a-z]\b"),
        "This redirects output directly onto a disk device, which can corrupt "
        "or destroy its contents.",
        False,
    ),
]

# Patterns confidently classified as SAFE regardless of what they touch —
# read-only inspection commands that might otherwise superficially resemble
# something risky (e.g. mentioning /etc without modifying it).
_SAFE_PATTERNS: list[re.Pattern] = [
    re.compile(r"^\s*(ls|cat|less|more|head|tail|grep|find|file|stat|du|df|ps|top|htop)\b"),
]


class Verdict:
    """Base for a classification outcome."""


@dataclass(frozen=True)
class Safe(Verdict):
    pass


@dataclass(frozen=True)
class Destructive(Verdict):
    explanation: str
    trash_alternative_possible: bool


@dataclass(frozen=True)
class ClassificationResult:
    verdict: Verdict
    source: str  # "regex" or "llm" — which tier produced this verdict


class DangerClassifierError(Exception):
    """Raised when the LLM fallback backend itself fails (not an ambiguous result)."""


class DangerLLMBackend(Protocol):
    def classify(self, command: str) -> dict:
        """Return the raw parsed JSON dict the model produced."""
        ...


_DEFAULT_MODEL = "gemini-3.5-flash-lite"

_SCHEMA = {
    "type": "object",
    "properties": {
        "destructive": {"type": "boolean"},
        "explanation": {"type": "string"},
        "trash_alternative_possible": {"type": "boolean"},
    },
    "required": ["destructive", "explanation", "trash_alternative_possible"],
}

_SYSTEM_PROMPT = (
    "You are a safety classifier for a Linux shell. Given one raw shell "
    "command, decide whether running it could cause irreversible data "
    "loss, system damage, or security compromise. Be conservative: if "
    "genuinely unsure, prefer destructive=true. trash_alternative_possible "
    "should be true only if the destructive effect is specifically file "
    "deletion that could instead be a move to a trash/recycle location."
)


class GoogleAIStudioDangerBackend:
    """LLM fallback backend for the danger classifier's second tier."""

    def __init__(self, model: str = _DEFAULT_MODEL, api_key: str | None = None):
        self.model = model
        self._api_key = api_key or os.environ.get("GOOGLE_AI_STUDIO_API_KEY")

    def classify(self, command: str) -> dict:
        from google import genai
        from google.genai import types

        if not self._api_key:
            raise DangerClassifierError(
                "GOOGLE_AI_STUDIO_API_KEY is not set — cannot call the danger classifier's LLM fallback."
            )

        try:
            client = genai.Client(api_key=self._api_key)
            response = client.models.generate_content(
                model=self.model,
                contents=command,
                config=types.GenerateContentConfig(
                    system_instruction=_SYSTEM_PROMPT,
                    response_mime_type="application/json",
                    response_schema=_SCHEMA,
                    temperature=0,
                    automatic_function_calling=types.AutomaticFunctionCallingConfig(disable=True),
                ),
            )
        except Exception as exc:
            raise DangerClassifierError(f"Google AI Studio call failed: {exc}") from exc

        try:
            return json.loads(response.text)
        except json.JSONDecodeError as exc:
            raise DangerClassifierError(f"Model response was not valid JSON: {exc}") from exc


# --- Independent risk-override patterns (see module docstring) ---------------
#
# Each tuple: (compiled pattern, reason). Deliberately narrow -- these exist
# to catch the SPECIFIC blind spots this session's own testing reproduced
# on two different Gemini models, not to be a general-purpose risk model.
_UNDER_RISKED_PATTERNS: list[tuple[re.Pattern, str]] = [
    (
        # ufw/firewall-cmd/iptables allow rules, or opening a port with nc/socat
        # in listen mode -- opening network exposure.
        re.compile(
            r"\bufw\s+allow\b"
            r"|\bfirewall-cmd\s+.*--add-port\b"
            r"|\biptables\s+.*-A\s+INPUT.*ACCEPT\b"
            r"|\b(nc|ncat|socat)\s+.*-l\b"
        ),
        "This opens a network port or allows traffic through the firewall — "
        "a real security-surface change that both tested cloud models "
        "reproducibly under-risked in this project's own evaluation.",
    ),
    (
        # useradd/adduser without a password step, or explicitly with an
        # empty/disabled password, or passwd -d (delete password).
        re.compile(
            r"\buseradd\b(?!.*-p\b)"
            r"|\badduser\b(?!.*--disabled-login\b.*--disabled-password\b)"
            r"|\bpasswd\s+-d\b"
        ),
        "This creates or leaves a user account without a password (or "
        "removes one) — both tested cloud models reproducibly under-risked "
        "this in this project's own evaluation.",
    ),
]


def override_risk(command: str, model_risk: str) -> str:
    """
    Independent re-check of an AI-generated command's own risk assessment
    (see module docstring's "Independent risk-override layer"). Returns the
    more severe of `model_risk` and whatever this function itself concludes
    — never lowers a risk the model already flagged higher.

    Args:
        command: the raw shell command text the model produced.
        model_risk: the model's own "low" | "medium" | "high" assessment
            (validation.py's ValidatedIntent.risk — already normalized to
            one of the three known levels by validate_intent()).

    Returns:
        "low" | "medium" | "high" — the effective risk to actually show the
        user, after this independent check.
    """
    order = {"low": 0, "medium": 1, "high": 2}
    baseline = model_risk if model_risk in order else "medium"
    effective = baseline

    for pattern, _reason in _UNDER_RISKED_PATTERNS:
        if pattern.search(command):
            effective = "high"
            break

    # Also defer to the existing regex-tier destructive patterns above --
    # anything already recognized as classically destructive (rm -rf, dd to
    # a device, mkfs, a fork bomb, broad chmod/chown on system paths) is at
    # least "high" here too, independent of what the model said.
    verdict = _regex_verdict(command)
    if isinstance(verdict, Destructive):
        effective = "high"

    # Never LOWER what the model itself already said -- this function only
    # escalates, it never overrides a model's own higher assessment.
    if order[baseline] > order[effective]:
        effective = baseline
    return effective


def _regex_verdict(command: str) -> Verdict | None:
    """Return a Verdict if a regex rule confidently matches, else None (ambiguous)."""
    for pattern, explanation, trash_possible in _DESTRUCTIVE_PATTERNS:
        if pattern.search(command):
            return Destructive(explanation=explanation, trash_alternative_possible=trash_possible)

    for pattern in _SAFE_PATTERNS:
        if pattern.search(command):
            return Safe()

    return None


def classify(
    command: str,
    *,
    llm_backend: DangerLLMBackend | None = None,
    model: str = _DEFAULT_MODEL,
) -> ClassificationResult:
    """
    Classify a raw shell command as safe or potentially destructive.

    Regex rules are checked first (Section 8.3.5's ordering) and used
    whenever they produce a confident answer. Only when no regex rule
    matches — the ambiguous middle ground — is the LLM fallback consulted.

    Args:
        command: the raw shell command text (already identified as
            RAW_SHELL by router.py — this function doesn't re-check that).
        llm_backend: override for testing/provider-swapping; defaults to a
            fresh GoogleAIStudioDangerBackend(model=model).
        model: Google AI Studio model name, used only if `llm_backend` is
            not given.

    Raises:
        DangerClassifierError: if the LLM fallback is needed and the
            backend itself fails. Callers (Step 6's main.py) are expected to
            fail safe on this — treat an unclassifiable command as
            destructive rather than silently running it — but that policy
            choice belongs to the caller, not this function.
    """
    regex_result = _regex_verdict(command)
    if regex_result is not None:
        return ClassificationResult(verdict=regex_result, source="regex")

    backend = llm_backend or GoogleAIStudioDangerBackend(model=model)
    raw = backend.classify(command)

    if raw.get("destructive"):
        verdict: Verdict = Destructive(
            explanation=raw.get("explanation", "The model flagged this as potentially destructive."),
            trash_alternative_possible=bool(raw.get("trash_alternative_possible", False)),
        )
    else:
        verdict = Safe()

    return ClassificationResult(verdict=verdict, source="llm")