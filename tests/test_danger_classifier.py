"""
Tests for danger_classifier.py (Build Order Step 8).

Regex-rule tests need no mocking at all (that's the whole point of tier 1).
LLM-fallback tests use a fake backend, matching intent_parser.py's testing
approach — no real network call needed to verify the classification logic.
"""

from unittest.mock import MagicMock, patch

import pytest

from ohmyshell.danger_classifier import (
    ClassificationResult,
    Destructive,
    Safe,
    classify,
    DangerClassifierError,
    GoogleAIStudioDangerBackend,
)


class FakeDangerBackend:
    def __init__(self, response: dict):
        self._response = response
        self.calls: list[str] = []

    def classify(self, command: str) -> dict:
        self.calls.append(command)
        return self._response


# --- Regex tier: destructive patterns ------------------------------------------


@pytest.mark.parametrize(
    "command",
    [
        "rm -rf /var/log/*",
        "rm -rf /",
        "rm -fr ~/important",
        "sudo rm -rf /home/user/project",
    ],
)
def test_rm_rf_variants_are_destructive_via_regex(command):
    result = classify(command)
    assert result.source == "regex"
    assert isinstance(result.verdict, Destructive)
    assert result.verdict.trash_alternative_possible is True


def test_dd_to_device_is_destructive_via_regex():
    result = classify("dd if=/dev/zero of=/dev/sda bs=1M")
    assert result.source == "regex"
    assert isinstance(result.verdict, Destructive)
    assert result.verdict.trash_alternative_possible is False


def test_mkfs_is_destructive_via_regex():
    result = classify("mkfs.ext4 /dev/sdb1")
    assert isinstance(result.verdict, Destructive)


def test_fork_bomb_is_destructive_via_regex():
    result = classify(":(){ :|:& };:")
    assert result.source == "regex"
    assert isinstance(result.verdict, Destructive)
    assert result.verdict.trash_alternative_possible is False


def test_recursive_chmod_on_system_dir_is_destructive():
    result = classify("chmod -R 777 /etc")
    assert isinstance(result.verdict, Destructive)


def test_recursive_chown_on_system_dir_is_destructive():
    result = classify("chown -R nobody /usr")
    assert isinstance(result.verdict, Destructive)


def test_redirect_onto_disk_device_is_destructive():
    result = classify("cat garbage > /dev/sda")
    assert isinstance(result.verdict, Destructive)


def test_destructive_verdict_carries_explanation():
    result = classify("rm -rf /tmp/whatever")
    assert isinstance(result.verdict, Destructive)
    assert len(result.verdict.explanation) > 0


# --- Regex tier: safe patterns ---------------------------------------------------


@pytest.mark.parametrize(
    "command",
    ["ls -la", "cat file.txt", "grep error log.txt", "ps aux", "df -h", "find . -name '*.py'"],
)
def test_common_readonly_commands_are_safe_via_regex(command):
    result = classify(command)
    assert result.source == "regex"
    assert isinstance(result.verdict, Safe)


def test_rm_without_force_recursive_flags_is_not_matched_by_destructive_regex():
    """Plain `rm file.txt` (no -rf/-fr) isn't in the destructive regex set — falls to LLM tier."""
    fake_backend = FakeDangerBackend(
        {"destructive": False, "explanation": "", "trash_alternative_possible": False}
    )
    result = classify("rm file.txt", llm_backend=fake_backend)
    assert result.source == "llm"
    assert fake_backend.calls == ["rm file.txt"]


# --- LLM fallback tier -----------------------------------------------------------


def test_ambiguous_command_falls_back_to_llm():
    fake_backend = FakeDangerBackend(
        {
            "destructive": True,
            "explanation": "This could overwrite an important config file.",
            "trash_alternative_possible": False,
        }
    )

    result = classify("echo 'x' > ~/.bashrc", llm_backend=fake_backend)

    assert result.source == "llm"
    assert isinstance(result.verdict, Destructive)
    assert result.verdict.explanation == "This could overwrite an important config file."
    assert len(fake_backend.calls) == 1


def test_llm_fallback_can_classify_as_safe():
    fake_backend = FakeDangerBackend(
        {"destructive": False, "explanation": "", "trash_alternative_possible": False}
    )

    result = classify("some ambiguous command", llm_backend=fake_backend)

    assert result.source == "llm"
    assert isinstance(result.verdict, Safe)


def test_regex_match_never_calls_llm_backend():
    """Regex short-circuits — the LLM backend must not be touched when regex is confident."""
    fake_backend = FakeDangerBackend(
        {"destructive": False, "explanation": "", "trash_alternative_possible": False}
    )

    classify("rm -rf /tmp/x", llm_backend=fake_backend)

    assert fake_backend.calls == []


def test_llm_backend_missing_explanation_gets_a_default():
    fake_backend = FakeDangerBackend({"destructive": True, "trash_alternative_possible": True})

    result = classify("some command", llm_backend=fake_backend)

    assert isinstance(result.verdict, Destructive)
    assert len(result.verdict.explanation) > 0


# --- GoogleAIStudioDangerBackend: JSON schema / response contract --------------


def test_google_ai_studio_danger_backend_passes_schema_and_parses_response():
    fake_response = MagicMock()
    fake_response.text = (
        '{"destructive": false, "explanation": "", "trash_alternative_possible": false}'
    )
    fake_client = MagicMock()
    fake_client.models.generate_content.return_value = fake_response

    with patch("google.genai.Client", return_value=fake_client) as mock_client_cls:
        backend = GoogleAIStudioDangerBackend(model="gemini-3.5-flash-lite", api_key="test-key")
        result = backend.classify("some command")

    assert result == {"destructive": False, "explanation": "", "trash_alternative_possible": False}
    mock_client_cls.assert_called_once_with(api_key="test-key")
    _, call_kwargs = fake_client.models.generate_content.call_args
    assert call_kwargs["model"] == "gemini-3.5-flash-lite"
    assert call_kwargs["config"].response_mime_type == "application/json"


def test_google_ai_studio_danger_backend_raises_when_api_key_missing(monkeypatch):
    monkeypatch.delenv("GOOGLE_AI_STUDIO_API_KEY", raising=False)
    backend = GoogleAIStudioDangerBackend(model="gemini-3.5-flash-lite")
    with pytest.raises(DangerClassifierError):
        backend.classify("some command")


def test_google_ai_studio_danger_backend_raises_on_client_error():
    with patch("google.genai.Client", side_effect=RuntimeError("connection refused")):
        backend = GoogleAIStudioDangerBackend(model="gemini-3.5-flash-lite", api_key="test-key")
        with pytest.raises(DangerClassifierError):
            backend.classify("some command")


def test_google_ai_studio_danger_backend_raises_on_malformed_json():
    fake_response = MagicMock()
    fake_response.text = "not json"
    fake_client = MagicMock()
    fake_client.models.generate_content.return_value = fake_response

    with patch("google.genai.Client", return_value=fake_client):
        backend = GoogleAIStudioDangerBackend(model="gemini-3.5-flash-lite", api_key="test-key")
        with pytest.raises(DangerClassifierError):
            backend.classify("some command")


def test_classify_propagates_backend_error():
    class BrokenBackend:
        def classify(self, command):
            raise DangerClassifierError("boom")

    with pytest.raises(DangerClassifierError):
        classify("ambiguous thing", llm_backend=BrokenBackend())