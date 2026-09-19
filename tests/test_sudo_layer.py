"""Tests for the Sudo/Permission Escalation Layer (Build Order Step 9)."""

from __future__ import annotations

import pytest

from ohmyshell.sudo_layer import (
    ElevatedStep,
    InputPrompt,
    SudoDecision,
    decide_step,
    render_prompt_text,
)


def _step(**overrides) -> ElevatedStep:
    defaults = dict(
        step_number=3,
        total_steps=5,
        description="Clear system-level cache in /var/cache",
        reason="this directory is owned by root",
    )
    defaults.update(overrides)
    return ElevatedStep(**defaults)


class _ScriptedPrompt:
    """Fake SudoPrompt returning a pre-scripted decision, recording calls."""

    def __init__(self, decision: SudoDecision):
        self.decision = decision
        self.asked_with: ElevatedStep | None = None
        self.call_count = 0

    def ask(self, step: ElevatedStep) -> SudoDecision:
        self.call_count += 1
        self.asked_with = step
        return self.decision


class TestRenderPromptText:
    def test_includes_step_number_and_total(self):
        text = render_prompt_text(_step(step_number=3, total_steps=5))
        assert "Step 3/5" in text

    def test_includes_description_and_reason(self):
        text = render_prompt_text(
            _step(description="Clear system-level cache in /var/cache", reason="this directory is owned by root")
        )
        assert "Clear system-level cache in /var/cache" in text
        assert "this directory is owned by root" in text

    def test_includes_all_three_menu_options(self):
        text = render_prompt_text(_step())
        assert "Grant" in text
        assert "sudo" in text
        assert "Skip" in text
        assert "Abort" in text

    def test_starts_with_warning_header(self):
        text = render_prompt_text(_step())
        assert text.startswith("⚠ Next step requires elevated permission")


class TestDecideStepWithFakePrompt:
    def test_returns_grant_decision_from_prompt(self):
        step = _step()
        prompt = _ScriptedPrompt(SudoDecision.GRANT)
        result = decide_step(step, prompt=prompt)
        assert result is SudoDecision.GRANT

    def test_returns_skip_decision_from_prompt(self):
        prompt = _ScriptedPrompt(SudoDecision.SKIP)
        result = decide_step(_step(), prompt=prompt)
        assert result is SudoDecision.SKIP

    def test_returns_abort_decision_from_prompt(self):
        prompt = _ScriptedPrompt(SudoDecision.ABORT)
        result = decide_step(_step(), prompt=prompt)
        assert result is SudoDecision.ABORT

    def test_passes_the_exact_step_through_to_the_prompt(self):
        step = _step(step_number=2, description="Delete old logs", reason="root-owned dir")
        prompt = _ScriptedPrompt(SudoDecision.GRANT)
        decide_step(step, prompt=prompt)
        assert prompt.asked_with is step

    def test_calls_prompt_exactly_once(self):
        prompt = _ScriptedPrompt(SudoDecision.GRANT)
        decide_step(_step(), prompt=prompt)
        assert prompt.call_count == 1

    def test_defaults_to_input_prompt_when_none_given(self, monkeypatch):
        # decide_step() with no `prompt` kwarg must fall back to InputPrompt;
        # verify by monkeypatching the `input` name InputPrompt's default
        # argument resolves against at call time (ohmyshell.sudo_layer.input).
        responses = iter([""])
        monkeypatch.setattr("ohmyshell.sudo_layer.input", lambda _: next(responses), raising=False)
        result = decide_step(_step())
        assert result is SudoDecision.GRANT


class TestInputPromptKeyMapping:
    def _run(self, keystrokes: list[str]) -> tuple[SudoDecision, list[str]]:
        printed: list[str] = []
        responses = iter(keystrokes)
        prompt = InputPrompt(input_fn=lambda _: next(responses), print_fn=printed.append)
        decision = prompt.ask(_step())
        return decision, printed

    def test_empty_line_grants(self):
        decision, _ = self._run([""])
        assert decision is SudoDecision.GRANT

    def test_whitespace_only_line_grants(self):
        decision, _ = self._run(["   "])
        assert decision is SudoDecision.GRANT

    def test_lowercase_s_skips(self):
        decision, _ = self._run(["s"])
        assert decision is SudoDecision.SKIP

    def test_uppercase_s_skips(self):
        decision, _ = self._run(["S"])
        assert decision is SudoDecision.SKIP

    def test_esc_aborts(self):
        decision, _ = self._run(["esc"])
        assert decision is SudoDecision.ABORT

    def test_q_aborts(self):
        decision, _ = self._run(["q"])
        assert decision is SudoDecision.ABORT

    def test_word_abort_aborts(self):
        decision, _ = self._run(["abort"])
        assert decision is SudoDecision.ABORT

    def test_case_insensitive_abort_keywords(self):
        decision, _ = self._run(["Q"])
        assert decision is SudoDecision.ABORT

    def test_invalid_choice_reprompts_then_accepts_valid_choice(self):
        decision, printed = self._run(["x", "s"])
        assert decision is SudoDecision.SKIP
        assert any("Please press Enter" in line for line in printed)

    def test_prints_the_rendered_prompt_text_first(self):
        _, printed = self._run([""])
        assert printed[0] == render_prompt_text(_step())

    def test_multiple_invalid_choices_before_valid_one(self):
        decision, printed = self._run(["nope", "??", ""])
        assert decision is SudoDecision.GRANT
        assert sum("Please press Enter" in line for line in printed) == 2


class TestSudoDecisionEnum:
    def test_three_distinct_members(self):
        assert len({SudoDecision.GRANT, SudoDecision.SKIP, SudoDecision.ABORT}) == 3

    def test_members_are_not_equal_to_each_other(self):
        assert SudoDecision.GRANT != SudoDecision.SKIP
        assert SudoDecision.SKIP != SudoDecision.ABORT
        assert SudoDecision.GRANT != SudoDecision.ABORT


class TestElevatedStepIsFrozenDataclass:
    def test_fields_are_accessible(self):
        step = _step(step_number=1, total_steps=1, description="d", reason="r")
        assert step.step_number == 1
        assert step.total_steps == 1
        assert step.description == "d"
        assert step.reason == "r"

    def test_is_immutable(self):
        step = _step()
        with pytest.raises(Exception):
            step.step_number = 99  # type: ignore[misc]