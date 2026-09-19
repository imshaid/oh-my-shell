"""
Tests for intent_parser.py (Build Order Step 5).

Uses a fake IntentBackend for all the parse_intent() flow tests, so these
run fast/deterministic without needing a real Ollama install or model —
only the small `test_ollama_backend_*` tests below mock `ollama.chat`
directly, specifically to verify the think:false / schema-format contract
(Section 7.3/7.4a) that OllamaBackend is responsible for.
"""

import json
from unittest.mock import MagicMock, patch

import pytest

from ohmyshell import intent_parser
from ohmyshell import registry as registry_module


@pytest.fixture(scope="module")
def registry():
    return registry_module.load()


class FakeBackend:
    """
    Returns a scripted sequence of raw JSON strings, one per call, so tests
    can control exactly what the "model" says on the first attempt vs. a
    retry without touching Ollama at all.
    """

    def __init__(self, responses: list[dict]):
        self._responses = list(responses)
        self.calls: list[dict] = []

    def generate(self, *, system_prompt, user_message, schema):
        self.calls.append(
            {"system_prompt": system_prompt, "user_message": user_message, "schema": schema}
        )
        return json.dumps(self._responses.pop(0))


# --- build_schema -------------------------------------------------------------


def test_build_schema_includes_all_registry_actions_plus_unmapped(registry):
    schema = intent_parser.build_schema(registry)

    action_enum = schema["properties"]["action"]["enum"]
    assert set(registry.actions()).issubset(set(action_enum))
    assert "unmapped" in action_enum


def test_build_schema_requires_action_risk_params(registry):
    schema = intent_parser.build_schema(registry)
    assert set(schema["required"]) == {"action", "risk", "params"}
    assert schema["properties"]["risk"]["enum"] == ["low", "medium", "high"]


# --- system prompt building ---------------------------------------------------


def test_system_prompt_lists_every_registered_action_description(registry, tmp_path):
    empty_knowledge = tmp_path / "knowledge.md"
    empty_knowledge.write_text("", encoding="utf-8")

    prompt = intent_parser._build_system_prompt(registry, empty_knowledge)

    for cap in registry.all_capabilities():
        assert cap["action"] in prompt
        assert cap["description"] in prompt


def test_system_prompt_includes_knowledge_file_content_when_present(registry, tmp_path):
    knowledge_file = tmp_path / "knowledge.md"
    knowledge_file.write_text("Special context: this machine has no swap.", encoding="utf-8")

    prompt = intent_parser._build_system_prompt(registry, knowledge_file)

    assert "Special context: this machine has no swap." in prompt


def test_system_prompt_tolerates_missing_knowledge_file(registry, tmp_path):
    missing = tmp_path / "does_not_exist.md"

    # Must not raise even though the file doesn't exist.
    prompt = intent_parser._build_system_prompt(registry, missing)
    assert isinstance(prompt, str)
    assert len(prompt) > 0


# --- parse_intent: happy path ---------------------------------------------------


def test_happy_path_valid_action_on_first_attempt(registry):
    backend = FakeBackend(
        [{"action": "list_processes", "risk": "low", "params": {"filter": "chrome"}}]
    )

    result = intent_parser.parse_intent("find chrome processes", registry, backend=backend)

    assert result.action == "list_processes"
    assert result.attempts == 1
    assert result.intent is not None
    assert result.intent.action == "list_processes"
    assert result.intent.risk == "low"  # registry static value, matches here anyway
    assert len(backend.calls) == 1


def test_risk_in_final_intent_is_always_registry_value_not_model_value(registry):
    """kill_process is hardcoded high risk; model claiming "low" must be overridden."""
    backend = FakeBackend(
        [{"action": "kill_process", "risk": "low", "params": {"target": "1234"}}]
    )

    result = intent_parser.parse_intent("kill pid 1234", registry, backend=backend)

    assert result.intent.risk == "high"


# --- parse_intent: model says unmapped on first try (no retry) ----------------


def test_model_says_unmapped_on_first_try_no_retry(registry):
    backend = FakeBackend([{"action": "unmapped", "risk": "low", "params": {}}])

    result = intent_parser.parse_intent("what's the meaning of life", registry, backend=backend)

    assert result.action == "unmapped"
    assert result.intent is None
    assert result.attempts == 1
    assert len(backend.calls) == 1  # no retry triggered


# --- parse_intent: first attempt fails validation, retry succeeds -------------


def test_first_attempt_invalid_second_attempt_valid(registry):
    backend = FakeBackend(
        [
            # organize_files requires target_dir — this fails harness validation
            {"action": "organize_files", "risk": "low", "params": {}},
            # retry succeeds
            {
                "action": "organize_files",
                "risk": "low",
                "params": {"target_dir": "/home/user/Downloads"},
            },
        ]
    )

    result = intent_parser.parse_intent("organize my downloads", registry, backend=backend)

    assert result.action == "organize_files"
    assert result.attempts == 2
    # organize_files' "by" param has a schema default ("extension") that
    # validate_intent now fills in when the model doesn't supply it (see
    # test_validation.py's dedicated schema-default tests).
    assert result.intent.params == {"target_dir": "/home/user/Downloads", "by": "extension"}
    assert len(backend.calls) == 2
    # the retry call's user_message should include the validation error as guidance
    assert "invalid" in backend.calls[1]["user_message"].lower()


# --- parse_intent: both attempts fail -> unmapped ------------------------------


def test_both_attempts_fail_validation_falls_back_to_unmapped(registry):
    backend = FakeBackend(
        [
            {"action": "organize_files", "risk": "low", "params": {}},
            {"action": "organize_files", "risk": "low", "params": {}},  # still missing target_dir
        ]
    )

    result = intent_parser.parse_intent("organize things", registry, backend=backend)

    assert result.action == "unmapped"
    assert result.intent is None
    assert result.attempts == 2
    assert result.last_error is not None


def test_retry_attempt_says_unmapped_falls_back_to_unmapped(registry):
    backend = FakeBackend(
        [
            {"action": "organize_files", "risk": "low", "params": {}},  # fails validation
            {"action": "unmapped", "risk": "low", "params": {}},  # model gives up on retry
        ]
    )

    result = intent_parser.parse_intent("organize things", registry, backend=backend)

    assert result.action == "unmapped"
    assert result.attempts == 2


def test_unknown_action_falls_back_to_unmapped_after_retry(registry):
    backend = FakeBackend(
        [
            {"action": "launch_missiles", "risk": "high", "params": {}},
            {"action": "launch_missiles", "risk": "high", "params": {}},
        ]
    )

    result = intent_parser.parse_intent("do something dangerous", registry, backend=backend)

    assert result.action == "unmapped"


def test_malformed_json_from_backend_treated_as_validation_failure(registry):
    """A non-JSON-parsable response shouldn't crash parse_intent — treated as a failed attempt."""

    class BrokenBackend:
        def __init__(self):
            self.call_count = 0

        def generate(self, *, system_prompt, user_message, schema):
            self.call_count += 1
            if self.call_count == 1:
                return "this is not json"
            return json.dumps({"action": "unmapped", "risk": "low", "params": {}})

    backend = BrokenBackend()

    result = intent_parser.parse_intent("anything", registry, backend=backend)

    assert result.action == "unmapped"
    assert backend.call_count == 2


# --- OllamaBackend: think:false / schema-format contract -----------------------


def test_ollama_backend_passes_think_false_and_schema_format(registry):
    """
    Section 7.3: think must be passed as a literal top-level False.
    Section 7.4a: format must receive the actual schema dict, not "json".
    """
    fake_message = MagicMock()
    fake_message.content = json.dumps({"action": "unmapped", "risk": "low", "params": {}})
    fake_response = MagicMock()
    fake_response.message = fake_message

    schema = intent_parser.build_schema(registry)

    with patch("ohmyshell.intent_parser.ollama.chat", return_value=fake_response) as mock_chat:
        backend = intent_parser.OllamaBackend(model="qwen3:8b")
        result = backend.generate(
            system_prompt="sys", user_message="hello", schema=schema
        )

    assert result == fake_message.content
    _, kwargs = mock_chat.call_args
    assert kwargs["think"] is False
    assert kwargs["format"] == schema  # actual schema dict, never the string "json"
    assert kwargs["model"] == "qwen3:8b"


def test_ollama_backend_raises_intent_parse_error_on_client_exception():
    with patch("ohmyshell.intent_parser.ollama.chat", side_effect=RuntimeError("connection refused")):
        backend = intent_parser.OllamaBackend(model="qwen3:8b")
        with pytest.raises(intent_parser.IntentParseError):
            backend.generate(system_prompt="sys", user_message="hi", schema={})


# --- Google AI Studio fallback stub --------------------------------------------


def test_google_ai_studio_fallback_is_not_yet_implemented():
    """Open Question 3 (Section 15) — must stay an explicit stub, not a silent guess."""
    with pytest.raises(NotImplementedError):
        intent_parser._call_google_ai_studio(system_prompt="sys", user_message="hi", schema={})