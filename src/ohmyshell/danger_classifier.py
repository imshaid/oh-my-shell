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
"""

from __future__ import annotations

import json
import re
from dataclasses import dataclass
from typing import Protocol

import ollama

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


class OllamaDangerBackend:
    """
    LLM fallback backend (Section 8.3.5's second tier). Same think:false /
    schema-constrained-decoding contract as intent_parser.py's OllamaBackend
    (Section 7.3/7.4a) — this is a separate, much smaller schema since the
    only question here is "destructive or not, and why", not action/params
    selection.
    """

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

    def __init__(self, model: str):
        self.model = model

    def classify(self, command: str) -> dict:
        try:
            response = ollama.chat(
                model=self.model,
                messages=[
                    {"role": "system", "content": self._SYSTEM_PROMPT},
                    {"role": "user", "content": command},
                ],
                format=self._SCHEMA,
                think=False,
                options={"temperature": 0},
            )
        except Exception as exc:
            raise DangerClassifierError(f"Ollama call failed: {exc}") from exc

        try:
            return json.loads(response.message.content)
        except json.JSONDecodeError as exc:
            raise DangerClassifierError(f"Model response was not valid JSON: {exc}") from exc


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
    model: str = "qwen3:8b",
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
            fresh OllamaDangerBackend(model=model).
        model: Ollama model name, used only if `llm_backend` is not given.

    Raises:
        DangerClassifierError: if the LLM fallback is needed and the
            backend itself fails (e.g. Ollama unreachable). Callers (Step 6's
            main.py) are expected to fail safe on this — treat an
            unclassifiable command as destructive rather than silently
            running it — but that policy choice belongs to the caller, not
            this function.
    """
    regex_result = _regex_verdict(command)
    if regex_result is not None:
        return ClassificationResult(verdict=regex_result, source="regex")

    backend = llm_backend or OllamaDangerBackend(model=model)
    raw = backend.classify(command)

    if raw.get("destructive"):
        verdict: Verdict = Destructive(
            explanation=raw.get("explanation", "The model flagged this as potentially destructive."),
            trash_alternative_possible=bool(raw.get("trash_alternative_possible", False)),
        )
    else:
        verdict = Safe()

    return ClassificationResult(verdict=verdict, source="llm")