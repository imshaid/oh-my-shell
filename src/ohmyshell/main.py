"""
Shell REPL loop, entrypoint (Build Order Step 6, danger-check integrated
Step 8, full Meta-Command Handler wired in Step 14, full Section 4.1
pipeline wired post-Build-Order).

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
  _handle_natural_language's own docstring for the exact wiring and the
  design decisions this required (none of the blueprint text retrieved so
  far spells out the REPL-level glue between these already-built modules
  in that much detail, so the glue itself -- not any individual module's
  behavior -- is this file's own design, documented inline).
- SLASH_COMMAND input goes through the full Meta-Command Handler
  (meta_commands.py, Step 14) — /help, /model, /history, /undo, /trash,
  /log, /capabilities, /explain, /stats, /system, /config, /clear, /exit,
  /quit are all recognized.
- Prompt rendering here is still plain text (folder name, model tag if
  non-default) — the `rich`-based visual polish from ui/prompt.py (Step 11)
  exists but isn't wired into this REPL's `input()` call, since `input()`
  only accepts a plain string prompt; adopting ui/prompt.py fully would
  need replacing `input()` with a `rich`/`prompt_toolkit`-driven read loop,
  a separate UI-framework change from wiring the pipeline's logic together.

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
from ohmyshell.sudo_layer import InputPrompt

EXIT_COMMANDS = {"/exit", "/quit"}


def _render_prompt(cfg: dict) -> str:
    """
    Plain-text prompt: current folder name only (not full path), plus a
    model tag if the active model isn't the default (Section 8.3.1).
    The icon-swap-on-AI-request behavior from 8.3.1 needs live in-place
    terminal redraw, which belongs to ui/streaming.py (Step 11) — this
    step always shows the raw-command icon "❯".
    """
    folder_name = Path.cwd().name or "/"
    active_model = config_module.get(cfg, "model.active")
    default_model = config_module.default_config()["model"]["active"]
    tag = f" ({active_model})" if active_model != default_model else ""
    return f"{folder_name}{tag} ❯ "


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


def _handle_raw_shell(text: str, cfg: dict, *, confirm: callable = input, base_dir=None) -> None:
    """
    Classify, then execute raw shell input directly (Section 8.3.5).

    Confirmation prompt (Section 8.3.5): a Destructive verdict shows
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
    try:
        result = classify(text, model=config_module.get(cfg, "model.active"))
    except DangerClassifierError as exc:
        print(f"  ⚠ Could not classify this command ({exc}) — not running it automatically.")
        print(f"  Re-run manually if you're sure: {text}")
        return

    if isinstance(result.verdict, Destructive):
        print("\n  ⚠ Potentially destructive command detected\n")
        print(f"  {result.verdict.explanation}\n")
        options = "  [y] Run anyway   [n] Cancel"
        if result.verdict.trash_alternative_possible:
            options += "   [t] Move to trash instead"
        print(options)
        choice = confirm("  > ").strip().lower()

        if choice == "t" and result.verdict.trash_alternative_possible:
            target = _extract_trash_target(text)
            if target is None:
                print("  Couldn't tell what to move to trash — cancelled. Nothing changed.")
                audit_log_module.record_action(
                    action=text, source="raw_shell", status="cancelled",
                    detail="trash target not determinable", base_dir=base_dir,
                )
                return
            try:
                trash_module.move_to_trash(target, base_dir=base_dir)
            except trash_module.TrashError as exc:
                print(f"  Could not move {target!r} to trash: {exc}")
                audit_log_module.record_action(
                    action=text, source="raw_shell", status="failed",
                    error=str(exc), detail=f"target={target}", base_dir=base_dir,
                )
                return
            print(f"  Moved {target!r} to .trash/ instead of running the original command.")
            audit_log_module.record_action(
                action=text, source="raw_shell", status="done",
                detail=f"moved {target!r} to trash instead of running command", base_dir=base_dir,
            )
            return

        if choice != "y":
            print("  Cancelled.")
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


def _repl_get_user_choice(plan: Plan, *, read: callable = input) -> str:
    """
    REPL-level [Enter] confirm / [e] edit / [c] chat / [Esc] cancel prompt
    (Section 8.3.3), mapped onto discussion.run_discussion's expected
    "confirm"/"edit"/"chat"/"cancel" return values.

    Design note (this file's own glue, Section 16 Rule 5): discussion.py's
    own docstring says the raw-keypress-to-choice mapping belongs to the
    REPL layer (Step 11/main.py), not to run_discussion itself -- this is
    that mapping. Plain input() line-reading, not a raw/cbreak single-key
    read (same "no true Esc key" limitation sudo_layer.InputPrompt already
    documents and works around the same way: a typed word instead).
    """
    print_fn = print
    print_fn("  [Enter] Confirm   [e] Edit   [c] Chat/adjust   [Esc/q] Cancel")
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


def _repl_edit_flow(plan: Plan, registry: Registry, *, read: callable = input, print_fn: callable = print) -> Plan:
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
    if not plan.params:
        print_fn("  This plan has no editable params.")
        return plan
    print_fn(f"  Editable params: {', '.join(plan.params.keys())}")
    param_name = read("  Edit which param? ").strip()
    if not param_name:
        print_fn("  Cancelled edit — plan unchanged.")
        return plan
    new_value = read(f"  New value for {param_name!r}: ").strip()
    try:
        return edit_step_param(plan, param_name, new_value, registry)
    except KeyError:
        print_fn(f"  {param_name!r} isn't a param on this plan — unchanged.")
        return plan


def _handle_natural_language(
    text: str,
    registry: Registry,
    cfg: dict,
    *,
    read: callable = input,
    print_fn: callable = print,
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
       preview-only version, nothing to plan or execute yet.
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
    3. On Cancelled(): the request ends, logged as status="cancelled" (no
       execution happened).
    4. On Confirmed(plan): run_plan() executes it, streaming StepEvents
       (printed plainly here -- ui/streaming.py's rich Live rendering is a
       separate UI-framework adoption, same scope line drawn for the prompt
       icon in this module's own docstring). run_plan() is given
       prompt=InputPrompt() so any step that hits a permission-denied retry
       shows the Section 8.3.6 sudo box via sudo_layer's own REPL prompt.
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
    model = config_module.get(cfg, "model.active")
    try:
        result = parse_intent(text, registry, model=model)
    except IntentParseError as exc:
        print_fn(f"  ⚠ Could not reach the model: {exc}")
        return

    if result.intent is None:
        print_fn("  I couldn't map that to anything I can do yet. Try `/help`.")
        return

    plan = generate_plan(result.intent, registry)

    def _reparse(adjustment_text: str, current_plan: Plan) -> Plan | None:
        try:
            adjusted = parse_intent(adjustment_text, registry, model=model)
        except IntentParseError:
            return None
        if adjusted.intent is None:
            return None
        return generate_plan(adjusted.intent, registry)

    def _get_user_choice(current_plan: Plan) -> tuple[str, Plan]:
        choice = _repl_get_user_choice(current_plan, read=read)
        if choice == "edit":
            current_plan = _repl_edit_flow(current_plan, registry, read=read, print_fn=print_fn)
        return choice, current_plan

    outcome = run_discussion(
        plan,
        get_user_choice=_get_user_choice,
        get_adjustment_text=lambda: read("  What would you like to change? "),
        reparse=_reparse,
        print_fn=print_fn,
    )

    if isinstance(outcome, Cancelled):
        print_fn("  Cancelled.")
        audit_log_module.record_action(
            action=result.intent.action, source="natural_language", status="cancelled",
            params=result.intent.params, risk=result.intent.risk, base_dir=base_dir,
        )
        return

    assert isinstance(outcome, Confirmed)
    confirmed_plan = outcome.plan

    def _on_event(event) -> None:
        label = event.status.name.lower()
        print_fn(f"  [{event.step_number}/{event.total_steps}] {label}: {event.detail}".rstrip(": "))

    start = time.time()
    execution = run_plan(confirmed_plan, registry, prompt=InputPrompt(), on_event=_on_event)
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

    if status == "done":
        print_fn("  Done.")
    elif status == "interrupted":
        print_fn("  Interrupted.")
    elif status == "cancelled":
        print_fn("  Aborted (elevated permission declined).")
    elif status == "skipped":
        print_fn("  Step skipped.")
    else:
        print_fn(f"  Failed: {last_detail}")


def _handle_slash_command(text: str, registry: Registry, cfg: dict, session_start: float) -> bool:
    """
    Dispatch a slash command through the full Meta-Command Handler
    (meta_commands.py, Step 14). Returns True if the REPL should exit.

    A MetaCommandError (recognized command, bad usage, or genuinely
    unrecognized command) is caught and printed rather than crashing the
    REPL — the same fail-soft spirit used throughout this codebase (e.g.
    the Danger Classifier's own LLM-failure handling).
    """
    try:
        outcome = meta_commands.dispatch(text, cfg=cfg, registry=registry, session_start=session_start)
    except meta_commands.MetaCommandError as exc:
        print(f"  {exc}")
        return False

    if outcome.text:
        print(outcome.text)
    return outcome.should_exit


def run() -> None:
    """Entrypoint (see pyproject.toml's [project.scripts] and bin/oh-my-shell)."""
    try:
        registry = load_registry()
    except RegistryError as exc:
        print(f"Fatal: could not load capability registry: {exc}", file=sys.stderr)
        sys.exit(1)

    cfg = config_module.load()
    session_start = time.time()

    while True:
        try:
            raw_input_text = input(_render_prompt(cfg))
        except (EOFError, KeyboardInterrupt):
            print()  # keep the terminal cursor on a clean line
            break

        routed = route(raw_input_text)

        if routed.kind == InputKind.EMPTY:
            continue
        if routed.kind == InputKind.SLASH_COMMAND:
            if _handle_slash_command(routed.text, registry, cfg, session_start):
                break
            continue
        if routed.kind == InputKind.RAW_SHELL:
            _handle_raw_shell(routed.text, cfg)
            continue
        if routed.kind == InputKind.NATURAL_LANGUAGE:
            _handle_natural_language(routed.text, registry, cfg)
            continue


if __name__ == "__main__":
    run()