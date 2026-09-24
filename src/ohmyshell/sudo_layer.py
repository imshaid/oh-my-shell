"""
Sudo/Permission Escalation Layer (Build Order Step 9).

Owns only the *decision*: given one plan step that needs elevated
permission, ask the user Grant / Skip / Abort and return that decision as
data. It does not itself invoke `sudo`, run any command, or write to the
audit log — those are the Executor's and Audit Log's jobs.

This layer only ever activates for steps from an AI-generated plan that are
marked as needing elevated permission. It is never invoked when the user
types `sudo` directly in a raw shell command — that goes straight to the OS
password prompt with no extra confirmation (main.py's raw-shell path never
calls into this module; only a Plan Generator step can reach here).

On ABORT, the Executor stops processing the remaining plan steps while
whatever already ran successfully stays done — the same "don't throw away
completed work" principle as SKIP.
"""

from __future__ import annotations

from dataclasses import dataclass
from enum import Enum, auto
from typing import Protocol


class SudoDecision(Enum):
    """The user's answer to one elevated-permission prompt."""

    GRANT = auto()
    SKIP = auto()
    ABORT = auto()


@dataclass(frozen=True)
class ElevatedStep:
    """
    One plan step that needs elevated permission (Section 8.3.6):

        ⚠ Next step requires elevated permission
        ┌─────────────────────────────────────────────────────┐
        │  Step 3: Clear system-level cache in /var/cache         │
        │  Reason: this directory is owned by root                │
        │  [Enter] Grant (sudo)   [s] Skip this step   [Esc] Abort  │
        └─────────────────────────────────────────────────────┘
    """

    step_number: int
    total_steps: int
    description: str
    reason: str


class SudoPrompt(Protocol):
    """
    Abstraction over however the UI actually asks the question — a plain
    REPL prompt (InputPrompt below) or a rich-rendered panel (ui/panels.py)
    can both implement this without this module needing to change.
    """

    def ask(self, step: ElevatedStep) -> SudoDecision: ...


def render_prompt_text(step: ElevatedStep) -> str:
    """Plain-text rendering of the elevated-permission prompt."""
    return (
        f"⚠ Next step requires elevated permission\n"
        f"  Step {step.step_number}/{step.total_steps}: {step.description}\n"
        f"  Reason: {step.reason}\n"
        f"  [Enter] Grant (sudo)   [s] Skip this step   [Esc/q] Abort"
    )


class InputPrompt:
    """
    Default `SudoPrompt` implementation: a blocking REPL prompt using plain
    `input()`.

    Key mapping (a real terminal Esc-key read needs raw/cbreak mode that
    plain `input()` cannot do, so this uses typed shortcuts instead):
      - empty line (bare Enter)  -> GRANT
      - "s" / "S"                -> SKIP
      - "esc", "q", "Q", "abort" -> ABORT
      - anything else            -> reprompt (invalid choice)
    """

    def __init__(self, *, input_fn: "callable[[str], str] | None" = None, print_fn: "callable[[str], None]" = print):
        # Default to None rather than binding `= input` directly, so tests
        # can monkeypatch this module's `input` name and have it take
        # effect (a default-argument value binds at definition time).
        self._input_fn = input_fn
        self._print_fn = print_fn

    def ask(self, step: ElevatedStep) -> SudoDecision:
        read = self._input_fn if self._input_fn is not None else input
        self._print_fn(render_prompt_text(step))
        while True:
            raw = read("> ")
            choice = raw.strip().lower()
            if choice == "":
                return SudoDecision.GRANT
            if choice == "s":
                return SudoDecision.SKIP
            if choice in ("esc", "q", "abort"):
                return SudoDecision.ABORT
            self._print_fn("Please press Enter to grant, 's' to skip, or 'esc'/'q' to abort.")


def decide_step(step: ElevatedStep, *, prompt: SudoPrompt | None = None) -> SudoDecision:
    """
    Ask the user what to do about one elevated-permission plan step and
    return their decision. Does not execute anything or write to the audit
    log itself — the Executor calls this once per step that needs elevated
    permission and:
      - on GRANT: runs the step's command with sudo (normal OS password
        prompt follows; Oh My Shell does not intercept or store it),
      - on SKIP: leaves the step un-run, continues with remaining steps,
        and records "1 step skipped" in the audit log,
      - on ABORT: stops the remaining plan, leaving already-completed steps
        as-is, and records the abort in the audit log.
    """
    active_prompt: SudoPrompt = prompt if prompt is not None else InputPrompt()
    return active_prompt.ask(step)