"""
rich-rendered confirmation/warning panels (Build Order Step 11).

Section 10.1's own line for Step 11: "`ui/streaming.py`, `ui/prompt.py`,
`ui/panels.py` — `rich` দিয়ে visual polish (এতদিন CLI plain-text-এ কাজ
করছিল)" -- i.e. this step's whole job is re-rendering, with `rich`, output
that earlier steps already produce correctly as plain text. Nothing about
*what* gets shown or *when* changes here -- danger_classifier.py,
sudo_layer.py, and discussion.py still own that logic; this module only
turns their existing plain-text renderers into boxed rich.Panel output.

--- Important limitation, disclosed up front (Section 16 Rule 5) ---
This conversation's transcript is missing Section 8.3.1-8.3.3 of the
blueprint (a gap in what was captured earlier, confirmed with the user --
not something invented here). That means the *exact* panel mockup for the
Confirmation + Discussion Loop (8.3.3) is not available verbatim. The user
confirmed proceeding with the assistant's own design rather than blocking
on it, to be adjusted once the real blueprint text is available.

What this module's design IS anchored to, verbatim, from sections that
*are* confirmed (8.3.5, 8.3.6):
  - single-line-box style, e.g.:
        ⚠ Potentially destructive command detected
        [box with the explanation and options]
  - the sudo box's exact border style:
        ┌─────────────────────────────────────────────────────┐
        │  Step 3: ...                                          │
        │  [Enter] Grant (sudo)   [s] Skip this step   [Esc] Abort │
        └─────────────────────────────────────────────────────┘
  - a ⚠ glyph for warnings, [key] Label option hints, plain option text
    (no color spec given anywhere in the retrieved blueprint text for
    risk levels or option keys).

Design choices made here (undocumented in the retrieved blueprint,
therefore my own, consistent, adjustable decisions):
  - `rich.panel.Panel` with `box.ROUNDED` for all three panel kinds (plan,
    destructive-command warning, sudo-escalation) -- one consistent visual
    language across the app rather than a different box style per panel.
  - Border/title color by severity: yellow for a warning-level panel
    (destructive command, sudo escalation) and cyan for a neutral
    plan/confirmation panel -- matching common CLI convention (yellow =
    caution) since the blueprint specifies no explicit palette.
  - Risk-level text color: low=green, medium=yellow, high=red -- the
    single most standard traffic-light mapping, used only as inline text
    styling (e.g. "[red]high[/red]"), not as a structural decision.
These are cosmetic and trivially swappable once the real 8.3.1-8.3.3 text
is available -- nothing downstream depends on the exact colors/box style.
"""

from __future__ import annotations

from rich.console import Console, RenderableType
from rich.panel import Panel
from rich.text import Text

from ohmyshell.danger_classifier import ClassificationResult, Destructive
from ohmyshell.plan_generator import Plan
from ohmyshell.sudo_layer import ElevatedStep, SudoDecision

_RISK_COLORS = {"low": "green", "medium": "yellow", "high": "red"}


def _risk_text(risk: str) -> Text:
    color = _RISK_COLORS.get(risk, "white")
    return Text(risk.capitalize(), style=color)


def render_plan_panel(plan: Plan) -> Panel:
    """
    Boxed rendering of a Plan for the Confirmation + Discussion Loop
    (Section 8.3.3 -- see module docstring's disclosed gap: the exact
    mockup for this specific panel is not available verbatim, so this is
    the assistant's own design, built to carry the same information as
    discussion.py's plain-text render_plan_text()).
    """
    body = Text()
    for i, step in enumerate(plan.steps, start=1):
        body.append(f"{i}. {step}\n")
    body.append("\n")
    body.append("Risk: ")
    body.append(_risk_text(plan.risk))
    body.append("\n")
    body.append("[Enter] Confirm   [e] Edit   [c] Chat/adjust   [Esc] Cancel", style="dim")

    return Panel(
        body,
        title=f"Plan — {plan.action}",
        border_style="cyan",
        expand=False,
    )


def render_destructive_command_panel(result: ClassificationResult) -> Panel:
    """
    Boxed rendering of the raw-command danger warning (Section 8.3.5,
    verbatim-confirmed mockup):

        ⚠ Potentially destructive command detected

        <explanation>

        [y] Run anyway   [n] Cancel   [t] Move to trash instead

    `[t]` is only shown when the verdict says a trash alternative exists
    (trash_alternative_possible), matching main.py's own existing logic.
    """
    verdict = result.verdict
    if not isinstance(verdict, Destructive):
        raise TypeError("render_destructive_command_panel expects a Destructive verdict")

    body = Text()
    body.append(f"{verdict.explanation}\n\n")
    options = "[y] Run anyway   [n] Cancel"
    if verdict.trash_alternative_possible:
        options += "   [t] Move to trash instead"
    body.append(options, style="dim")

    return Panel(
        body,
        title="⚠ Potentially destructive command detected",
        title_align="left",
        border_style="yellow",
        expand=False,
    )


def render_sudo_panel(step: ElevatedStep) -> Panel:
    """
    Boxed rendering of the sudo/permission-escalation prompt (Section
    8.3.6, verbatim-confirmed mockup):

        ⚠ Next step requires elevated permission
        ┌─────────────────────────────────────────────────────┐
        │  Step 3: Clear system-level cache in /var/cache         │
        │  Reason: this directory is owned by root                │
        │  [Enter] Grant (sudo)   [s] Skip this step   [Esc] Abort  │
        └─────────────────────────────────────────────────────┘
    """
    body = Text()
    body.append(f"Step {step.step_number}/{step.total_steps}: {step.description}\n")
    body.append(f"Reason: {step.reason}\n\n")
    body.append("[Enter] Grant (sudo)   [s] Skip this step   [Esc/q] Abort", style="dim")

    return Panel(
        body,
        title="⚠ Next step requires elevated permission",
        title_align="left",
        border_style="yellow",
        expand=False,
    )


def render_undo_confirm_panel(*, count: int) -> Panel:
    """
    Boxed rendering of the undo confirmation (Section 8.3.7, verbatim):

        ↺ Undo: Restore 340 files from .trash/?
        [Enter] Confirm undo   [Esc] Cancel
    """
    noun = "file" if count == 1 else "files"
    body = Text()
    body.append(f"↺ Undo: Restore {count} {noun} from .trash/?\n\n")
    body.append("[Enter] Confirm undo   [Esc] Cancel", style="dim")

    return Panel(body, border_style="cyan", expand=False)


def print_panel(panel: RenderableType, *, console: Console | None = None) -> None:
    """Render any of the above panels to the terminal (or an injected Console)."""
    active_console = console if console is not None else Console()
    active_console.print(panel)


class RichSudoPrompt:
    """
    The Step 11 `SudoPrompt` implementation sudo_layer.py's own docstring
    already names as the eventual replacement for `InputPrompt` -- same
    Grant/Skip/Abort decision logic, boxed via render_sudo_panel() instead
    of sudo_layer.render_prompt_text()'s plain string.

    Key mapping is intentionally identical to InputPrompt (Section 8.3.6's
    own confirmed default): bare Enter -> GRANT, "s" -> SKIP, "esc"/"q"/
    "abort" -> ABORT, anything else re-prompts. Only the *rendering*
    changes here -- the decision contract (SudoPrompt.ask -> SudoDecision)
    is unchanged, so executor.py needs no changes to accept this in place
    of InputPrompt.
    """

    def __init__(
        self,
        *,
        input_fn: "callable[[str], str] | None" = None,
        console: Console | None = None,
    ) -> None:
        self._input_fn = input_fn
        self._console = console if console is not None else Console()

    def ask(self, step: ElevatedStep) -> SudoDecision:
        read = self._input_fn if self._input_fn is not None else input
        self._console.print(render_sudo_panel(step))
        while True:
            raw = read("> ")
            choice = raw.strip().lower()
            if choice == "":
                return SudoDecision.GRANT
            if choice == "s":
                return SudoDecision.SKIP
            if choice in ("esc", "q", "abort"):
                return SudoDecision.ABORT
            self._console.print(
                "[dim]Please press Enter to grant, 's' to skip, or 'esc'/'q' to abort.[/dim]"
            )