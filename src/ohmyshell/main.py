"""
Shell REPL loop, entrypoint (Build Order Step 6, danger-check integrated
Step 8, full Meta-Command Handler wired in Step 14, full Section 4.1
pipeline wired post-Build-Order, full `rich`/`prompt_toolkit` visual
polish wired post-Build-Order — Step 11).

Per the Build Order's own scoping for Step 6 ("basic REPL loop, raw
pass-through প্রথমে — সবচেয়ে সহজ path"), this was initially NOT the full
Section 4.1 pipeline. Every later step filled in one piece; this module now
wires all of them together end to end:

- RAW_SHELL input is classified (danger_classifier.py, Step 8) before
  running. A Destructive verdict shows Section 8.3.5's confirmation prompt
  with all three options: [y] Run anyway / [n] Cancel / [t] Move to trash
  instead (only offered when the verdict says trash_alternative_possible;
  [t] moves the command's target into .trash/ via trash.py, Step 10,
  instead of running the original destructive command at all).
- NATURAL_LANGUAGE input now runs the full pipeline (Section 4.1): Intent
  Parser (Step 5) -> Plan Generator (Step 7) -> Confirmation + Discussion
  Loop (Step 7/11) -> Sudo Layer (Step 9, only if a step needs elevated
  permission) -> Streaming Executor (Step 10) -> Audit Log (Step 12). See
  _handle_natural_language's own docstring for the exact wiring.
- SLASH_COMMAND input goes through the full Meta-Command Handler
  (meta_commands.py, Step 14) — /help, /model, /history, /undo, /trash,
  /log, /capabilities, /explain, /stats, /system, /config, /clear, /exit,
  /quit are all recognized.

--- Step 11 visual-polish wiring (post-Build-Order) ---
Until this pass, every render in this module went through plain `print`/
`print_fn`, even though ui/panels.py (boxed rich.Panel confirmation/warning
prompts) and ui/streaming.py (rich.Live execution progress) already existed
and were already unit-tested — they were simply never called from here.
Found and fixed after the person compared a real session's output against
other CLIs' prompt/output styling (fish, nushell, exa) and it looked
"noisy, non-aligned, generic" by contrast — not a functional bug, but a
real gap between what Step 11 was supposed to deliver and what actually
reached the terminal.

What changed, module by module:
  - ui/prompt.py: `_render_prompt` (plain "<folder> (<model>) ❯ " string)
    is replaced by ui/prompt.py's `render_prompt_ansi`, which reuses the
    same content/logic but colors the folder name and the icon (cyan ❯).
    ui/prompt.py's `ai_active` parameter (icon swap to magenta ✦) is not
    driven from here: this REPL's `session.prompt()` call blocks until the
    *next* line is typed, so there is no later moment in the same prompt
    render to redraw it "while" a request runs — the earliest an
    `ai_active=True` render could show is the *next* prompt after the
    request already finished, which would misrepresent what's currently
    happening rather than reflect it. Actually redrawing the live prompt
    mid-request would need a different input mechanism (a background
    thread refreshing a prompt_toolkit Application, not a single blocking
    `.prompt()` call) — left as a real Step-11-completing follow-up rather
    than faked here with a parameter that would always render `False`.
  - ui/session.py: the bare `input()` REPL read is replaced by a
    `ReplSession` (prompt_toolkit-backed) so the colored prompt actually
    renders with working line-editing (`input()` cannot safely take a
    pre-ANSI-escaped string — see ui/session.py's own docstring for why
    prompt_toolkit specifically, not just wrapping input() in more rich
    markup). `ReplSession` is also used as the `read` callable everywhere
    a raw `input` was passed before (edit-param sub-prompts, chat-adjust
    text, sudo-decision reads) — its `__call__` makes it a drop-in
    replacement for every existing `read: callable = input` parameter
    throughout this file, discussion.py, and sudo_layer.py.
  - ui/panels.py: the plan (confirm/edit/chat/cancel), destructive-command
    warning, and sudo-escalation prompts are now rendered as boxed
    rich.Panel output (render_plan_panel / render_destructive_command_panel
    / render_sudo_panel) instead of plain print() lines. `RichSudoPrompt`
    (new, alongside these) replaces `sudo_layer.InputPrompt` as the
    `SudoPrompt` passed into `run_plan` — same Grant/Skip/Abort decision
    contract, boxed rendering.
  - ui/streaming.py: execution progress now goes through
    `StreamingRenderer` (a `rich.Live` spinner that collapses to a
    ✓/✗/⚠/⊘ summary line per step) instead of one flat print per StepEvent.

Every function below that used to take `print_fn: callable = print` now
takes `console: Console | None = None` instead (defaulting to a fresh rich
Console, matching ui/panels.py's own `print_panel` convention) — tests
inject a `Console(file=io.StringIO(), force_terminal=False)` and assert
against the buffer's rendered text, the same pattern ui/panels.py's and
ui/streaming.py's own test suites already established. `read: callable`
parameters are unchanged in shape (still "a thing callable with an
optional prompt string that returns str") — a `ReplSession` or a plain
test fake both satisfy that shape identically, so discussion.py and
sudo_layer.py needed no contract changes at all for this pass.

--- Spacing + "Thinking..." spinner (post-Build-Order, same Step 11 pass) ---
Two more gaps found the same way (a real session compared side-by-side
against other CLIs):
  - No blank line separated one turn's output from the next prompt, so a
    session read as one unbroken wall of text. Fixed with a single
    `console.print()` right before every `session.prompt(...)` call in
    `run()`'s loop (see that loop's own comment for why one seam there,
    not scattered blank-line prints throughout every handler).
  - Both blocking `parse_intent()` calls in `_handle_natural_language`
    (the initial parse, and `_reparse`'s chat-adjust re-parse) gave no
    feedback at all while Ollama was generating — the terminal just sat
    there. Wrapped both in `console.status("Thinking...", spinner="dots")`
    (rich's own spinner context manager) so there's visible activity
    during what can be a multi-second model call. This is NOT the "real
    live token streaming" the person separately asked for (a spinner
    still resolves to the complete parsed result all at once) — that is
    a distinct, larger follow-up (rewiring intent_parser.py's Ollama call
    to stream=True and incrementally parse partial JSON), intentionally
    scoped out of this pass and tracked separately.

`--yes`/`-y`/`--dry-run`/`--verbose`/`--quiet` (Section 8.5) are still not
implemented — no inline-modifier parsing exists on either the raw-shell or
natural-language input paths yet. The one hard constraint that DOES apply
today — `--yes`/`-y` must never bypass a destructive-command confirmation —
remains satisfied by construction: neither _handle_raw_shell nor the NL
pipeline inspects command/request text for these flags at all.
"""

from __future__ import annotations

import shlex
import subprocess
import sys
import time
from pathlib import Path

from rich.console import Console
from rich.text import Text

from ohmyshell import audit_log as audit_log_module
from ohmyshell import config as config_module
from ohmyshell import meta_commands
from ohmyshell import trash as trash_module
from ohmyshell.danger_classifier import Destructive, DangerClassifierError, classify
from ohmyshell.discussion import Cancelled, Confirmed, edit_step_param, run_discussion
from ohmyshell.executor import StepStatus, run_plan
from ohmyshell.intent_parser import IntentParseError, parse_intent
from ohmyshell.plan_generator import Plan, generate_plan
from ohmyshell.registry import Registry, RegistryError
from ohmyshell.registry import load as load_registry
from ohmyshell.router import InputKind, route
from ohmyshell.ui.panels import (
    RichSudoPrompt,
    render_destructive_command_panel,
    render_plan_panel,
)
from ohmyshell.ui.prompt import render_prompt_ansi
from ohmyshell.ui.session import ReplSession
from ohmyshell.ui.streaming import StreamingRenderer

EXIT_COMMANDS = {"/exit", "/quit"}

# One consistent banner shown once at startup — gives the app a signature
# look on launch rather than dropping straight into a bare prompt (no
# blueprint mockup covers this exactly; kept intentionally small/quiet so
# it doesn't compete with /help's own reference text).
_BANNER = Text.from_markup(
    "[bold cyan]✦ Oh My Shell[/bold cyan] [dim]— natural-language Linux shell[/dim]\n"
    "[dim]Type naturally, or /help for commands.[/dim]"
)


def _extract_trash_target(text: str) -> str | None:
    """
    Best-effort guess at "the path this raw command would have deleted",
    for the [t] move-to-trash-instead option on a raw shell command.

    Design note (Section 16 Rule 5 -- this is this file's own decision, not
    blueprint text): a raw command is free-form shell syntax (e.g.
    "rm -rf /tmp/build", "rm -rf ./old_logs/*"), not a structured plan with
    a named `path` param the way Plan Generator/Executor's params dict is.
    There is no reliable general way to know "the target" of an arbitrary
    shell command. The heuristic used here: take the last whitespace-
    separated token that isn't a flag (doesn't start with "-") -- this
    covers the common `rm [-flags...] <path>` shape (including the
    trailing-glob case `rm -rf ./old_logs/*`, since glob expansion is the
    shell's job and `shlex.split` still returns the literal glob token
    here). This is deliberately conservative: if no such token is found
    (e.g. a fork-bomb pattern, or a `dd`/`mkfs` device-write with no plain
    trailing path), [t] is simply not offered -- matching
    `trash_alternative_possible`'s own purpose of gating this option to
    the cases it can plausibly apply to. Multi-path commands
    (`rm -rf a b c`) only recover the LAST path with this heuristic; that
    limitation is accepted here rather than guessed around, since a fuller
    shell-argument parser is out of scope for a CEP-level raw-shell escape
    hatch that already has [n] Cancel as a safe fallback. The command word
    itself (the first token, e.g. "rm") is never returned as a target -- a
    command with no path argument at all (just flags, e.g. "rm -rf") has no
    real target to recover, and returning the command name in that case
    would silently trash a file that happens to be named "rm" if one
    exists in the current directory, which is worse than just not offering
    [t] at all.
    """
    try:
        tokens = shlex.split(text)
    except ValueError:
        return None
    if not tokens:
        return None
    for token in reversed(tokens[1:]):
        if token and not token.startswith("-"):
            return token
    return None


def _handle_raw_shell(
    text: str, cfg: dict, *, confirm: callable = input, console: Console | None = None, base_dir=None
) -> None:
    """
    Classify, then execute raw shell input directly (Section 8.3.5).

    Confirmation prompt (Section 8.3.5): a Destructive verdict shows a
    boxed rich.Panel (ui/panels.render_destructive_command_panel) with
    [y] Run anyway / [n] Cancel, and -- only when the classifier says
    `trash_alternative_possible` -- also [t] Move to trash instead, which
    moves the command's apparent target (see _extract_trash_target's own
    docstring for how that's determined) into .trash/ via trash.py instead
    of running the original destructive command at all.

    Hard constraint (Section 8.4, line 853): nothing here inspects the
    command text for --yes/-y and skips the prompt on a destructive
    verdict — that flag has no special handling anywhere in this function,
    which is what makes it unable to bypass this confirmation.

    Fail-safe policy (this function's own choice, not the classifier's):
    if the LLM fallback itself fails (Ollama unreachable, etc.), the
    command is treated as unclassifiable and NOT run automatically — the
    user is told why and asked to re-run manually, rather than silently
    executing something that couldn't be checked.

    Every outcome (run / cancelled / trashed) is recorded to the audit log
    (source="raw_shell") so /history, /log, and /stats see raw-shell
    activity too, not just natural-language actions -- there is no
    blueprint text specifically calling this out, but Section 4.2's Audit
    Log row ("প্রতিটা action ... রেকর্ড করে") does not scope itself to
    AI-originated actions only, so this module logs both paths uniformly.
    """
    active_console = console if console is not None else Console()
    try:
        result = classify(text, model=config_module.get(cfg, "model.active"))
    except DangerClassifierError as exc:
        active_console.print(
            f"  [yellow]⚠[/yellow] Could not classify this command ({exc}) — not running it automatically."
        )
        active_console.print(f"  [dim]Re-run manually if you're sure: {text}[/dim]")
        return

    if isinstance(result.verdict, Destructive):
        active_console.print(render_destructive_command_panel(result))
        choice = confirm("  > ").strip().lower()

        if choice == "t" and result.verdict.trash_alternative_possible:
            target = _extract_trash_target(text)
            if target is None:
                active_console.print("  Couldn't tell what to move to trash — cancelled. Nothing changed.")
                audit_log_module.record_action(
                    action=text, source="raw_shell", status="cancelled",
                    detail="trash target not determinable", base_dir=base_dir,
                )
                return
            try:
                trash_module.move_to_trash(target, base_dir=base_dir)
            except trash_module.TrashError as exc:
                active_console.print(f"  Could not move {target!r} to trash: {exc}")
                audit_log_module.record_action(
                    action=text, source="raw_shell", status="failed",
                    error=str(exc), detail=f"target={target}", base_dir=base_dir,
                )
                return
            active_console.print(f"  [green]✓[/green] Moved {target!r} to .trash/ instead of running the original command.")
            audit_log_module.record_action(
                action=text, source="raw_shell", status="done",
                detail=f"moved {target!r} to trash instead of running command", base_dir=base_dir,
            )
            return

        if choice != "y":
            active_console.print("  Cancelled.")
            audit_log_module.record_action(action=text, source="raw_shell", status="cancelled", base_dir=base_dir)
            return

    _run_shell_command(text)
    audit_log_module.record_action(action=text, source="raw_shell", status="done", base_dir=base_dir)


def _run_shell_command(text: str) -> None:
    """
    Actually execute a raw shell command. Uses shell=True so pipes/redirects/
    chaining the router already detected actually work; interactive
    full-screen programs (vim/top) are a pty hand-off concern for a later
    step — this is plain blocking subprocess execution.
    """
    try:
        subprocess.run(text, shell=True)
    except OSError as exc:
        print(f"error: {exc}", file=sys.stderr)


def _repl_get_user_choice(plan: Plan, *, read: callable = input, console: Console | None = None) -> str:
    """
    REPL-level [Enter] confirm / [e] edit / [c] chat / [Esc] cancel prompt
    (Section 8.3.3), mapped onto discussion.run_discussion's expected
    "confirm"/"edit"/"chat"/"cancel" return values.

    Design note (this file's own glue, Section 16 Rule 5): discussion.py's
    own docstring says the raw-keypress-to-choice mapping belongs to the
    REPL layer (Step 11/main.py), not to run_discussion itself -- this is
    that mapping, now rendering the plan via ui/panels.render_plan_panel
    (boxed) instead of discussion.render_plan_text (plain).
    """
    active_console = console if console is not None else Console()
    active_console.print(render_plan_panel(plan))
    raw = read("> ").strip().lower()
    if raw == "":
        return "confirm"
    if raw == "e":
        return "edit"
    if raw == "c":
        return "chat"
    if raw in ("esc", "q", "cancel"):
        return "cancel"
    return raw  # let run_discussion's own "unrecognized choice" branch handle it


def _repl_edit_flow(
    plan: Plan, registry: Registry, *, read: callable = input, console: Console | None = None
) -> Plan:
    """
    Drives the [e] direct-edit sub-flow: ask which param, ask its new
    value, apply via discussion.edit_step_param, return the updated plan.

    Design note: discussion.run_discussion's own "edit" branch just
    continues the loop and expects the caller to have already applied the
    edit before the next get_user_choice call (see its docstring) -- this
    function is that caller-side piece. If the param name isn't valid
    (KeyError from edit_step_param) or the plan has no params to edit at
    all, the plan is returned unchanged and a message is printed, rather
    than crashing the discussion loop over a typo.
    """
    active_console = console if console is not None else Console()
    if not plan.params:
        active_console.print("  This plan has no editable params.")
        return plan
    active_console.print(f"  [dim]Editable params:[/dim] {', '.join(plan.params.keys())}")
    param_name = read("  Edit which param? ").strip()
    if not param_name:
        active_console.print("  Cancelled edit — plan unchanged.")
        return plan
    new_value = read(f"  New value for {param_name!r}: ").strip()
    try:
        return edit_step_param(plan, param_name, new_value, registry)
    except KeyError:
        active_console.print(f"  {param_name!r} isn't a param on this plan — unchanged.")
        return plan


def _handle_natural_language(
    text: str,
    registry: Registry,
    cfg: dict,
    *,
    read: callable = input,
    console: Console | None = None,
    base_dir=None,
) -> None:
    """
    Full Section 4.1 pipeline: Intent Parser -> Plan Generator ->
    Confirmation + Discussion Loop -> (Sudo Layer, only if a step needs
    it) -> Streaming Executor -> Audit Log.

    Wiring decisions (Section 16 Rule 5 -- this glue is not itself spelled
    out anywhere in the retrieved blueprint text, only each module's own
    piece of it is; documented here rather than guessed at silently):

    1. Intent Parser failure (backend unreachable) and "unmapped" both end
       the request here with a message -- same behavior as the Step 5-era
       preview-only version, nothing to plan or execute yet. Both blocking
       parse_intent() calls below (the initial parse, and _reparse's
       chat-adjust re-parse) show a "Thinking..." spinner (rich's
       Console.status) while they run — a real turn against a local model
       can take several seconds, and a bare blocking call with no
       feedback read as the terminal having hung.
    2. Plan Generator runs once up front (Step 7), then the Discussion Loop
       (discussion.run_discussion) drives confirm/edit/chat/cancel using
       the REPL callbacks above. "chat" reparses via parse_intent + a
       fresh generate_plan (the `reparse` callback below) -- the adjustment
       text is sent to the Intent Parser as a fresh request rather than
       appended to the original text, since intent_parser.parse_intent's
       contract only takes one user_message string; a real "conversation
       history" concept doesn't exist in this module yet, so each chat-turn
       is its own independent parse. This is a simplification worth
       disclosing: a multi-turn *contextual* adjustment (e.g. "no wait,
       just the Downloads folder") only works as well as the Intent Parser
       can infer from that fragment alone.

       Known limitation, confirmed via manual end-to-end testing
       (post-Build-Order, local qwen3:8b / qwen3.5:4b): a chat-adjust whose
       intent is to change a capability's non-required, no-enum param (e.g.
       clean_temp_files' `paths`, an array with no fixed value set) is
       unreliable even after intent_parser._build_system_prompt was fixed
       to actually splice each capability's few_shot_examples into the
       prompt (that fix was itself a real bug -- the field existed in
       capabilities.json since Step 3 but was never read into the prompt
       the model saw). With the fix in place and a matching example added
       ("clean up my downloads folder instead"), standalone single-sentence
       adjustments still sometimes came back "unmapped", and in one
       observed case the model produced a schema-valid but semantically
       wrong result (organize_files with target_dir hallucinated as
       "~/.cache" -- a value copied from clean_temp_files' unrelated
       default, not from the user's actual words). This is model reasoning
       capacity, not a code defect: the schema-constrained decoding
       (Section 7.4a) and harness validation (Step 4) both did their job --
       the JSON was well-formed and passed its schema -- the *content* was
       just wrong, which those layers are not designed to catch (semantic
       correctness of free-form param values is fundamentally the model's
       job, not the harness's). No further attempt was made to prompt-
       engineer around this within this project's scope; a larger/different
       local model, or a future context-carrying reparse contract, are the
       two directions noted for anyone picking this up later.
    3. On Cancelled(): the request ends, logged as status="cancelled" (no
       execution happened).
    4. On Confirmed(plan): run_plan() executes it, streaming StepEvents
       through ui/streaming.StreamingRenderer (a rich.Live spinner that
       collapses to a summary line per step) instead of a flat print per
       event. run_plan() is given prompt=RichSudoPrompt(console=...) so
       any step that hits a permission-denied retry shows the Section
       8.3.6 sudo box as a boxed rich.Panel via sudo_layer's own decision
       contract.
    5. Exactly one audit_log.record_action() call per request outcome,
       mapping ExecutionResult onto the audit schema:
         - all_done                      -> status="done"
         - interrupted                   -> status="interrupted"
         - aborted_for_sudo              -> status="cancelled" (abort at the
           sudo prompt is the user declining the rest of the plan -- the
           closest existing status; "aborted" isn't its own enum value per
           audit_log.py's VALID_STATUSES, confirmed in Step 12)
         - a SKIPPED step with otherwise no failures -> status="skipped"
         - anything else (a FAILED step)  -> status="failed"
       `used_sudo` is True if any step result used it; `duration_seconds`
       is wall-clock time around run_plan(); `detail` carries the last
       step's stderr/detail for a failed/interrupted outcome so /explain
       and /log have something concrete to show.
    """
    active_console = console if console is not None else Console()
    model = config_module.get(cfg, "model.active")
    try:
        with active_console.status("[dim]Thinking...[/dim]", spinner="dots"):
            result = parse_intent(text, registry, model=model)
    except IntentParseError as exc:
        active_console.print(f"  [yellow]⚠[/yellow] Could not reach the model: {exc}")
        return

    if result.intent is None:
        active_console.print("  I couldn't map that to anything I can do yet. Try `/help`.")
        return

    plan = generate_plan(result.intent, registry)

    def _reparse(adjustment_text: str, current_plan: Plan) -> Plan | None:
        try:
            with active_console.status("[dim]Thinking...[/dim]", spinner="dots"):
                adjusted = parse_intent(adjustment_text, registry, model=model)
        except IntentParseError:
            return None
        if adjusted.intent is None:
            return None
        return generate_plan(adjusted.intent, registry)

    def _get_user_choice(current_plan: Plan) -> tuple[str, Plan]:
        choice = _repl_get_user_choice(current_plan, read=read, console=active_console)
        if choice == "edit":
            current_plan = _repl_edit_flow(current_plan, registry, read=read, console=active_console)
        return choice, current_plan

    def _discussion_print(line: str) -> None:
        # run_discussion's own print_fn calls (diff-notes, soft-limit nudge,
        # "couldn't apply that adjustment", "unrecognized choice") are short
        # single lines -- rendered dim rather than boxed, so they read as
        # secondary/system text next to the boxed plan panel above them.
        active_console.print(f"[dim]{line}[/dim]" if line.strip() else line)

    outcome = run_discussion(
        plan,
        get_user_choice=_get_user_choice,
        get_adjustment_text=lambda: read("  What would you like to change? "),
        reparse=_reparse,
        print_fn=_discussion_print,
    )

    if isinstance(outcome, Cancelled):
        active_console.print("  Cancelled.")
        audit_log_module.record_action(
            action=result.intent.action, source="natural_language", status="cancelled",
            params=result.intent.params, risk=result.intent.risk, base_dir=base_dir,
        )
        return

    assert isinstance(outcome, Confirmed)
    confirmed_plan = outcome.plan

    start = time.time()
    with StreamingRenderer(console=active_console) as renderer:
        execution = run_plan(
            confirmed_plan,
            registry,
            prompt=RichSudoPrompt(input_fn=read, console=active_console),
            on_event=renderer.on_event,
        )
    renderer.print_summary(execution)
    duration = time.time() - start

    used_sudo = any(r.used_sudo for r in execution.step_results)
    last_detail = execution.step_results[-1].stderr or execution.step_results[-1].description if execution.step_results else ""

    if execution.all_done:
        status = "done"
    elif execution.interrupted:
        status = "interrupted"
    elif execution.aborted_for_sudo:
        status = "cancelled"
    elif any(r.status is StepStatus.SKIPPED for r in execution.step_results):
        status = "skipped"
    else:
        status = "failed"

    audit_log_module.record_action(
        action=confirmed_plan.action,
        source="natural_language",
        params=confirmed_plan.params,
        risk=confirmed_plan.risk,
        status=status,
        duration_seconds=duration,
        used_sudo=used_sudo,
        detail=last_detail,
        base_dir=base_dir,
    )


def _handle_slash_command(
    text: str, registry: Registry, cfg: dict, session_start: float, *, console: Console | None = None
) -> bool:
    """
    Dispatch a slash command through the full Meta-Command Handler
    (meta_commands.py, Step 14). Returns True if the REPL should exit.

    A MetaCommandError (recognized command, bad usage, or genuinely
    unrecognized command) is caught and printed rather than crashing the
    REPL — the same fail-soft spirit used throughout this codebase (e.g.
    the Danger Classifier's own LLM-failure handling). meta_commands.py's
    own output stays plain text (it's a thin wrapper over already-built
    modules per its own docstring, out of this pass's scope) -- only the
    surrounding print call is routed through the shared Console so its
    output lands in the same stream/buffer as everything else this module
    renders (important for tests that capture one Console's buffer).
    """
    active_console = console if console is not None else Console()
    try:
        outcome = meta_commands.dispatch(text, cfg=cfg, registry=registry, session_start=session_start)
    except meta_commands.MetaCommandError as exc:
        active_console.print(f"  {exc}")
        return False

    if outcome.text:
        active_console.print(outcome.text, highlight=False)
    return outcome.should_exit


def run() -> None:
    """Entrypoint (see pyproject.toml's [project.scripts] and bin/oh-my-shell)."""
    console = Console()

    try:
        registry = load_registry()
    except RegistryError as exc:
        console.print(f"[red]Fatal: could not load capability registry: {exc}[/red]")
        sys.exit(1)

    cfg = config_module.load()
    session_start = time.time()
    session = ReplSession()

    console.print(_BANNER)

    while True:
        # A blank line before every prompt (Step 11 visual-polish pass,
        # found missing when the person compared a real session's output
        # against other CLIs' spacing) -- without it, one command's output
        # runs directly into the next "<folder> ❯ " line with no visual
        # separation, which is what made a real session read as "noisy,
        # non-aligned" by contrast. This alone, deliberately, rather than
        # bracketing every individual print call throughout this module
        # with blank lines -- one seam, in the one place every turn passes
        # through, is enough to separate turns without scattering spacing
        # concerns across every handler.
        console.print()
        try:
            raw_input_text = session.prompt(render_prompt_ansi(cfg))
        except (EOFError, KeyboardInterrupt):
            console.print()  # keep the terminal cursor on a clean line
            break

        routed = route(raw_input_text)

        if routed.kind == InputKind.EMPTY:
            continue
        if routed.kind == InputKind.SLASH_COMMAND:
            if _handle_slash_command(routed.text, registry, cfg, session_start, console=console):
                break
            continue
        if routed.kind == InputKind.RAW_SHELL:
            _handle_raw_shell(routed.text, cfg, confirm=session, console=console)
            continue
        if routed.kind == InputKind.NATURAL_LANGUAGE:
            _handle_natural_language(routed.text, registry, cfg, read=session, console=console)
            continue


if __name__ == "__main__":
    run()