"""
Sudo/Permission Escalation Layer (Build Order Step 9).

Implements the "simple explicit approve-flow" described in Build Order Step 9
("Module 3-এর pty-automation stretch goal-এর আগে") -- the pty-based automation
mentioned there is explicitly a stretch goal and is NOT implemented here.

Scope decision (Section 16 Rule 5 -- documented since the Executor, Step 10,
does not exist yet): this module owns only the *decision* -- given one plan
step that needs elevated permission, ask the user Grant / Skip / Abort and
return that decision as data. It does not itself invoke `sudo`, run any
command, or write to the audit log. Those are the Executor's (Step 10) and
Audit Log's (Step 12) jobs respectively; this module exposes a small, stable
interface (`decide_step`, `SudoDecision`) that those modules will call into
once they exist, so no code written here needs to change when they land.

Trigger rule (Section 8.3.6, line ~736): this layer only ever activates for
steps that come from an AI-generated plan and are marked as needing elevated
permission. It is explicitly NOT invoked when the user types `sudo` directly
in a raw shell command -- that goes straight to the OS password prompt with
no extra Oh My Shell confirmation, because the user already made an informed
decision themselves (this is enforced by construction: main.py's raw-shell
path in _handle_raw_shell never calls into this module at all; only a
plan step -- something the Plan Generator produced -- can reach here).

Abort behavior (documented assumption, Section 16 Rule 5): the blueprint's
sudo-escalation box spells out Skip's behavior explicitly ("বাকি সম্পন্ন কাজ
রক্ষা পায় ... audit log-এ 'skipped' নোট থাকে") but never states, beyond the
"[Esc] Abort" menu label itself, what Abort does. The reasonable, standard
reading of "Abort" next to "Skip this step" is: stop processing the
*remaining* steps of the plan immediately, while whatever steps already ran
successfully before this one stay done (the same "don't throw away completed
work" principle the blueprint states explicitly for Skip). This module
encodes that reading as the `ABORT` decision; the Executor (Step 10) is
expected to honor it by not proceeding to any further step in the plan.
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
    One plan step that needs elevated permission.

    `step_number`/`total_steps` and `description`/`reason` map directly onto
    the blueprint's mockup box (Section 8.3.6):

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
    Abstraction over however the UI actually asks the question.

    Step 9 only needs a plain-text REPL prompt; Step 11 replaces the
    implementation with a `rich`-rendered panel matching the blueprint's
    mockup exactly, without this module (or its tests) needing to change --
    only the concrete `SudoPrompt` implementation passed in changes.
    """

    def ask(self, step: ElevatedStep) -> SudoDecision: ...


def render_prompt_text(step: ElevatedStep) -> str:
    """
    Plain-text rendering of the elevated-permission prompt (Step 9's own
    UI -- rich panels are Step 11's job, same split as plan_generator.py /
    discussion.py already use via render_plan_text).
    """
    return (
        f"⚠ Next step requires elevated permission\n"
        f"  Step {step.step_number}/{step.total_steps}: {step.description}\n"
        f"  Reason: {step.reason}\n"
        f"  [Enter] Grant (sudo)   [s] Skip this step   [Esc/q] Abort"
    )


class InputPrompt:
    """
    Default `SudoPrompt` implementation for Step 9: a blocking REPL prompt
    using plain `input()`.

    Key mapping (documented default, since a real terminal Esc-key read
    needs raw/cbreak mode that plain `input()` cannot do -- that belongs to
    Step 11's rich/questionary-based UI layer, not here):
      - empty line (bare Enter)  -> GRANT
      - "s" / "S"                -> SKIP
      - "esc", "q", "Q", "abort" -> ABORT
      - anything else            -> reprompt (invalid choice)
    """

    def __init__(self, *, input_fn: "callable[[str], str] | None" = None, print_fn: "callable[[str], None]" = print):
        # `input_fn` defaults to None (not the `input` builtin directly) so
        # that tests can monkeypatch this module's `input` name and have it
        # take effect -- a default-argument value binds at function
        # definition time, so `= input` here would freeze the reference to
        # the ORIGINAL builtin, and be invisible to monkeypatch.
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
    return their decision.

    This function does not execute anything and does not write to the audit
    log -- see the module docstring for why. The Executor is expected to
    call this once per plan step that the registry/plan marks as needing
    elevated permission, and to:
      - on GRANT: run the step's command with sudo (normal OS password
        prompt follows; Oh My Shell does not intercept or store it),
      - on SKIP: leave the step un-run, keep going with the remaining
        steps, and record "1 step skipped" in the audit log,
      - on ABORT: stop the whole remaining plan, leaving already-completed
        steps as-is, and record the abort in the audit log.
    """
    active_prompt: SudoPrompt = prompt if prompt is not None else InputPrompt()
    return active_prompt.ask(step)