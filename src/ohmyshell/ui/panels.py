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
  - Border/title color by severity: `omsh.warning` for a warning-level
    panel (destructive command, sudo escalation) and `omsh.accent` for a
    neutral plan/confirmation panel.
  - Risk-level text color: low=`omsh.risk.low`, medium=`omsh.risk.medium`,
    high=`omsh.risk.high`, used only as inline text styling (e.g.
    "[omsh.risk.high]high[/omsh.risk.high]"), not as a structural decision.

--- Fixed accent palette (post-Build-Order, user-requested) ---
Every color above used to be a `rich` NAMED color string ("cyan",
"yellow", "green", "red") -- terminal-theme-relative by design, which is
exactly right for *command output* (ls/grep/etc.'s own colors) but not
what was wanted for THIS app's own chrome: the person asked for
oh-my-shell's panels/prompts to have a consistent, recognizable "brand"
look across any terminal/theme, the same way Claude Code's own CLI always
renders the same accent color regardless of the terminal it's running in.
See ui/theme.py's own module docstring for the full rationale and the
fixed hex values themselves. Every color reference in this module is now
one of ui/theme.py's `"omsh.*"` style names instead of a raw color
string; `print_panel`/`RichSudoPrompt` construct their fallback `Console`
via `ui.theme.themed_console()` (not a bare `rich.console.Console()`) so
those style names actually resolve wherever no console is injected by a
caller.
"""

from __future__ import annotations

import re

from rich.console import Console, Group, RenderableType
from rich.panel import Panel
from rich.rule import Rule
from rich.text import Text

from ohmyshell.danger_classifier import ClassificationResult, Destructive
from ohmyshell.intent_parser import ParseTelemetry
from ohmyshell.plan_generator import Plan
from ohmyshell.sudo_layer import ElevatedStep, SudoDecision
from ohmyshell.ui.theme import themed_console

_RISK_STYLES = {"low": "omsh.risk.low", "medium": "omsh.risk.medium", "high": "omsh.risk.high"}

# Matches one bracketed key token, e.g. "[Enter]", "[e]", "[Esc/q]" -- used
# by `_key_hint_text` to split an option-hint line ("[Enter] Confirm   [e]
# Edit   ...") into its "[key]" and "label" pieces so each can carry its
# own style, without needing a second hand-maintained copy of the line's
# text in a structured (list-of-tuples) shape at every call site.
_KEY_TOKEN_RE = re.compile(r"(\[[^\]]+\])")


def _key_hint_text(line: str) -> Text:
    """
    Color-audit fix (post-Build-Order, "I want to add color in
    everywhere", extended to every "[key] Label" option-hint line in this
    module): these lines used to be one flat `omsh.muted` string --
    Nord4, a very light near-white shade that reads as almost
    indistinguishable from plain unstyled text on a real dark terminal
    (see ui/theme.py's own docstring for the same shade's earlier
    legibility issue elsewhere). The person confirmed (asked directly,
    since this is a judgment call affecting every panel in this module,
    not just one line) that every "[key]" token app-wide should be
    colored consistently with `omsh.accent` -- this app's own signature
    accent, already used for /help's own command names and the palette's
    command-name column (ui/palette.py) -- while the label text after it
    stays `omsh.muted`, so a person scanning a panel can pick out "what
    key do I press" at a glance the same way they scan `/help`'s own
    command list for a name.

    Splits on `_KEY_TOKEN_RE` rather than hand-building a fresh `Text`
    per call site with hardcoded slice indices -- call sites (plan panel,
    destructive-command panel, sudo panel) already have this exact
    "[key] Label   [key] Label   ..." string built for other reasons
    (matching the blueprint's verbatim mockups); this only needs to
    re-style it, not reconstruct the wording.
    """
    text = Text()
    for chunk in _KEY_TOKEN_RE.split(line):
        if not chunk:
            continue
        if _KEY_TOKEN_RE.fullmatch(chunk):
            text.append(chunk, style="omsh.accent")
        else:
            text.append(chunk, style="omsh.muted")
    return text


def _risk_text(risk: str) -> Text:
    style = _RISK_STYLES.get(risk, "omsh.muted")
    return Text(risk.capitalize(), style=style)


def _telemetry_footer_text(telemetry: ParseTelemetry, *, attempts: int | None = None) -> Text | None:
    """
    Section 8.3.3's plan-panel footer, verbatim mockup:
    "↯ 94 tokens in · 62 tokens out · 0.8s · qwen3:8b" -- shown below a
    horizontal rule inside the same panel box. Extended (per the user's own
    "share your thoughts freely" invitation and confirmed selections) with
    tokens/sec and a retry count when relevant -- tokens_in doubling as the
    "context/prompt size" figure the user separately asked for, since that
    number already IS the size of the prompt sent to the model; a second,
    differently-labeled figure for the same count would be redundant.

    Any field ParseTelemetry doesn't have (a backend that couldn't report
    it -- see ParseTelemetry's own docstring) is simply left out of this
    line rather than shown as a placeholder like "? tokens" -- an honest
    partial line beats a fake-looking complete one. Returns None only when
    NOTHING is available at all (every field is None and no retry
    happened), so a genuinely empty telemetry object doesn't add an empty
    footer/rule to the panel.

    `attempts` (from ParseResult.attempts) adds "· retried once" only when
    it is 2 or more -- a normal, single-attempt call shows nothing extra,
    per the user's own "Retry count যদি ১-এর বেশি হয়" selection (only
    surface it when it actually happened).
    """
    parts: list[str] = []
    if telemetry.tokens_in is not None:
        parts.append(f"{telemetry.tokens_in} tokens in")
    if telemetry.tokens_out is not None:
        parts.append(f"{telemetry.tokens_out} tokens out")
    if telemetry.duration_seconds is not None:
        parts.append(f"{telemetry.duration_seconds:.1f}s")
    if (
        telemetry.tokens_out is not None
        and telemetry.duration_seconds is not None
        and telemetry.duration_seconds > 0
    ):
        tokens_per_second = telemetry.tokens_out / telemetry.duration_seconds
        parts.append(f"{tokens_per_second:.0f} tok/s")
    if telemetry.model is not None:
        parts.append(telemetry.model)
    if attempts is not None and attempts > 1:
        retry_count = attempts - 1
        noun = "retry" if retry_count == 1 else "retries"
        parts.append(f"{retry_count} {noun}")
    if not parts:
        return None
    return Text(f"↯ {' · '.join(parts)}", style="omsh.muted")


def render_plan_panel(
    plan: Plan, *, telemetry: ParseTelemetry | None = None, attempts: int | None = None
) -> Panel:
    """
    Boxed rendering of a Plan for the Confirmation + Discussion Loop
    (Section 8.3.3's confirmed mockup).

    `telemetry` (intent_parser.ParseTelemetry, optional) adds the mockup's
    footer line -- real tokens-in/tokens-out/elapsed-time/model/tokens-per-
    second, separated from the plan body by a horizontal rule, exactly as
    shown in Section 8.3.3:

        │  Risk: Medium  ·  Est. 340 files  ·  ~1.2 GB             │
        ├───────────────────────────────────────────────────────┤
        │  ↯ 94 tokens in · 62 tokens out · 0.8s · 77 tok/s · qwen3:8b │

    `attempts` (intent_parser.ParseResult.attempts, optional) adds a
    "· N retries" segment to that same line, but only when it is 2 or
    more -- see _telemetry_footer_text's own docstring.

    Omitted entirely (no rule, no empty line) when telemetry is None or
    carries no usable fields and no retry happened -- callers that don't
    have real numbers yet (or a backend that can't report them) get
    exactly the panel this function rendered before telemetry existed, not
    a broken-looking line.
    """
    body_text = Text()
    for i, step in enumerate(plan.steps, start=1):
        body_text.append(f"{i}. {step}\n")
    body_text.append("\n")
    body_text.append("Risk: ")
    body_text.append(_risk_text(plan.risk))
    body_text.append("\n")
    body_text.append_text(_key_hint_text("[Enter] Confirm   [e] Edit   [c] Chat/adjust   [Esc] Cancel"))

    footer = _telemetry_footer_text(telemetry, attempts=attempts) if telemetry is not None else None
    if footer is None:
        body: RenderableType = body_text
    else:
        body = Group(body_text, Rule(style="omsh.muted"), footer)

    return Panel(
        body,
        title=f"Plan — {plan.command}",
        border_style="omsh.accent",
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
    body.append_text(_key_hint_text(options))

    return Panel(
        body,
        title="⚠ Potentially destructive command detected",
        title_align="left",
        border_style="omsh.warning",
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
    body.append_text(_key_hint_text("[Enter] Grant (sudo)   [s] Skip this step   [Esc/q] Abort"))

    return Panel(
        body,
        title="⚠ Next step requires elevated permission",
        title_align="left",
        border_style="omsh.warning",
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
    body.append_text(_key_hint_text("[Enter] Confirm undo   [Esc] Cancel"))

    return Panel(body, border_style="omsh.accent", expand=False)


def print_panel(panel: RenderableType, *, console: Console | None = None) -> None:
    """Render any of the above panels to the terminal (or an injected Console)."""
    active_console = console if console is not None else themed_console()
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

    --- Esc/repeated-"s" bug fix (post-Build-Order, found via real-terminal
    testing) ---
    `input_fn`, when not overridden, now defaults to
    `ui.session.read_sudo_choice_keypress` -- a real single-keypress reader
    bound to the actual Esc key event and to "s"/"q" as immediate-submit
    keys, not a `ReplSession.prompt()` line-editing read compared against
    typed text after Enter. The previous default -- `input_fn=read` passed
    in from main.py, `read` being the REPL's `ReplSession` -- meant
    pressing "s" only inserted the character into a line buffer that
    wasn't submitted until Enter was pressed, which real-terminal testing
    showed as "s" appearing to need several presses (it was never actually
    submitted the first several times) and Esc doing nothing at all, same
    root cause as `_repl_get_user_choice`'s identical bug in main.py (see
    `read_plan_choice_keypress`'s own docstring for the full explanation).
    `input_fn` stays overridable (main.py's edit/chat-adjust sub-prompts,
    and every existing test in this module, still pass an explicit
    `input_fn` lambda) since only the plain no-argument construction path
    is the one real callers hit during normal operation.
    """

    def __init__(
        self,
        *,
        input_fn: "callable[[str], str] | None" = None,
        console: Console | None = None,
    ) -> None:
        self._input_fn = input_fn
        self._console = console if console is not None else themed_console()

    def ask(self, step: ElevatedStep) -> SudoDecision:
        self._console.print(render_sudo_panel(step))
        while True:
            if self._input_fn is None:
                from ohmyshell.ui.session import read_sudo_choice_keypress

                choice = read_sudo_choice_keypress().strip().lower()
            else:
                choice = self._input_fn("> ").strip().lower()
            if choice in ("", "grant"):
                return SudoDecision.GRANT
            if choice in ("s", "skip"):
                return SudoDecision.SKIP
            if choice in ("esc", "q", "abort"):
                return SudoDecision.ABORT
            self._console.print(
                "[omsh.muted]Please press Enter to grant, 's' to skip, or 'esc'/'q' to abort.[/omsh.muted]"
            )