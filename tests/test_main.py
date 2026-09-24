"""
Tests for main.py (Build Order Step 6, fully wired post-Build-Order to the
Section 4.1 pipeline, then to full `rich`/`prompt_toolkit` visual polish —
Step 11), rewritten for the open-ended architecture (see main.py's own
module docstring and intent_parser.py/validation.py/discussion.py's own
docstrings for the full rationale).

There is no more Capability Registry anywhere in main.py -- every function
signature drops the old `registry` argument. `_handle_natural_language`
now also runs `override_risk()` on the produced command before building
the plan; `_repl_edit_flow`/`edit_command` drive a raw command-text edit,
not a per-param edit.

`input()`/`ReplSession`, `subprocess.Popen` (raw-shell execution switched
from `subprocess.run` to a teed-stderr `Popen`, post-Build-Order -- see
main.py's `_run_shell_command` docstring), `intent_parser.parse_intent`,
and the Executor/Audit Log are all mocked or redirected to a tmp_path
base_dir so these tests exercise only main.py's own wiring logic, not real
shell execution, a real model call, or the user's real ~/.oh-my-shell/.

`_FakeRawPopen` (below) is the fake `subprocess.Popen` these raw-shell
tests inject in place of `ohmyshell.main.subprocess.Popen` -- it supports
just enough of the real interface (`.stderr.read(n)` chunked reads that
terminate with `b""`, `.wait()`, `.returncode`) for `_run_shell_command`'s
own tee-thread to run against it exactly as it would a real subprocess.

Rich-rendered output (panels, the prompt, streaming) is asserted against an
injected `Console(file=io.StringIO(), force_terminal=False)` buffer's
rendered text, the same pattern ui/panels.py's and ui/streaming.py's own
test suites already use.
"""

from __future__ import annotations

import io
import os
from unittest.mock import MagicMock, patch

import pytest
from rich.console import Console

from ohmyshell import config as config_module
from ohmyshell import main as main_module
from ohmyshell.discussion import Cancelled, Confirmed
from ohmyshell.executor import ExecutionResult, StepResult, StepStatus
from ohmyshell.intent_parser import IntentParseError, ParseResult
from ohmyshell.plan_generator import Plan
from ohmyshell.ui.panels import RichSudoPrompt
from ohmyshell.ui.prompt import render_prompt_ansi
from ohmyshell.validation import ValidatedIntent


@pytest.fixture
def default_cfg():
    return config_module.default_config()


@pytest.fixture(autouse=True)
def _no_real_fish_config_editing(monkeypatch):
    # `run()` now calls `ensure_fish_guard_installed()` on every startup
    # (see main.py's own docstring for why) -- it's a real-filesystem,
    # real-$SHELL-dependent side effect (edits the person's own
    # config.fish) that no test in this file should ever actually trigger,
    # regardless of what $SHELL happens to be set to on the machine
    # running the test suite. Tests that specifically want to exercise
    # `ensure_fish_guard_installed()` itself (see TestEnsureFishGuardInstalled)
    # undo this patch locally.
    monkeypatch.setattr(main_module, "ensure_fish_guard_installed", lambda: None)


def _buffer_console() -> tuple[io.StringIO, Console]:
    # Fixed accent palette (post-Build-Order): main.py's own chrome now
    # uses ui/theme.py's fixed "omsh.*" style names (see ui/theme.py's
    # module docstring), so any Console built for a test must carry
    # OMSH_THEME the same way main.py's real Console does (via
    # ui.theme.themed_console()), or rich raises MissingStyle trying to
    # resolve those names against a themeless Console.
    from ohmyshell.ui.theme import themed_console

    buffer = io.StringIO()
    console = themed_console(file=buffer, width=100, force_terminal=False)
    return buffer, console


class _FakeRawStderr:
    """`.read(n)`-chunked reader matching real `Popen.stderr`'s shape --
    returns each queued chunk once, then b"" forever (real pipe EOF)."""

    def __init__(self, chunks: list[bytes]) -> None:
        self._chunks = list(chunks)

    def read(self, _size: int = 4096) -> bytes:
        if self._chunks:
            return self._chunks.pop(0)
        return b""


class _FakeRawPopen:
    """Fake `subprocess.Popen` for `_run_shell_command`'s teed-stderr
    execution (see this file's module docstring) -- captures the command
    it was invoked with so tests can assert `shell=True` was used, and
    lets each test script an exit code + stderr chunks without spawning a
    real process."""

    last_instance: "_FakeRawPopen | None" = None

    def __init__(self, returncode: int = 0, stderr_text: str = "") -> None:
        self._returncode = returncode
        self._stderr_bytes = stderr_text.encode()
        self.stderr = _FakeRawStderr([self._stderr_bytes] if self._stderr_bytes else [])
        self.returncode: int | None = None
        self.command: str | None = None
        self.kwargs: dict | None = None

    def __call__(self, command, **kwargs):
        # Used as the `subprocess.Popen` replacement itself: each call
        # returns `self` (pre-configured), recording the args it was
        # actually invoked with for later assertion.
        self.command = command
        self.kwargs = kwargs
        _FakeRawPopen.last_instance = self
        return self

    def wait(self) -> int:
        self.returncode = self._returncode
        return self._returncode


# --- ui/prompt.render_prompt_ansi (main.py's prompt source) ------------------------


def test_render_prompt_ansi_shows_folder_name_only(default_cfg, tmp_path):
    rendered = render_prompt_ansi(default_cfg, cwd=tmp_path)
    assert tmp_path.name in rendered
    assert str(tmp_path) not in rendered  # full path must not appear, only the folder name


def test_render_prompt_ansi_contains_color_escapes(default_cfg, tmp_path):
    rendered = render_prompt_ansi(default_cfg, cwd=tmp_path)
    assert "\x1b[" in rendered


# --- main.py's own startup banner (_BANNER) ------------------------------------


def test_banner_headline_carries_a_real_color_escape():
    """
    Regression test (post-Build-Order, found via real-terminal testing --
    see ui/prompt.py's "bold omsh.path" fix for the full root-cause
    story): _BANNER's headline used to be built via
    `Text.from_markup("[bold omsh.accent]...[/bold omsh.accent]...")` --
    a composite markup tag mixing a plain attribute with a ui/theme.py
    "omsh.*" theme name, which rich silently renders completely unstyled
    (no color, no bold, no escape codes at all) instead of applying
    either attribute. Rendered through a themed, truecolor-forced Console
    (the same way main.py's real `run()` prints it), so a regression to a
    composite style/markup string for this banner fails here.
    """
    from ohmyshell.ui.theme import themed_console

    console = themed_console(force_terminal=True, color_system="truecolor", no_color=False)
    with console.capture() as capture:
        console.print(main_module._BANNER)
    rendered = capture.get()
    headline_index = rendered.index("Oh My Shell")
    assert "\x1b[" in rendered[:headline_index]


# --- _handle_raw_shell -------------------------------------------------------------


def test_handle_raw_shell_calls_subprocess_popen_with_shell_true_when_no_shell_env(
    default_cfg, tmp_path, monkeypatch
):
    """
    Fallback path (no usable $SHELL): still runs via `shell=True`, exactly
    as this function always did before `_raw_shell_popen_args` existed.
    """
    monkeypatch.delenv("SHELL", raising=False)
    with patch("ohmyshell.main.classify") as mock_classify:
        from ohmyshell.danger_classifier import ClassificationResult, Safe

        mock_classify.return_value = ClassificationResult(verdict=Safe(), source="regex")
        fake_popen = _FakeRawPopen(returncode=0)
        with patch("ohmyshell.main.subprocess.Popen", fake_popen):
            main_module._handle_raw_shell("ls -la", default_cfg, base_dir=tmp_path)
    assert fake_popen.command == "ls -la"
    assert fake_popen.kwargs["shell"] is True


def test_handle_raw_shell_prefers_real_login_shell_over_bin_sh(default_cfg, tmp_path, monkeypatch):
    """
    Bug fix (post-Build-Order, found via real-terminal testing): a raw
    command must run through the person's own real shell (from $SHELL) --
    e.g. fish -- when one is available, not Python's hardcoded /bin/sh,
    so that shell's own environment setup (LS_COLORS, aliases, etc.) is
    actually present for the command. Real argv, not a second shell=True
    string, so there's no risk of `text` being re-interpreted/quoted by
    an extra shell layer.
    """
    monkeypatch.setenv("SHELL", "/bin/bash")  # a real, always-present executable
    with patch("ohmyshell.main.classify") as mock_classify:
        from ohmyshell.danger_classifier import ClassificationResult, Safe

        mock_classify.return_value = ClassificationResult(verdict=Safe(), source="regex")
        fake_popen = _FakeRawPopen(returncode=0)
        with patch("ohmyshell.main.subprocess.Popen", fake_popen):
            main_module._handle_raw_shell("ls -la", default_cfg, base_dir=tmp_path)
    assert fake_popen.command == ["/bin/bash", "-c", "ls -la"]
    assert "shell" not in (fake_popen.kwargs or {})


def test_handle_raw_shell_falls_back_to_shell_true_when_shell_env_not_a_real_executable(
    default_cfg, tmp_path, monkeypatch
):
    monkeypatch.setenv("SHELL", "/not/a/real/shell/binary")
    with patch("ohmyshell.main.classify") as mock_classify:
        from ohmyshell.danger_classifier import ClassificationResult, Safe

        mock_classify.return_value = ClassificationResult(verdict=Safe(), source="regex")
        fake_popen = _FakeRawPopen(returncode=0)
        with patch("ohmyshell.main.subprocess.Popen", fake_popen):
            main_module._handle_raw_shell("ls -la", default_cfg, base_dir=tmp_path)
    assert fake_popen.command == "ls -la"
    assert fake_popen.kwargs["shell"] is True


def test_handle_raw_shell_passes_fish_guard_env_var_to_child(default_cfg, tmp_path, monkeypatch):
    """
    Fish-greeting-noise fix (post-Build-Order, found via real-terminal
    testing): every raw-command child process must see
    `OMSH_RAW_EXEC=1` in its own environment -- this is the signal
    `ensure_fish_guard_installed`'s injected config.fish guard checks for
    to skip the rest of the person's config (fastfetch etc.) for this
    non-interactive invocation only. The real `os.environ` itself must be
    left untouched (only the child's env dict gets the extra var).
    """
    monkeypatch.setenv("SHELL", "/bin/bash")
    with patch("ohmyshell.main.classify") as mock_classify:
        from ohmyshell.danger_classifier import ClassificationResult, Safe

        mock_classify.return_value = ClassificationResult(verdict=Safe(), source="regex")
        fake_popen = _FakeRawPopen(returncode=0)
        with patch("ohmyshell.main.subprocess.Popen", fake_popen):
            main_module._handle_raw_shell("ls -la", default_cfg, base_dir=tmp_path)
    assert fake_popen.kwargs["env"]["OMSH_RAW_EXEC"] == "1"
    assert "OMSH_RAW_EXEC" not in os.environ


def test_handle_raw_shell_reports_os_error_without_raising(default_cfg, tmp_path, capsys):
    with patch("ohmyshell.main.classify") as mock_classify:
        from ohmyshell.danger_classifier import ClassificationResult, Safe

        mock_classify.return_value = ClassificationResult(verdict=Safe(), source="regex")
        with patch("ohmyshell.main.subprocess.Popen", side_effect=OSError("boom")):
            main_module._handle_raw_shell("whatever", default_cfg, base_dir=tmp_path)  # must not raise
    captured = capsys.readouterr()
    assert "boom" in captured.err


def test_handle_raw_shell_safe_verdict_never_prompts(default_cfg, tmp_path):
    from ohmyshell.danger_classifier import ClassificationResult, Safe

    with patch("ohmyshell.main.classify", return_value=ClassificationResult(verdict=Safe(), source="regex")):
        with patch("ohmyshell.main.subprocess.Popen", _FakeRawPopen(returncode=0)):
            confirm_fn = MagicMock()
            main_module._handle_raw_shell("ls -la", default_cfg, confirm=confirm_fn, base_dir=tmp_path)
    confirm_fn.assert_not_called()


def test_handle_raw_shell_destructive_verdict_prompts_and_runs_on_yes(default_cfg, tmp_path, monkeypatch):
    from ohmyshell.danger_classifier import ClassificationResult, Destructive

    monkeypatch.delenv("SHELL", raising=False)
    destructive = ClassificationResult(
        verdict=Destructive(explanation="dangerous", trash_alternative_possible=True),
        source="regex",
    )
    fake_popen = _FakeRawPopen(returncode=0)
    with patch("ohmyshell.main.classify", return_value=destructive):
        with patch("ohmyshell.main.subprocess.Popen", fake_popen):
            main_module._handle_raw_shell("rm -rf /tmp/x", default_cfg, confirm=lambda _: "y", base_dir=tmp_path)
    assert fake_popen.command == "rm -rf /tmp/x"
    assert fake_popen.kwargs["shell"] is True


def test_handle_raw_shell_destructive_verdict_cancelled_on_no(default_cfg, tmp_path):
    from ohmyshell.danger_classifier import ClassificationResult, Destructive

    destructive = ClassificationResult(
        verdict=Destructive(explanation="dangerous", trash_alternative_possible=True),
        source="regex",
    )
    fake_popen = _FakeRawPopen(returncode=0)
    with patch("ohmyshell.main.classify", return_value=destructive):
        with patch("ohmyshell.main.subprocess.Popen", fake_popen):
            main_module._handle_raw_shell("rm -rf /tmp/x", default_cfg, confirm=lambda _: "n", base_dir=tmp_path)
    assert fake_popen.command is None  # never invoked


def test_handle_raw_shell_destructive_verdict_shows_explanation_in_panel(default_cfg, tmp_path):
    from ohmyshell.danger_classifier import ClassificationResult, Destructive

    destructive = ClassificationResult(
        verdict=Destructive(explanation="this will delete everything", trash_alternative_possible=True),
        source="regex",
    )
    buffer, console = _buffer_console()
    with patch("ohmyshell.main.classify", return_value=destructive):
        with patch("ohmyshell.main.subprocess.Popen", _FakeRawPopen(returncode=0)):
            main_module._handle_raw_shell(
                "rm -rf /tmp/x", default_cfg, confirm=lambda _: "n", console=console, base_dir=tmp_path
            )
    assert "this will delete everything" in buffer.getvalue()


def test_handle_raw_shell_classifier_error_does_not_run_command(default_cfg, tmp_path):
    from ohmyshell.danger_classifier import DangerClassifierError

    buffer, console = _buffer_console()
    fake_popen = _FakeRawPopen(returncode=0)
    with patch("ohmyshell.main.classify", side_effect=DangerClassifierError("backend unreachable")):
        with patch("ohmyshell.main.subprocess.Popen", fake_popen):
            main_module._handle_raw_shell("some ambiguous command", default_cfg, console=console, base_dir=tmp_path)
    assert fake_popen.command is None  # never invoked
    assert "backend unreachable" in buffer.getvalue()


def test_handle_raw_shell_destructive_verdict_offers_trash_option_when_possible(default_cfg, tmp_path):
    from ohmyshell.danger_classifier import ClassificationResult, Destructive

    destructive = ClassificationResult(
        verdict=Destructive(explanation="dangerous", trash_alternative_possible=True),
        source="regex",
    )
    buffer, console = _buffer_console()
    with patch("ohmyshell.main.classify", return_value=destructive):
        with patch("ohmyshell.main.subprocess.Popen", _FakeRawPopen(returncode=0)):
            main_module._handle_raw_shell(
                "rm -rf /tmp/x", default_cfg, confirm=lambda _: "n", console=console, base_dir=tmp_path
            )
    assert "Move to trash instead" in buffer.getvalue()


def test_handle_raw_shell_destructive_verdict_omits_trash_option_when_not_possible(default_cfg, tmp_path):
    from ohmyshell.danger_classifier import ClassificationResult, Destructive

    destructive = ClassificationResult(
        verdict=Destructive(explanation="wipes a device", trash_alternative_possible=False),
        source="regex",
    )
    buffer, console = _buffer_console()
    with patch("ohmyshell.main.classify", return_value=destructive):
        with patch("ohmyshell.main.subprocess.Popen", _FakeRawPopen(returncode=0)):
            main_module._handle_raw_shell(
                "dd if=/dev/zero of=/dev/sda", default_cfg, confirm=lambda _: "n", console=console, base_dir=tmp_path
            )
    assert "Move to trash" not in buffer.getvalue()


def test_handle_raw_shell_trash_choice_moves_target_instead_of_running(default_cfg, tmp_path):
    from ohmyshell.danger_classifier import ClassificationResult, Destructive

    target = tmp_path / "doomed.txt"
    target.write_text("x")
    destructive = ClassificationResult(
        verdict=Destructive(explanation="dangerous", trash_alternative_possible=True),
        source="regex",
    )
    fake_popen = _FakeRawPopen(returncode=0)
    with patch("ohmyshell.main.classify", return_value=destructive):
        with patch("ohmyshell.main.subprocess.Popen", fake_popen):
            main_module._handle_raw_shell(f"rm -rf {target}", default_cfg, confirm=lambda _: "t", base_dir=tmp_path)
    assert fake_popen.command is None  # never invoked
    assert not target.exists()

    from ohmyshell import trash as trash_module

    assert len(trash_module.list_trash(base_dir=tmp_path)) == 1


def test_handle_raw_shell_logs_audit_entry_on_run(default_cfg, tmp_path):
    from ohmyshell.danger_classifier import ClassificationResult, Safe

    with patch("ohmyshell.main.classify", return_value=ClassificationResult(verdict=Safe(), source="regex")):
        with patch("ohmyshell.main.subprocess.Popen", _FakeRawPopen(returncode=0)):
            main_module._handle_raw_shell("ls -la", default_cfg, base_dir=tmp_path)

    from ohmyshell import audit_log as audit_log_module

    entries = audit_log_module.read_entries(base_dir=tmp_path)
    assert len(entries) == 1
    assert entries[0].source == "raw_shell"
    assert entries[0].status == "done"


def test_handle_raw_shell_command_not_found_automatically_falls_back(default_cfg, tmp_path):
    """
    Revised (user-requested): a shell "command not found" signature in
    stderr -- e.g. a genuinely missing command -- must now silently and
    automatically reinterpret as natural language, with no [y/N] prompt
    and no confirm() call at all.
    """
    from ohmyshell.danger_classifier import ClassificationResult, Safe

    fake_popen = _FakeRawPopen(returncode=127, stderr_text="sh: 1: frefox: not found\n")
    buffer, console = _buffer_console()
    confirm_fn = MagicMock(return_value="n")
    with patch("ohmyshell.main.classify", return_value=ClassificationResult(verdict=Safe(), source="regex")):
        with patch("ohmyshell.main.subprocess.Popen", fake_popen):
            with patch("ohmyshell.main._handle_natural_language") as mock_nl:
                main_module._handle_raw_shell(
                    "frefox", default_cfg, confirm=confirm_fn, console=console, base_dir=tmp_path
                )
    # No confirmation question is printed anymore, and confirm() is never
    # called for this path (it's still used elsewhere in the function for
    # the Destructive-verdict [y/n/t] prompt, just not here).
    assert "natural language" not in buffer.getvalue().lower()
    confirm_fn.assert_not_called()
    mock_nl.assert_called_once()
    args, kwargs = mock_nl.call_args
    assert args[0] == "frefox"


def test_handle_raw_shell_misrouted_existing_command_automatically_falls_back(default_cfg, tmp_path):
    """
    The real bug this feature targets: "open firefox" misrouted as
    RAW_SHELL (router.py's own documented margin case -- `open` is a real
    PATH executable) doesn't fail with exit 127 at all, since `open`
    itself IS a real command -- it fails with ITS OWN ordinary
    argument-parsing error instead (xdg-open's "unexpected argument", or
    gio's "No such file or directory" when it treats "firefox" as a
    filename). Detection here must be stderr-text-based, not exit-code-127
    -only, to actually catch this real-world case -- and now it must fall
    back automatically, without ever calling confirm().
    """
    from ohmyshell.danger_classifier import ClassificationResult, Safe

    fake_popen = _FakeRawPopen(
        returncode=1, stderr_text="xdg-open: unexpected argument 'firefox'\n"
    )
    confirm_fn = MagicMock(return_value="n")
    with patch("ohmyshell.main.classify", return_value=ClassificationResult(verdict=Safe(), source="regex")):
        with patch("ohmyshell.main.subprocess.Popen", fake_popen):
            with patch("ohmyshell.main._handle_natural_language") as mock_nl:
                main_module._handle_raw_shell(
                    "open firefox", default_cfg, confirm=confirm_fn, base_dir=tmp_path
                )
    confirm_fn.assert_not_called()
    mock_nl.assert_called_once()
    args, kwargs = mock_nl.call_args
    assert args[0] == "open firefox"


def test_handle_raw_shell_command_not_understood_logs_cancelled_before_nl_takes_over(default_cfg, tmp_path):
    """
    Before handing off to _handle_natural_language (which logs its own
    outcome under source="natural_language"), this function still records
    its own audit-log entry marking the raw-shell attempt as cancelled/
    reinterpreted -- unchanged from the old confirmed-path behavior,
    just reached unconditionally now instead of behind a "y" answer.
    """
    from ohmyshell.danger_classifier import ClassificationResult, Safe

    fake_popen = _FakeRawPopen(returncode=127, stderr_text="sh: 1: frefox: not found\n")
    with patch("ohmyshell.main.classify", return_value=ClassificationResult(verdict=Safe(), source="regex")):
        with patch("ohmyshell.main.subprocess.Popen", fake_popen):
            with patch("ohmyshell.main._handle_natural_language") as mock_nl:
                main_module._handle_raw_shell("frefox", default_cfg, base_dir=tmp_path)
    mock_nl.assert_called_once()
    args, kwargs = mock_nl.call_args
    assert args[0] == "frefox"

    from ohmyshell import audit_log as audit_log_module

    entries = audit_log_module.read_entries(base_dir=tmp_path)
    assert len(entries) == 1
    assert entries[0].status == "cancelled"
    assert entries[0].source == "raw_shell"


def test_handle_raw_shell_ordinary_nonzero_exit_never_falls_back(default_cfg, tmp_path):
    """
    A generic non-zero exit with no "command not understood" style stderr
    -- e.g. `grep` finding nothing, or `rmdir` on a non-empty directory --
    is a completely ordinary, correctly classified raw-shell failure and
    must never trigger the AI fallback.
    """
    from ohmyshell.danger_classifier import ClassificationResult, Safe

    fake_popen = _FakeRawPopen(returncode=1, stderr_text="")
    confirm_fn = MagicMock(return_value="n")
    buffer, console = _buffer_console()
    with patch("ohmyshell.main.classify", return_value=ClassificationResult(verdict=Safe(), source="regex")):
        with patch("ohmyshell.main.subprocess.Popen", fake_popen):
            with patch("ohmyshell.main._handle_natural_language") as mock_nl:
                main_module._handle_raw_shell(
                    "grep foo bar.txt", default_cfg, confirm=confirm_fn, console=console, base_dir=tmp_path
                )
    confirm_fn.assert_not_called()
    mock_nl.assert_not_called()
    assert "natural language" not in buffer.getvalue().lower()

    from ohmyshell import audit_log as audit_log_module

    entries = audit_log_module.read_entries(base_dir=tmp_path)
    assert len(entries) == 1
    assert entries[0].status == "done"
    assert entries[0].source == "raw_shell"


def test_handle_raw_shell_tees_stderr_to_real_stderr(default_cfg, tmp_path, capsys):
    """
    The captured-for-signature-matching stderr must still reach the real
    terminal live -- this function must not swallow error output the user
    would otherwise see.

    Regression note: the stderr text here must NOT match any
    `_COMMAND_NOT_UNDERSTOOD_SIGNATURES` phrase (e.g. "No such file or
    directory" would), since with the automatic-fallback behavior a
    matching signature now hands off straight to
    `_handle_natural_language` -- unrelated to what this test actually
    checks (that stderr is teed live), and that path isn't mocked out
    here. "Permission denied" is an ordinary, unrelated raw-shell failure
    that keeps this test on its own concern.
    """
    from ohmyshell.danger_classifier import ClassificationResult, Safe

    fake_popen = _FakeRawPopen(returncode=1, stderr_text="chmod: changing permissions of 'nope': Operation not permitted\n")
    with patch("ohmyshell.main.classify", return_value=ClassificationResult(verdict=Safe(), source="regex")):
        with patch("ohmyshell.main.subprocess.Popen", fake_popen):
            main_module._handle_raw_shell("chmod 700 nope", default_cfg, confirm=lambda _: "n", base_dir=tmp_path)
    captured = capsys.readouterr()
    assert "changing permissions of 'nope'" in captured.err


# --- _handle_natural_language --------------------------------------------------------


def _fake_parse_result(command="ps aux", risk="low", explanation="List processes.", attempts=1):
    return ParseResult(
        action=command,
        intent=ValidatedIntent(command=command, risk=risk, explanation=explanation),
        attempts=attempts,
    )


def _fake_plan(command="ps aux", risk="low", explanation="List processes.", steps=None):
    return Plan(command=command, risk=risk, explanation=explanation, steps=steps or [explanation or command])


def test_handle_natural_language_reports_unmapped(default_cfg, tmp_path):
    fake_result = ParseResult(action="unmapped", intent=None, attempts=2, last_error="nope")
    buffer, console = _buffer_console()
    with patch("ohmyshell.main.parse_intent", return_value=fake_result):
        main_module._handle_natural_language(
            "do something weird", default_cfg, console=console, base_dir=tmp_path
        )
    assert "couldn't turn that into a command" in buffer.getvalue().lower()


def test_handle_natural_language_reports_backend_failure(default_cfg, tmp_path):
    buffer, console = _buffer_console()
    with patch("ohmyshell.main.parse_intent", side_effect=IntentParseError("backend unreachable")):
        main_module._handle_natural_language(
            "clean up temp files", default_cfg, console=console, base_dir=tmp_path
        )
    assert "backend unreachable" in buffer.getvalue()


def test_handle_natural_language_cancel_never_executes_anything(default_cfg, tmp_path):
    """Cancelling at the Discussion Loop must not touch subprocess/run_plan at all."""
    fake_result = _fake_parse_result(command="find /tmp -mtime +7 -delete", risk="medium")

    with patch("ohmyshell.main.parse_intent", return_value=fake_result):
        with patch("ohmyshell.main.run_discussion", return_value=Cancelled()):
            with patch("ohmyshell.main.run_plan") as mock_run_plan:
                main_module._handle_natural_language(
                    "clean up temp files", default_cfg, base_dir=tmp_path
                )
    mock_run_plan.assert_not_called()


def test_handle_natural_language_cancel_logs_cancelled_status(default_cfg, tmp_path):
    fake_result = _fake_parse_result(command="find /tmp -mtime +7 -delete", risk="medium")

    with patch("ohmyshell.main.parse_intent", return_value=fake_result):
        with patch("ohmyshell.main.run_discussion", return_value=Cancelled()):
            main_module._handle_natural_language("clean up temp files", default_cfg, base_dir=tmp_path)

    from ohmyshell import audit_log as audit_log_module

    entries = audit_log_module.read_entries(base_dir=tmp_path)
    assert len(entries) == 1
    assert entries[0].status == "cancelled"
    assert entries[0].source == "natural_language"


def test_handle_natural_language_confirmed_runs_plan_and_logs_done(default_cfg, tmp_path):
    fake_result = _fake_parse_result(command="ps aux | grep chrome", risk="low")
    execution = ExecutionResult(
        action="ps aux | grep chrome",
        step_results=[
            StepResult(step_number=1, description="list", status=StepStatus.DONE, returncode=0, stdout="", stderr="")
        ],
    )

    def _fake_run_discussion(plan, **kwargs):
        return Confirmed(plan=plan)

    with patch("ohmyshell.main.parse_intent", return_value=fake_result):
        with patch("ohmyshell.main.run_discussion", side_effect=_fake_run_discussion):
            with patch("ohmyshell.main.run_plan", return_value=execution) as mock_run_plan:
                main_module._handle_natural_language(
                    "find chrome processes", default_cfg, base_dir=tmp_path
                )

    mock_run_plan.assert_called_once()
    from ohmyshell import audit_log as audit_log_module

    entries = audit_log_module.read_entries(base_dir=tmp_path)
    assert len(entries) == 1
    assert entries[0].status == "done"
    assert entries[0].action == "ps aux | grep chrome"
    assert entries[0].risk == "low"


def test_handle_natural_language_confirmed_never_calls_subprocess_directly(default_cfg, tmp_path):
    """main.py itself must not shell out for NL input -- only run_plan() may."""
    fake_result = _fake_parse_result(command="find /tmp -mtime +7 -delete", risk="medium")
    execution = ExecutionResult(
        action="find /tmp -mtime +7 -delete",
        step_results=[
            StepResult(step_number=1, description="clean", status=StepStatus.DONE, returncode=0, stdout="", stderr="")
        ],
    )

    def _fake_run_discussion(plan, **kwargs):
        return Confirmed(plan=plan)

    with patch("ohmyshell.main.parse_intent", return_value=fake_result):
        with patch("ohmyshell.main.run_discussion", side_effect=_fake_run_discussion):
            with patch("ohmyshell.main.run_plan", return_value=execution):
                with patch("ohmyshell.main.subprocess.run") as mock_run:
                    main_module._handle_natural_language(
                        "clean up temp files", default_cfg, base_dir=tmp_path
                    )
    mock_run.assert_not_called()


def test_handle_natural_language_interrupted_execution_logs_interrupted(default_cfg, tmp_path):
    fake_result = _fake_parse_result(command="find /tmp -mtime +7 -delete", risk="medium")
    execution = ExecutionResult(
        action="find /tmp -mtime +7 -delete",
        step_results=[
            StepResult(
                step_number=1, description="clean", status=StepStatus.INTERRUPTED,
                returncode=None, stdout="", stderr="",
            )
        ],
        interrupted=True,
    )

    def _fake_run_discussion(plan, **kwargs):
        return Confirmed(plan=plan)

    with patch("ohmyshell.main.parse_intent", return_value=fake_result):
        with patch("ohmyshell.main.run_discussion", side_effect=_fake_run_discussion):
            with patch("ohmyshell.main.run_plan", return_value=execution):
                main_module._handle_natural_language(
                    "clean up temp files", default_cfg, base_dir=tmp_path
                )

    from ohmyshell import audit_log as audit_log_module

    entries = audit_log_module.read_entries(base_dir=tmp_path)
    assert entries[0].status == "interrupted"


def test_handle_natural_language_aborted_for_sudo_logs_cancelled(default_cfg, tmp_path):
    fake_result = _fake_parse_result(command="find /tmp -mtime +7 -delete", risk="medium")
    execution = ExecutionResult(
        action="find /tmp -mtime +7 -delete",
        step_results=[
            StepResult(step_number=1, description="clean", status=StepStatus.INTERRUPTED, returncode=None)
        ],
        aborted_for_sudo=True,
    )

    def _fake_run_discussion(plan, **kwargs):
        return Confirmed(plan=plan)

    with patch("ohmyshell.main.parse_intent", return_value=fake_result):
        with patch("ohmyshell.main.run_discussion", side_effect=_fake_run_discussion):
            with patch("ohmyshell.main.run_plan", return_value=execution):
                main_module._handle_natural_language(
                    "clean up temp files", default_cfg, base_dir=tmp_path
                )

    from ohmyshell import audit_log as audit_log_module

    entries = audit_log_module.read_entries(base_dir=tmp_path)
    assert entries[0].status == "cancelled"


def test_handle_natural_language_failed_step_logs_failed(default_cfg, tmp_path):
    fake_result = _fake_parse_result(command="find /tmp -mtime +7 -delete", risk="medium")
    execution = ExecutionResult(
        action="find /tmp -mtime +7 -delete",
        step_results=[
            StepResult(
                step_number=1, description="clean", status=StepStatus.FAILED,
                returncode=1, stdout="", stderr="boom",
            )
        ],
    )

    def _fake_run_discussion(plan, **kwargs):
        return Confirmed(plan=plan)

    with patch("ohmyshell.main.parse_intent", return_value=fake_result):
        with patch("ohmyshell.main.run_discussion", side_effect=_fake_run_discussion):
            with patch("ohmyshell.main.run_plan", return_value=execution):
                main_module._handle_natural_language(
                    "clean up temp files", default_cfg, base_dir=tmp_path
                )

    from ohmyshell import audit_log as audit_log_module

    entries = audit_log_module.read_entries(base_dir=tmp_path)
    assert entries[0].status == "failed"
    assert "boom" in entries[0].detail


def test_handle_natural_language_passes_rich_sudo_prompt_to_run_plan(default_cfg, tmp_path):
    """A step needing sudo must be able to reach the Step 11 boxed sudo prompt."""
    fake_result = _fake_parse_result(command="find /tmp -mtime +7 -delete", risk="medium")
    execution = ExecutionResult(
        action="find /tmp -mtime +7 -delete",
        step_results=[StepResult(step_number=1, description="clean", status=StepStatus.DONE, returncode=0)],
    )

    def _fake_run_discussion(plan, **kwargs):
        return Confirmed(plan=plan)

    with patch("ohmyshell.main.parse_intent", return_value=fake_result):
        with patch("ohmyshell.main.run_discussion", side_effect=_fake_run_discussion):
            with patch("ohmyshell.main.run_plan", return_value=execution) as mock_run_plan:
                main_module._handle_natural_language(
                    "clean up temp files", default_cfg, base_dir=tmp_path
                )

    _, kwargs = mock_run_plan.call_args
    assert isinstance(kwargs["prompt"], RichSudoPrompt)


# --- Independent risk override integration (override_risk) -----------------------


def test_handle_natural_language_applies_independent_risk_override(default_cfg, tmp_path):
    """
    This session's own testing found models under-risking port-opening and
    passwordless-user-creation commands (see danger_classifier.py's own
    module docstring) -- _handle_natural_language must call override_risk()
    on the produced command before the plan ever reaches the user, and take
    the more severe verdict.
    """
    # A command matching override_risk's port-opening pattern, with the
    # model itself claiming "low" -- override_risk must force this to "high".
    fake_result = _fake_parse_result(command="ufw allow 22", risk="low", explanation="Open port 22.")

    captured_plans = []

    def _fake_run_discussion(plan, **kwargs):
        captured_plans.append(plan)
        return Cancelled()

    with patch("ohmyshell.main.parse_intent", return_value=fake_result):
        with patch("ohmyshell.main.run_discussion", side_effect=_fake_run_discussion):
            main_module._handle_natural_language("open port 22", default_cfg, base_dir=tmp_path)

    assert len(captured_plans) == 1
    assert captured_plans[0].risk == "high"  # overridden, not the model's own "low"


def test_handle_natural_language_does_not_override_when_model_risk_already_adequate(default_cfg, tmp_path):
    """override_risk() never LOWERS a risk either -- a benign, correctly
    low-risk command's plan must still show "low"."""
    fake_result = _fake_parse_result(command="ls -la", risk="low", explanation="List files.")

    captured_plans = []

    def _fake_run_discussion(plan, **kwargs):
        captured_plans.append(plan)
        return Cancelled()

    with patch("ohmyshell.main.parse_intent", return_value=fake_result):
        with patch("ohmyshell.main.run_discussion", side_effect=_fake_run_discussion):
            main_module._handle_natural_language("list files", default_cfg, base_dir=tmp_path)

    assert captured_plans[0].risk == "low"


def test_handle_natural_language_override_applies_on_chat_reparse_too(default_cfg, tmp_path):
    """
    The independent risk override must be re-applied on every chat-adjust
    reparse too, not just the initial parse -- otherwise a chat-adjust
    could "launder" an under-risked command past the override by simply
    triggering a second parse_intent() call. Drives the real
    discussion.run_discussion loop (not mocked) through
    _handle_natural_language's own choice_read/read plumbing, exactly like
    a real terminal session would: [c] chat -> adjustment text -> [Enter]
    confirm.
    """
    initial_result = _fake_parse_result(command="ls -la", risk="low", explanation="List files.")
    adjusted_result = _fake_parse_result(command="ufw allow 22", risk="low", explanation="Open port 22.")

    parse_calls = {"n": 0}

    def _fake_parse_intent(text, **kwargs):
        parse_calls["n"] += 1
        return initial_result if parse_calls["n"] == 1 else adjusted_result

    execution = ExecutionResult(
        action="ufw allow 22",
        step_results=[StepResult(step_number=1, description="x", status=StepStatus.DONE, returncode=0)],
    )

    choice_responses = iter(["c", ""])  # [c] chat-adjust, then bare Enter to confirm

    with patch("ohmyshell.main.parse_intent", side_effect=_fake_parse_intent):
        with patch("ohmyshell.main.run_plan", return_value=execution):
            main_module._handle_natural_language(
                "list files",
                default_cfg,
                read=lambda _: "open port 22 instead",
                choice_read=lambda _: next(choice_responses),
                base_dir=tmp_path,
            )

    assert parse_calls["n"] == 2  # initial parse + one chat-adjust reparse

    from ohmyshell import audit_log as audit_log_module

    entries = audit_log_module.read_entries(base_dir=tmp_path)
    assert len(entries) == 1
    # The FINAL plan (after the chat-adjust reparse) must reflect the
    # override, even though its own risk came back "low" from the model.
    assert entries[0].risk == "high"
    assert entries[0].action == "ufw allow 22"


# --- _repl_get_user_choice / _repl_edit_flow ------------------------------------------


def test_repl_get_user_choice_bare_enter_confirms():
    plan = _fake_plan()
    choice = main_module._repl_get_user_choice(plan, read=lambda _: "")
    assert choice == "confirm"


def test_repl_get_user_choice_maps_keys():
    plan = _fake_plan()
    assert main_module._repl_get_user_choice(plan, read=lambda _: "e") == "edit"
    assert main_module._repl_get_user_choice(plan, read=lambda _: "c") == "chat"
    assert main_module._repl_get_user_choice(plan, read=lambda _: "q") == "cancel"


def test_repl_get_user_choice_renders_plan_panel():
    plan = _fake_plan(command="find /tmp -mtime +7 -delete", risk="medium", explanation="Scan for old files")
    buffer, console = _buffer_console()
    main_module._repl_get_user_choice(plan, read=lambda _: "", console=console)
    assert "Scan for old files" in buffer.getvalue()
    assert "Confirm" in buffer.getvalue()


def test_repl_edit_flow_applies_edit():
    plan = _fake_plan(command="find /tmp -mtime +7 -delete", risk="medium")
    updated = main_module._repl_edit_flow(plan, read=lambda _: "find /tmp -mtime +30 -delete")
    assert updated.command == "find /tmp -mtime +30 -delete"


def test_repl_edit_flow_blank_input_leaves_plan_unchanged():
    plan = _fake_plan(command="find /tmp -mtime +7 -delete", risk="medium")
    updated = main_module._repl_edit_flow(plan, read=lambda _: "")
    assert updated == plan


# --- _extract_trash_target --------------------------------------------------------


def test_extract_trash_target_picks_trailing_path():
    assert main_module._extract_trash_target("rm -rf /tmp/build") == "/tmp/build"


def test_extract_trash_target_skips_flags():
    assert main_module._extract_trash_target("rm -rf -v /tmp/build") == "/tmp/build"


def test_extract_trash_target_none_for_empty_text():
    assert main_module._extract_trash_target("") is None


def test_extract_trash_target_none_when_only_flags_after_command():
    assert main_module._extract_trash_target("rm -rf") is None


# --- _handle_slash_command --------------------------------------------------------


def test_slash_exit_returns_true(default_cfg):
    assert main_module._handle_slash_command("/exit", default_cfg, 0.0) is True


def test_slash_quit_returns_true(default_cfg):
    assert main_module._handle_slash_command("/quit", default_cfg, 0.0) is True


def test_slash_help_is_fully_handled_by_meta_commands(default_cfg):
    buffer, console = _buffer_console()
    result = main_module._handle_slash_command("/help", default_cfg, 0.0, console=console)
    assert result is False
    assert "Command Reference" in buffer.getvalue()


def test_unrecognized_slash_command_prints_error_and_does_not_exit(default_cfg):
    buffer, console = _buffer_console()
    result = main_module._handle_slash_command("/totally-bogus", default_cfg, 0.0, console=console)
    assert result is False
    assert "Unrecognized command" in buffer.getvalue()


# --- run(): REPL loop integration ---------------------------------------------------


class _FakeSession:
    """Stands in for ui.session.ReplSession in run()'s REPL loop."""

    def __init__(self, responses):
        self._responses = iter(responses)

    def prompt(self, *_args, **_kwargs):
        try:
            return next(self._responses)
        except StopIteration:
            raise EOFError from None

    def __call__(self, *args, **kwargs):
        return self.prompt(*args, **kwargs)


def test_run_exits_cleanly_on_slash_exit():
    with patch("ohmyshell.main.ReplSession", return_value=_FakeSession(["/exit"])):
        with patch("ohmyshell.main.wizard_module.should_run_wizard", return_value=False):
            main_module.run()  # must return without raising


def test_run_exits_on_eof():
    with patch("ohmyshell.main.ReplSession", return_value=_FakeSession([])):
        with patch("ohmyshell.main.wizard_module.should_run_wizard", return_value=False):
            main_module.run()


def test_run_dispatches_raw_shell_then_exits(tmp_path, monkeypatch):
    monkeypatch.setattr(config_module, "CONFIG_DIR", tmp_path)
    monkeypatch.delenv("SHELL", raising=False)
    fake_popen = _FakeRawPopen(returncode=0)
    with patch("ohmyshell.main.ReplSession", return_value=_FakeSession(["ls -la", "/exit"])):
        with patch("ohmyshell.main.wizard_module.should_run_wizard", return_value=False):
            with patch("ohmyshell.main.subprocess.Popen", fake_popen):
                main_module.run()
    assert fake_popen.command == "ls -la"
    assert fake_popen.kwargs["shell"] is True


def test_run_invokes_wizard_on_genuine_first_run(tmp_path, monkeypatch):
    monkeypatch.setattr(config_module, "CONFIG_DIR", tmp_path)
    monkeypatch.setattr(config_module, "CONFIG_PATH", tmp_path / "config.json")
    assert not (tmp_path / "config.json").exists()

    with patch("ohmyshell.main.ReplSession", return_value=_FakeSession(["/exit"])):
        with patch("ohmyshell.main.wizard_module.run_wizard") as mock_wizard:
            main_module.run()

    mock_wizard.assert_called_once()


def test_run_skips_wizard_when_config_already_exists(tmp_path, monkeypatch):
    monkeypatch.setattr(config_module, "CONFIG_DIR", tmp_path)
    config_path = tmp_path / "config.json"
    monkeypatch.setattr(config_module, "CONFIG_PATH", config_path)
    config_path.write_text("{}", encoding="utf-8")

    with patch("ohmyshell.main.ReplSession", return_value=_FakeSession(["/exit"])):
        with patch("ohmyshell.main.wizard_module.run_wizard") as mock_wizard:
            main_module.run()

    mock_wizard.assert_not_called()


def test_run_survives_ctrl_c_at_a_mid_command_confirmation_prompt(tmp_path, monkeypatch):
    monkeypatch.setattr(config_module, "CONFIG_DIR", tmp_path)

    class _RaisesOnConfirm:
        def __init__(self, repl_inputs):
            self._repl_inputs = iter(repl_inputs)

        def prompt(self, *_args, **_kwargs):
            try:
                return next(self._repl_inputs)
            except StopIteration:
                raise EOFError from None

        def __call__(self, *_args, **_kwargs):
            raise KeyboardInterrupt

    fake_session = _RaisesOnConfirm(["rm -rf /tmp/somedir", "/exit"])

    with patch("ohmyshell.main.ReplSession", return_value=fake_session):
        with patch("ohmyshell.main.wizard_module.should_run_wizard", return_value=False):
            with patch("ohmyshell.main.subprocess.Popen") as mock_popen:
                main_module.run()  # must return normally, not raise

    mock_popen.assert_not_called()


class TestEnsureFishGuardInstalled:
    """
    `ensure_fish_guard_installed` -- the fish-greeting-noise fix (see
    main.py's own module-level comment above the function for the full
    story). These tests undo the file's own autouse no-op patch so they
    can exercise the real function against a fake `$HOME`/config.fish.
    """

    def test_does_nothing_when_shell_is_not_fish(self, monkeypatch, tmp_path):
        monkeypatch.setenv("SHELL", "/bin/bash")
        monkeypatch.setenv("HOME", str(tmp_path))
        monkeypatch.delenv("XDG_CONFIG_HOME", raising=False)
        config_path = tmp_path / ".config" / "fish" / "config.fish"
        config_path.parent.mkdir(parents=True)
        config_path.write_text("fastfetch\n")

        _real_ensure_fish_guard_installed()

        assert config_path.read_text() == "fastfetch\n"

    def test_does_nothing_when_config_fish_does_not_exist(self, monkeypatch, tmp_path):
        monkeypatch.setenv("SHELL", "/usr/bin/fish")
        monkeypatch.setenv("HOME", str(tmp_path))
        monkeypatch.delenv("XDG_CONFIG_HOME", raising=False)
        # No .config/fish/config.fish at all.

        _real_ensure_fish_guard_installed()  # must not raise, must not create one

        assert not (tmp_path / ".config" / "fish" / "config.fish").exists()

    def test_wraps_a_bare_unguarded_fastfetch_line_in_place(self, monkeypatch, tmp_path):
        """
        Third bug fix, on top of two earlier broken attempts (see main.py's
        own module-level comment above `_FISH_GUARD_ENV` for the full
        story): the actual fix wraps the noisy command's OWN line directly
        -- `if not set -q OMSH_RAW_EXEC / <line> / end` -- in place, rather
        than trying to prepend a block that intercepts `status
        is-interactive` (impossible -- `status` is a fish reserved keyword,
        confirmed by a real `function: status: cannot use reserved keyword
        as function name` error) or `exit`ing the whole file (which broke
        every raw command's LS_COLORS/alias setup along with the banner it
        was meant to silence).
        """
        monkeypatch.setenv("SHELL", "/usr/bin/fish")
        monkeypatch.setenv("HOME", str(tmp_path))
        monkeypatch.delenv("XDG_CONFIG_HOME", raising=False)
        config_path = tmp_path / ".config" / "fish" / "config.fish"
        config_path.parent.mkdir(parents=True)
        config_path.write_text(
            "starship init fish | source\n"
            "fastfetch\n"
            'alias ls="eza --icons --group-directories-first"\n'
            "zoxide init fish | source\n"
        )

        _real_ensure_fish_guard_installed()

        new_content = config_path.read_text()
        assert "    exit\n" not in new_content
        assert "function status" not in new_content
        assert (
            "if not set -q OMSH_RAW_EXEC\n"
            "    fastfetch\n"
            "end"
        ) in new_content
        # Every other line must survive completely untouched.
        assert "starship init fish | source" in new_content
        assert 'alias ls="eza --icons --group-directories-first"' in new_content
        assert "zoxide init fish | source" in new_content

    def test_does_not_wrap_a_fastfetch_call_that_is_already_guarded(self, monkeypatch, tmp_path):
        monkeypatch.setenv("SHELL", "/usr/bin/fish")
        monkeypatch.setenv("HOME", str(tmp_path))
        monkeypatch.delenv("XDG_CONFIG_HOME", raising=False)
        config_path = tmp_path / ".config" / "fish" / "config.fish"
        config_path.parent.mkdir(parents=True)
        config_path.write_text("if status is-interactive\n    fastfetch\nend\n")

        _real_ensure_fish_guard_installed()

        # Already-interactive-guarded -- nothing here needed wrapping, so
        # this run is a no-op (no OMSH_RAW_EXEC guard anywhere at all).
        new_content = config_path.read_text()
        assert "OMSH_RAW_EXEC" not in new_content
        assert new_content == "if status is-interactive\n    fastfetch\nend\n"

    def test_migrates_an_already_installed_old_exit_based_guard(self, monkeypatch, tmp_path):
        """
        Anyone who ran the very first version of this fix already has the
        old, `exit`-based guard installed in their real config.fish. A
        naive "marker already present -> skip" check would leave that
        broken block in place forever. This must detect and strip it
        entirely before applying the new, working per-line wrap.
        """
        monkeypatch.setenv("SHELL", "/usr/bin/fish")
        monkeypatch.setenv("HOME", str(tmp_path))
        monkeypatch.delenv("XDG_CONFIG_HOME", raising=False)
        config_path = tmp_path / ".config" / "fish" / "config.fish"
        config_path.parent.mkdir(parents=True)
        old_guard = (
            "# oh-my-shell: skip rest of config.fish for non-interactive raw exec\n"
            "if set -q OMSH_RAW_EXEC\n"
            "    exit\n"
            "end\n"
        )
        config_path.write_text(old_guard + "\nalias ls 'ls --color=auto'\nfastfetch\n")

        _real_ensure_fish_guard_installed()

        new_content = config_path.read_text()
        assert "    exit\n" not in new_content
        assert "function status" not in new_content
        assert "if set -q OMSH_RAW_EXEC" not in new_content  # old block's own opening line, fully gone
        # The person's own real content must survive the migration untouched.
        assert "alias ls 'ls --color=auto'" in new_content
        assert (
            "if not set -q OMSH_RAW_EXEC\n"
            "    fastfetch\n"
            "end"
        ) in new_content
        # Migrating twice in a row must not double-wrap anything.
        _real_ensure_fish_guard_installed()
        assert config_path.read_text() == new_content

    def test_migrates_an_already_installed_old_status_function_guard(self, monkeypatch, tmp_path):
        """
        The SECOND broken attempt -- redefining `status` as a fish function
        -- doesn't `exit`, it fails to source config.fish at ALL (`status`
        is a reserved keyword; a real fish error confirmed this directly).
        Same migration requirement as the `exit`-based guard: strip the
        whole broken block, then apply the real per-line fix underneath.
        """
        monkeypatch.setenv("SHELL", "/usr/bin/fish")
        monkeypatch.setenv("HOME", str(tmp_path))
        monkeypatch.delenv("XDG_CONFIG_HOME", raising=False)
        config_path = tmp_path / ".config" / "fish" / "config.fish"
        config_path.parent.mkdir(parents=True)
        old_guard = (
            "# oh-my-shell: skip rest of config.fish for non-interactive raw exec\n"
            "if set -q OMSH_RAW_EXEC\n"
            "    function status --wraps status\n"
            '        if test "$argv" = "is-interactive"\n'
            "            return 1\n"
            "        end\n"
            "        builtin status $argv\n"
            "    end\n"
            "end\n"
        )
        config_path.write_text(
            old_guard
            + "\nif status is-interactive\n    # Commands to run in interactive sessions can go here\nend\n"
            "starship init fish | source\n"
            "fastfetch\n"
            'alias ls="eza --icons --group-directories-first"\n'
        )

        _real_ensure_fish_guard_installed()

        new_content = config_path.read_text()
        assert "function status" not in new_content
        assert "if set -q OMSH_RAW_EXEC" not in new_content
        assert "starship init fish | source" in new_content
        assert 'alias ls="eza --icons --group-directories-first"' in new_content
        assert (
            "if not set -q OMSH_RAW_EXEC\n"
            "    fastfetch\n"
            "end"
        ) in new_content
        _real_ensure_fish_guard_installed()
        assert config_path.read_text() == new_content

    def test_is_idempotent_does_not_double_wrap(self, monkeypatch, tmp_path):
        monkeypatch.setenv("SHELL", "/usr/bin/fish")
        monkeypatch.setenv("HOME", str(tmp_path))
        monkeypatch.delenv("XDG_CONFIG_HOME", raising=False)
        config_path = tmp_path / ".config" / "fish" / "config.fish"
        config_path.parent.mkdir(parents=True)
        config_path.write_text("fastfetch\n")

        _real_ensure_fish_guard_installed()
        first_pass = config_path.read_text()
        _real_ensure_fish_guard_installed()
        second_pass = config_path.read_text()

        assert first_pass == second_pass
        assert second_pass.count("OMSH_RAW_EXEC") == 1

    def test_respects_xdg_config_home(self, monkeypatch, tmp_path):
        monkeypatch.setenv("SHELL", "/usr/bin/fish")
        monkeypatch.setenv("HOME", str(tmp_path / "unused_home"))
        xdg_dir = tmp_path / "xdg"
        monkeypatch.setenv("XDG_CONFIG_HOME", str(xdg_dir))
        config_path = xdg_dir / "fish" / "config.fish"
        config_path.parent.mkdir(parents=True)
        config_path.write_text("fastfetch\n")

        _real_ensure_fish_guard_installed()

        assert "OMSH_RAW_EXEC" in config_path.read_text()


# `ensure_fish_guard_installed` is captured here, before this file's own
# autouse fixture patches `main_module.ensure_fish_guard_installed` to a
# no-op for every test -- `TestEnsureFishGuardInstalled` above calls this
# real reference directly instead of going through the (locally patched)
# module attribute.
_real_ensure_fish_guard_installed = main_module.ensure_fish_guard_installed


class _StopAfterWizard(Exception):
    """Sentinel raised from a patched `config_module.load` so `run()` tests
    below can observe exactly what the wizard-retry block above it printed
    without also having to fake out the entire REPL loop that follows."""


class TestRunWizardErrorHandling:
    """Covers the fresh-machine bug found via manual Docker testing: a
    transient error from the live Gemini verification call (a 503 during
    an API demand spike, in the real case that surfaced this) was raised
    by wizard.run_wizard() as ApiKeyVerificationError and, uncaught by
    run(), crashed the whole app with a raw traceback on a brand new
    user's very first launch. wizard.py itself must keep raising --
    test_wizard.py's own test_raises_when_key_verification_fails locks
    that in -- so this is the retry/clean-message handling added at the
    one call site in run() that invokes it.
    """

    def _run_with_wizard_mocked(self, monkeypatch, *, run_wizard_side_effect):
        monkeypatch.setattr(main_module.wizard_module, "should_run_wizard", lambda: True)
        run_wizard_mock = MagicMock(side_effect=run_wizard_side_effect)
        monkeypatch.setattr(main_module.wizard_module, "run_wizard", run_wizard_mock)

        buffer, console = _buffer_console()
        monkeypatch.setattr(main_module, "themed_console", lambda: console)
        monkeypatch.setattr(main_module.config_module, "load", MagicMock(side_effect=_StopAfterWizard))

        return buffer, run_wizard_mock

    def test_successful_verification_does_not_retry(self, monkeypatch):
        buffer, run_wizard_mock = self._run_with_wizard_mocked(
            monkeypatch, run_wizard_side_effect=[None]
        )

        with pytest.raises(_StopAfterWizard):
            main_module.run()

        assert run_wizard_mock.call_count == 1
        assert "Couldn't verify" not in buffer.getvalue()

    def test_transient_failure_then_success_retries_and_continues(self, monkeypatch):
        buffer, run_wizard_mock = self._run_with_wizard_mocked(
            monkeypatch,
            run_wizard_side_effect=[
                main_module.wizard_module.ApiKeyVerificationError("503 UNAVAILABLE. high demand"),
                None,
            ],
        )

        with pytest.raises(_StopAfterWizard):
            main_module.run()

        assert run_wizard_mock.call_count == 2
        output = buffer.getvalue()
        assert "Couldn't verify that API key" in output
        assert "503 UNAVAILABLE" in output
        assert "try again" in output.lower()

    def test_exhausting_all_attempts_gives_up_cleanly_without_crashing(self, monkeypatch):
        buffer, run_wizard_mock = self._run_with_wizard_mocked(
            monkeypatch,
            run_wizard_side_effect=main_module.wizard_module.ApiKeyVerificationError("invalid key"),
        )

        # Should return quietly (no unhandled exception reaching the caller,
        # and in particular never reaching config_module.load -- if it did,
        # our _StopAfterWizard sentinel would fire instead).
        main_module.run()

        assert run_wizard_mock.call_count == 3
        output = buffer.getvalue()
        assert output.count("Couldn't verify that API key") == 3
        assert "Giving up after 3 attempts" in output


class TestRunLoadsDotenv:
    """Covers a second real bug found in the same fresh-machine Docker test
    as TestRunWizardErrorHandling above: wizard.save_api_key() writes
    GOOGLE_AI_STUDIO_API_KEY into ~/.oh-my-shell/.env, but nothing ever
    loaded that file back into os.environ -- danger_classifier.py and
    intent_parser.py both read the key via a plain os.environ.get(), which
    never sees a value that only lives in a .env file. The wizard reported
    a successfully verified key, then every natural-language command in
    the very same session immediately failed with "GOOGLE_AI_STUDIO_API_KEY
    is not set". Fixed with load_dotenv(wizard_module.ENV_PATH) in run(),
    both before should_run_wizard() (so an existing .env from a previous
    run is picked up on every normal startup) and again right after a
    fresh wizard run writes one for the first time (so the very first
    session can use it without restarting).
    """

    def _run_until_stop(self, monkeypatch, *, env_path, wizard_runs=False):
        monkeypatch.setattr(main_module.wizard_module, "should_run_wizard", lambda: wizard_runs)
        if wizard_runs:
            monkeypatch.setattr(
                main_module.wizard_module,
                "run_wizard",
                MagicMock(side_effect=lambda **_: env_path.write_text(
                    "GOOGLE_AI_STUDIO_API_KEY=key-from-wizard\n"
                )),
            )
        monkeypatch.setattr(main_module.wizard_module, "ENV_PATH", env_path)

        buffer, console = _buffer_console()
        monkeypatch.setattr(main_module, "themed_console", lambda: console)
        monkeypatch.setattr(main_module.config_module, "load", MagicMock(side_effect=_StopAfterWizard))

        with pytest.raises(_StopAfterWizard):
            main_module.run()

    def test_existing_env_file_is_loaded_into_os_environ(self, tmp_path, monkeypatch):
        monkeypatch.delenv("GOOGLE_AI_STUDIO_API_KEY", raising=False)
        env_path = tmp_path / ".env"
        env_path.write_text("GOOGLE_AI_STUDIO_API_KEY=key-from-dotenv\n")

        self._run_until_stop(monkeypatch, env_path=env_path, wizard_runs=False)

        assert os.environ.get("GOOGLE_AI_STUDIO_API_KEY") == "key-from-dotenv"

    def test_freshly_wizard_written_env_is_usable_same_session(self, tmp_path, monkeypatch):
        monkeypatch.delenv("GOOGLE_AI_STUDIO_API_KEY", raising=False)
        env_path = tmp_path / ".env"
        assert not env_path.exists()

        self._run_until_stop(monkeypatch, env_path=env_path, wizard_runs=True)

        # This is the exact bug: the wizard writes the file, but without a
        # second load_dotenv() call after it, this assertion used to fail --
        # the key existed on disk but never made it into os.environ for the
        # rest of this same process.
        assert os.environ.get("GOOGLE_AI_STUDIO_API_KEY") == "key-from-wizard"

    def test_real_exported_env_var_takes_priority_over_dotenv_file(self, tmp_path, monkeypatch):
        monkeypatch.setenv("GOOGLE_AI_STUDIO_API_KEY", "key-from-real-shell-env")
        env_path = tmp_path / ".env"
        env_path.write_text("GOOGLE_AI_STUDIO_API_KEY=key-from-dotenv\n")

        self._run_until_stop(monkeypatch, env_path=env_path, wizard_runs=False)

        assert os.environ.get("GOOGLE_AI_STUDIO_API_KEY") == "key-from-real-shell-env"


# --- Removed / superseded tests --------------------------------------------------
#
# test_run_exits_process_if_registry_fails_to_load, and every `registry`
# fixture/argument in this file, are gone: run() no longer loads a registry
# at all (see main.py's own module docstring: "run() no longer loads a
# registry at all"), so there is no registry-load failure path left to
# test. test_handle_natural_language_edit_then_confirm_executes_edited_plan
# was translated into test_repl_edit_flow_applies_edit (edit is now a
# direct command-text replacement, not a param sub-flow) plus the discussion
# loop's own edit-reaches-final-plan coverage in test_discussion.py.