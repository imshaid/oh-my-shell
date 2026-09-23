"""
Tests for intent_parser.py (Build Order Step 5), rewritten for the
open-ended architecture (see intent_parser.py's own module docstring).

Uses a fake IntentBackend for all the parse_intent() flow tests, so these
run fast/deterministic without needing a real Ollama install, model, or
network call -- only the `test_ollama_backend_*` tests mock `ollama.chat`
directly (unchanged in spirit from before), and the GoogleAIStudioBackend
rollover tests inject a fake `genai`-like client via monkeypatching
`GoogleAIStudioBackend._client`, never making a real network call.
"""

import json
from unittest.mock import MagicMock, patch

import pytest

from ohmyshell import intent_parser


class FakeBackend:
    """
    Returns a scripted sequence of raw JSON strings, one per call, so tests
    can control exactly what the "model" says on the first attempt vs. a
    retry without touching Ollama or Google AI Studio at all.
    """

    def __init__(self, responses: list[dict]):
        self._responses = list(responses)
        self.calls: list[dict] = []

    def generate(self, *, system_prompt, user_message, schema):
        self.calls.append(
            {"system_prompt": system_prompt, "user_message": user_message, "schema": schema}
        )
        return json.dumps(self._responses.pop(0))


# --- build_schema (fixed shape, no registry) -----------------------------------


def test_build_schema_has_fixed_command_risk_explanation_shape():
    schema = intent_parser.build_schema()

    assert set(schema["required"]) == {"command", "risk", "explanation"}
    assert schema["properties"]["risk"]["enum"] == ["low", "medium", "high"]
    assert schema["properties"]["command"]["type"] == "string"


def test_build_schema_takes_no_arguments():
    # Calling with an argument (the old registry-based signature) must fail —
    # this locks in that build_schema() is no longer registry-driven.
    with pytest.raises(TypeError):
        intent_parser.build_schema(object())


# --- parse_intent: happy path ---------------------------------------------------


def test_happy_path_valid_command_on_first_attempt():
    backend = FakeBackend(
        [{"command": "ps aux | grep chrome", "risk": "low", "explanation": "List chrome processes."}]
    )

    result = intent_parser.parse_intent("find chrome processes", backend=backend)

    assert result.action == "ps aux | grep chrome"
    assert result.attempts == 1
    assert result.intent is not None
    assert result.intent.command == "ps aux | grep chrome"
    assert result.intent.risk == "low"
    assert len(backend.calls) == 1


def test_model_own_risk_assessment_is_carried_through_by_parse_intent():
    """
    intent_parser.py only validates SHAPE (see its own module docstring) --
    it does not itself run the independent override_risk() check any more
    (there's no registry lookup left to do that job either). The caller
    (main.py) is expected to call danger_classifier.override_risk()
    afterward -- see test_danger_classifier.py and test_main.py for that
    coverage. Here we only confirm parse_intent() passes the model's risk
    through unmodified.
    """
    backend = FakeBackend(
        [{"command": "rm -rf /home/user/.cache", "risk": "low", "explanation": "Clear the user cache."}]
    )

    result = intent_parser.parse_intent("clear my cache", backend=backend)

    assert result.intent.risk == "low"  # not yet overridden — that's a different layer's job


# --- parse_intent: model produces something unusable on first try (no retry) --
#
# There is no more "unmapped" model OUTPUT sentinel to check for (the model
# always tries to produce a real command now) -- "unmapped" only happens
# when BOTH attempts fail harness validation (see below).


# --- parse_intent: first attempt fails validation, retry succeeds -------------


def test_first_attempt_invalid_second_attempt_valid():
    backend = FakeBackend(
        [
            # empty command fails harness validation
            {"command": "", "risk": "low", "explanation": ""},
            # retry succeeds
            {"command": "ls ~/Downloads", "risk": "low", "explanation": "List Downloads."},
        ]
    )

    result = intent_parser.parse_intent("show my downloads", backend=backend)

    assert result.action == "ls ~/Downloads"
    assert result.attempts == 2
    assert result.intent.command == "ls ~/Downloads"
    assert len(backend.calls) == 2
    # the retry call's user_message should include the validation error as guidance
    assert "invalid" in backend.calls[1]["user_message"].lower()


# --- parse_intent: both attempts fail -> unmapped ------------------------------


def test_both_attempts_fail_validation_falls_back_to_unmapped():
    backend = FakeBackend(
        [
            {"command": "", "risk": "low", "explanation": ""},
            {"command": "", "risk": "low", "explanation": ""},  # still empty
        ]
    )

    result = intent_parser.parse_intent("do something impossible", backend=backend)

    assert result.action == intent_parser.UNMAPPED_ACTION
    assert result.intent is None
    assert result.attempts == 2
    assert result.last_error is not None


def test_invalid_risk_enum_falls_back_to_unmapped_after_retry():
    backend = FakeBackend(
        [
            {"command": "ls", "risk": "catastrophic", "explanation": ""},
            {"command": "ls", "risk": "catastrophic", "explanation": ""},
        ]
    )

    result = intent_parser.parse_intent("do something dangerous", backend=backend)

    assert result.action == intent_parser.UNMAPPED_ACTION


def test_malformed_json_from_backend_treated_as_validation_failure():
    """A non-JSON-parsable response shouldn't crash parse_intent — treated as a failed attempt."""

    class BrokenBackend:
        def __init__(self):
            self.call_count = 0

        def generate(self, *, system_prompt, user_message, schema):
            self.call_count += 1
            if self.call_count == 1:
                return "this is not json"
            return json.dumps({"command": "ls", "risk": "low", "explanation": ""})

    backend = BrokenBackend()

    result = intent_parser.parse_intent("anything", backend=backend)

    assert result.action == "ls"
    assert backend.call_count == 2


# --- OllamaBackend: think:false / schema-format contract -----------------------


def _fake_stream_chunk(*, content="", done=False, **fields):
    chunk = MagicMock()
    chunk.message = MagicMock()
    chunk.message.content = content
    chunk.eval_count = fields.get("eval_count")
    chunk.prompt_eval_count = fields.get("prompt_eval_count")
    chunk.total_duration = fields.get("total_duration")
    chunk.model = fields.get("model")
    chunk.done = done
    return chunk


def test_ollama_backend_passes_think_false_schema_format_and_stream():
    final_content = json.dumps({"command": "ls", "risk": "low", "explanation": "List files."})
    chunks = [
        _fake_stream_chunk(content=final_content[:5]),
        _fake_stream_chunk(content=final_content[5:]),
        _fake_stream_chunk(
            content="",
            done=True,
            eval_count=12,
            prompt_eval_count=40,
            total_duration=500_000_000,
            model="qwen3:8b",
        ),
    ]

    schema = intent_parser.build_schema()

    with patch("ohmyshell.intent_parser.ollama.chat", return_value=iter(chunks)) as mock_chat:
        backend = intent_parser.OllamaBackend(model="qwen3:8b")
        result = backend.generate(system_prompt="sys", user_message="hello", schema=schema)

    assert result == final_content
    _, kwargs = mock_chat.call_args
    assert kwargs["think"] is False
    assert kwargs["format"] == schema  # actual schema dict, never the string "json"
    assert kwargs["model"] == "qwen3:8b"
    assert kwargs["stream"] is True
    assert backend.last_telemetry.tokens_out == 12
    assert backend.last_telemetry.tokens_in == 40
    assert backend.last_telemetry.duration_seconds == 0.5


def test_ollama_backend_calls_on_token_for_every_chunk():
    final_content = json.dumps({"command": "ls", "risk": "low", "explanation": ""})
    chunks = [
        _fake_stream_chunk(content=final_content[:5]),
        _fake_stream_chunk(content=final_content[5:10]),
        _fake_stream_chunk(content=final_content[10:]),
        _fake_stream_chunk(content="", done=True, eval_count=12, prompt_eval_count=40, total_duration=500_000_000),
    ]
    schema = intent_parser.build_schema()
    seen: list[intent_parser.StreamProgress] = []

    with patch("ohmyshell.intent_parser.ollama.chat", return_value=iter(chunks)):
        backend = intent_parser.OllamaBackend(model="qwen3:8b")
        backend.generate(system_prompt="sys", user_message="hello", schema=schema, on_token=seen.append)

    assert len(seen) == 4
    assert [p.tokens_out for p in seen] == [1, 2, 3, 3]
    assert seen[0].text_delta == final_content[:5]
    assert seen[1].text_delta == final_content[5:10]
    assert seen[0].text_so_far == final_content[:5]
    assert seen[1].text_so_far == final_content[:10]
    assert seen[2].text_so_far == final_content


def test_ollama_backend_raises_intent_parse_error_on_client_exception():
    with patch("ohmyshell.intent_parser.ollama.chat", side_effect=RuntimeError("connection refused")):
        backend = intent_parser.OllamaBackend(model="qwen3:8b")
        with pytest.raises(intent_parser.IntentParseError):
            backend.generate(system_prompt="sys", user_message="hi", schema={})


def test_parse_intent_forwards_on_token_when_backend_supports_it():
    final_content = json.dumps({"command": "ls", "risk": "low", "explanation": ""})
    chunks = [_fake_stream_chunk(content=final_content, done=True, eval_count=5)]
    seen: list[intent_parser.StreamProgress] = []

    with patch("ohmyshell.intent_parser.ollama.chat", return_value=iter(chunks)):
        backend = intent_parser.OllamaBackend(model="qwen3:8b")
        intent_parser.parse_intent("anything", backend=backend, on_token=seen.append)

    assert len(seen) == 1


def test_parse_intent_ignores_on_token_when_backend_does_not_support_it():
    """
    A backend with the older 3-kwarg generate() signature (every
    hand-written fake in this test file, and GoogleAIStudioBackend) must
    not blow up just because a caller happens to pass on_token --
    parse_intent()'s duck-typing check (_backend_accepts_on_token) must
    fall back to the plain 3-kwarg call.
    """
    backend = FakeBackend([{"command": "ls", "risk": "low", "explanation": ""}])

    result = intent_parser.parse_intent("anything", backend=backend, on_token=lambda progress: None)

    assert result.action == "ls"


# --- GoogleAIStudioBackend: _looks_like_quota_error -----------------------------


class TestLooksLikeQuotaError:
    @pytest.mark.parametrize(
        "message",
        [
            "429 Too Many Requests",
            "Quota exceeded for this project",
            "You have hit the rate limit",
            "google.api_core.exceptions.ResourceExhausted: quota",
        ],
    )
    def test_recognizes_quota_markers(self, message):
        assert intent_parser._looks_like_quota_error(RuntimeError(message)) is True

    def test_does_not_flag_an_unrelated_error(self):
        assert intent_parser._looks_like_quota_error(RuntimeError("invalid API key")) is False


# --- GoogleAIStudioBackend: rollover behavior -----------------------------------
#
# Tested by monkeypatching GoogleAIStudioBackend._client to return a fake
# "genai" object exposing just what generate() actually calls
# (GenerativeModel(...).generate_content(...)) -- no real network call, no
# real google.generativeai import side effects beyond what _client() itself
# already does elsewhere in the app.


class _FakeResponse:
    def __init__(self, text: str):
        self.text = text


class _FakeGenerativeModel:
    def __init__(self, model_name, *, system_instruction=None, behavior):
        self.model_name = model_name
        self.behavior = behavior

    def generate_content(self, user_message, *, generation_config):
        outcome = self.behavior(self.model_name)
        if isinstance(outcome, Exception):
            raise outcome
        return _FakeResponse(outcome)


class _FakeGenAI:
    """Stands in for the `genai` module GoogleAIStudioBackend._client() returns."""

    def __init__(self, behavior):
        self._behavior = behavior

    def GenerativeModel(self, model_name, system_instruction=None):
        return _FakeGenerativeModel(model_name, system_instruction=system_instruction, behavior=self._behavior)


class TestGoogleAIStudioBackendRollover:
    def test_uses_primary_model_when_it_succeeds(self):
        calls = []

        def behavior(model_name):
            calls.append(model_name)
            return json.dumps({"command": "ls", "risk": "low", "explanation": ""})

        backend = intent_parser.GoogleAIStudioBackend(api_key="fake-key")
        with patch.object(backend, "_client", return_value=_FakeGenAI(behavior)):
            result = backend.generate(system_prompt="sys", user_message="hi", schema={})

        assert json.loads(result)["command"] == "ls"
        assert calls == [intent_parser.GOOGLE_AI_STUDIO_MODELS[0]]
        assert backend.last_telemetry.model == intent_parser.GOOGLE_AI_STUDIO_MODELS[0]

    def test_rolls_over_to_fallback_model_on_quota_error(self):
        calls = []

        def behavior(model_name):
            calls.append(model_name)
            if model_name == intent_parser.GOOGLE_AI_STUDIO_MODELS[0]:
                raise RuntimeError("429 quota exceeded")
            return json.dumps({"command": "ls", "risk": "low", "explanation": ""})

        backend = intent_parser.GoogleAIStudioBackend(api_key="fake-key")
        with patch.object(backend, "_client", return_value=_FakeGenAI(behavior)):
            result = backend.generate(system_prompt="sys", user_message="hi", schema={})

        assert json.loads(result)["command"] == "ls"
        assert calls == list(intent_parser.GOOGLE_AI_STUDIO_MODELS)
        assert backend.last_telemetry.model == intent_parser.GOOGLE_AI_STUDIO_MODELS[1]

    def test_non_quota_error_raises_immediately_without_rollover(self):
        calls = []

        def behavior(model_name):
            calls.append(model_name)
            raise RuntimeError("invalid API key")

        backend = intent_parser.GoogleAIStudioBackend(api_key="fake-key")
        with patch.object(backend, "_client", return_value=_FakeGenAI(behavior)):
            with pytest.raises(intent_parser.IntentParseError):
                backend.generate(system_prompt="sys", user_message="hi", schema={})

        # Must NOT have tried the fallback model — a real bug shouldn't be
        # silently masked behind a rollover.
        assert calls == [intent_parser.GOOGLE_AI_STUDIO_MODELS[0]]

    def test_quota_error_on_every_model_raises_intent_parse_error(self):
        def behavior(model_name):
            raise RuntimeError("429 quota exceeded")

        backend = intent_parser.GoogleAIStudioBackend(api_key="fake-key")
        with patch.object(backend, "_client", return_value=_FakeGenAI(behavior)):
            with pytest.raises(intent_parser.IntentParseError):
                backend.generate(system_prompt="sys", user_message="hi", schema={})

    def test_missing_api_key_raises_intent_parse_error(self, monkeypatch):
        monkeypatch.delenv("GOOGLE_AI_STUDIO_API_KEY", raising=False)
        backend = intent_parser.GoogleAIStudioBackend(api_key=None)
        with pytest.raises(intent_parser.IntentParseError):
            backend.generate(system_prompt="sys", user_message="hi", schema={})


# --- parse_intent: provider selection -------------------------------------------


def test_parse_intent_defaults_to_google_ai_studio_provider_when_no_backend_given(monkeypatch):
    """
    Confirms _backend_for()'s selection logic without a real network call:
    monkeypatch GoogleAIStudioBackend.generate itself so parse_intent()'s
    default construction path (model_provider="google_ai_studio") is what's
    actually exercised.
    """

    def fake_generate(self, *, system_prompt, user_message, schema):
        return json.dumps({"command": "ls", "risk": "low", "explanation": ""})

    monkeypatch.setattr(intent_parser.GoogleAIStudioBackend, "generate", fake_generate)

    result = intent_parser.parse_intent("anything")

    assert result.action == "ls"


def test_parse_intent_uses_ollama_backend_when_provider_is_ollama():
    final_content = json.dumps({"command": "ls", "risk": "low", "explanation": ""})
    chunks = [_fake_stream_chunk(content=final_content, done=True, eval_count=5)]

    with patch("ohmyshell.intent_parser.ollama.chat", return_value=iter(chunks)):
        result = intent_parser.parse_intent("anything", model_provider="ollama", model="qwen3:8b")

    assert result.action == "ls"


# --- Removed tests (no longer applicable) ---------------------------------------
#
# test_build_schema_includes_all_registry_actions_plus_unmapped,
# test_build_schema_requires_action_risk_params (old required set included
# "params"), test_system_prompt_lists_every_registered_action_description,
# test_system_prompt_includes_knowledge_file_content_when_present,
# test_system_prompt_tolerates_missing_knowledge_file,
# test_system_prompt_includes_each_capabilitys_few_shot_examples,
# test_system_prompt_tolerates_capability_with_no_few_shot_examples,
# test_format_capability_line_* (three tests), test_model_says_unmapped_on_
# first_try_no_retry, test_retry_attempt_says_unmapped_falls_back_to_
# unmapped, test_unknown_action_falls_back_to_unmapped_after_retry --
# _build_system_prompt/_format_capability_line/build_schema(registry) and
# the whole knowledge_path mechanism were all removed (there is no registry
# to describe capabilities from any more -- SYSTEM_PROMPT is now one fixed
# constant, see intent_parser.py's own module docstring). The old
# "unmapped" MODEL OUTPUT tests no longer apply either -- the model always
# tries to produce a real command now; "unmapped" only occurs when both
# harness-validation attempts fail (covered above).
# test_google_ai_studio_fallback_is_not_yet_implemented tested the old
# _call_google_ai_studio() NotImplementedError stub, which is gone now that
# GoogleAIStudioBackend is a real, implemented backend (see
# TestGoogleAIStudioBackendRollover above for its replacement coverage).