"""
Tests for intent_parser.py (Build Order Step 5), open-ended architecture.

Uses a fake IntentBackend for all the parse_intent() flow tests, so these
run fast/deterministic without any network call. The GoogleAIStudioBackend
rollover tests inject a fake `genai`-like client via monkeypatching
`GoogleAIStudioBackend._client`, never making a real network call.
"""


import json
from unittest.mock import patch

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
# "client" object exposing just what generate() actually calls
# (client.models.generate_content(...) / generate_content_stream(...)) -- no
# real network call, no real google.genai import side effects beyond what
# _client() itself already does elsewhere in the app.


class _FakeResponse:
    def __init__(self, text: str):
        self.text = text


class _FakeUsageMetadata:
    def __init__(self, *, prompt_token_count=None, candidates_token_count=None):
        self.prompt_token_count = prompt_token_count
        self.candidates_token_count = candidates_token_count


class _FakeStreamChunk:
    def __init__(self, *, text, usage_metadata=None):
        self.text = text
        self.usage_metadata = usage_metadata


class _FakeModels:
    def __init__(self, behavior):
        self._behavior = behavior

    def generate_content(self, *, model, contents, config=None):
        outcome = self._behavior(model)
        if isinstance(outcome, Exception):
            raise outcome
        return _FakeResponse(outcome)

    def generate_content_stream(self, *, model, contents, config=None):
        outcome = self._behavior(model)
        if isinstance(outcome, Exception):
            raise outcome
        return iter(outcome)


class _FakeGenAI:
    """Stands in for the `client` object GoogleAIStudioBackend._client() returns."""

    def __init__(self, behavior):
        self.models = _FakeModels(behavior)


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


class TestGoogleAIStudioBackendStreaming:
    """
    `on_token` switches generate() onto the `stream=True` path, and real
    token counts are recovered from the (only-on-the-final-chunk)
    `usage_metadata`.
    """

    def test_on_token_switches_to_streaming_and_reports_deltas(self):
        final_json = json.dumps({"command": "ls", "risk": "low", "explanation": ""})
        # Simulate two streamed chunks whose .text values are DELTAS (not
        # accumulated), the second one carrying usage_metadata (only the
        # FINAL chunk does, per this SDK's own behavior).
        half = len(final_json) // 2
        stream_chunks = [
            _FakeStreamChunk(text=final_json[:half]),
            _FakeStreamChunk(
                text=final_json[half:],
                usage_metadata=_FakeUsageMetadata(prompt_token_count=12, candidates_token_count=7),
            ),
        ]

        def behavior(model_name):
            return stream_chunks

        progress_events = []
        backend = intent_parser.GoogleAIStudioBackend(api_key="fake-key")
        with patch.object(backend, "_client", return_value=_FakeGenAI(behavior)):
            result = backend.generate(
                system_prompt="sys", user_message="hi", schema={}, on_token=progress_events.append
            )

        assert json.loads(result)["command"] == "ls"
        assert len(progress_events) == 2
        # Deltas concatenate back to the full response.
        assert progress_events[0].text_delta + progress_events[1].text_delta == final_json
        # text_so_far accumulates.
        assert progress_events[1].text_so_far == final_json
        assert progress_events[1].tokens_out == 2  # one increment per chunk with real content

    def test_streaming_populates_telemetry_from_final_chunk(self):
        final_json = json.dumps({"command": "ls", "risk": "low", "explanation": ""})
        stream_chunks = [
            _FakeStreamChunk(text=final_json),  # no usage_metadata mid-stream
        ]
        # Append a trailing no-text chunk carrying the final usage_metadata,
        # matching how a real stream's last chunk can be metadata-only.
        stream_chunks.append(
            _FakeStreamChunk(text="", usage_metadata=_FakeUsageMetadata(prompt_token_count=20, candidates_token_count=9))
        )

        def behavior(model_name):
            return stream_chunks

        backend = intent_parser.GoogleAIStudioBackend(api_key="fake-key")
        with patch.object(backend, "_client", return_value=_FakeGenAI(behavior)):
            backend.generate(system_prompt="sys", user_message="hi", schema={}, on_token=lambda _p: None)

        assert backend.last_telemetry.tokens_in == 20
        assert backend.last_telemetry.tokens_out == 9
        assert backend.last_telemetry.model == intent_parser.GOOGLE_AI_STUDIO_MODELS[0]

    def test_streaming_still_rolls_over_on_quota_error(self):
        final_json = json.dumps({"command": "ls", "risk": "low", "explanation": ""})
        calls = []

        def behavior(model_name):
            calls.append(model_name)
            if model_name == intent_parser.GOOGLE_AI_STUDIO_MODELS[0]:
                raise RuntimeError("429 quota exceeded")
            return [
                _FakeStreamChunk(
                    text=final_json,
                    usage_metadata=_FakeUsageMetadata(prompt_token_count=1, candidates_token_count=1),
                )
            ]

        backend = intent_parser.GoogleAIStudioBackend(api_key="fake-key")
        with patch.object(backend, "_client", return_value=_FakeGenAI(behavior)):
            result = backend.generate(
                system_prompt="sys", user_message="hi", schema={}, on_token=lambda _p: None
            )

        assert json.loads(result)["command"] == "ls"
        assert calls == list(intent_parser.GOOGLE_AI_STUDIO_MODELS)
        assert backend.last_telemetry.model == intent_parser.GOOGLE_AI_STUDIO_MODELS[1]

    def test_parse_intent_forwards_on_token_to_google_backend(self, monkeypatch):
        """
        End-to-end confirmation that parse_intent()'s _backend_accepts_on_token
        duck-typing check now recognizes GoogleAIStudioBackend too (it used
        to report False for this backend, silently dropping on_token).
        """
        final_json = json.dumps({"command": "ls", "risk": "low", "explanation": ""})

        def fake_generate(self, *, system_prompt, user_message, schema, on_token=None):
            assert on_token is not None  # would fail before the fix (on_token silently dropped)
            on_token(intent_parser.StreamProgress(text_delta=final_json, text_so_far=final_json, tokens_out=1))
            return final_json

        monkeypatch.setattr(intent_parser.GoogleAIStudioBackend, "generate", fake_generate)

        seen = []
        result = intent_parser.parse_intent("anything", on_token=seen.append)

        assert result.action == "ls"
        assert len(seen) == 1


# --- parse_intent: provider selection -------------------------------------------


def test_parse_intent_defaults_to_google_ai_studio_backend_when_no_backend_given(monkeypatch):
    """Confirms _backend_for() builds a GoogleAIStudioBackend by default."""

    def fake_generate(self, *, system_prompt, user_message, schema):
        return json.dumps({"command": "ls", "risk": "low", "explanation": ""})

    monkeypatch.setattr(intent_parser.GoogleAIStudioBackend, "generate", fake_generate)

    result = intent_parser.parse_intent("anything")

    assert result.action == "ls"