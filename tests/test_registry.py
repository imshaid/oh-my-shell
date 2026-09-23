"""Tests for registry.py (Build Order Step 3) — capabilities.json loader + jsonschema integrity check."""

import json

import pytest

from ohmyshell import registry as registry_module


def test_loads_real_capabilities_json_from_default_path():
    """The actual repo capabilities/capabilities.json (core-4) must load cleanly."""
    reg = registry_module.load()

    assert reg.version == "1.0"
    assert len(reg) == 4
    assert set(reg.actions()) == {
        "clean_temp_files",
        "organize_files",
        "list_processes",
        "kill_process",
    }


def test_risk_for_matches_registry_static_values():
    """Section 7.4: risk is a static, hardcoded field — never AI-generated."""
    reg = registry_module.load()

    assert reg.risk_for("clean_temp_files") == "medium"
    assert reg.risk_for("organize_files") == "low"
    assert reg.risk_for("list_processes") == "low"
    assert reg.risk_for("kill_process") == "high"


def test_get_returns_full_entry():
    reg = registry_module.load()
    entry = reg.get("kill_process")
    assert entry["action"] == "kill_process"
    assert "params_schema" in entry
    assert "command_template" in entry


def test_get_raises_keyerror_for_unknown_action():
    reg = registry_module.load()
    with pytest.raises(KeyError):
        reg.get("nonexistent_action")
    with pytest.raises(KeyError):
        reg.risk_for("nonexistent_action")


def test_contains_and_len():
    reg = registry_module.load()
    assert "organize_files" in reg
    assert "nonexistent_action" not in reg
    assert len(reg) == 4


def test_params_schema_for_and_command_template_for():
    reg = registry_module.load()
    schema = reg.params_schema_for("organize_files")
    assert schema["type"] == "object"
    assert "target_dir" in schema["properties"]

    template = reg.command_template_for("organize_files")
    assert "{target_dir}" in template


def test_all_capabilities_preserves_file_order():
    reg = registry_module.load()
    actions_in_order = [c["action"] for c in reg.all_capabilities()]
    assert actions_in_order == reg.actions()


class TestCommandTemplatesActuallyRender:
    """
    Regression coverage for a real bug (found via manual end-to-end
    testing, post-Build-Order): organize_files' own command_template used
    bash's `${f##*.}` parameter-expansion syntax with its braces
    UNESCAPED for str.format() -- executor.render_command() calls
    `command_template.format(**quoted_params)`, and Python's str.format()
    parses ANY `{...}` in the template as a placeholder field, not just the
    ones this registry's own `{param}` convention intends. `${f##*.}` was
    therefore parsed as a format field named `f##*`, which is never a
    param, so render_command() raised KeyError on every single
    organize_files invocation -- the capability could never actually
    execute, despite passing intent parsing, harness validation, and plan
    generation cleanly (none of which ever calls render_command() to
    render the real, final command).

    No prior test in this suite (nor test_validation.py, test_plan_
    generator.py, test_intent_parser.py, or test_ui_panels.py, all of
    which reference organize_files) ever exercised the actual rendering
    step -- this class closes that gap for every REGISTERED capability,
    not just organize_files, so a future capability with the same kind of
    unescaped-brace mistake in its command_template fails a test
    immediately instead of only at real, live execution time.
    """

    # Best-effort placeholder values for a required param with no schema
    # default -- realistic-shaped strings for the params this registry's
    # four core capabilities actually declare as required-without-default
    # (organize_files' target_dir, kill_process' target). A future
    # capability adding a new required-without-default param of a kind not
    # listed here would fall back to the generic "placeholder" string,
    # which is enough to prove render_command() doesn't raise -- it does
    # not need to be a semantically perfect value for this test's purpose.
    _REQUIRED_PARAM_PLACEHOLDERS = {
        "target_dir": "/tmp/placeholder-dir",
        "target": "1234",
    }

    def _placeholder_params(self, reg, action):
        schema = reg.params_schema_for(action)
        params = {}
        for name, prop in schema.get("properties", {}).items():
            if "default" in prop:
                params[name] = prop["default"]
            elif name in schema.get("required", []):
                params[name] = self._REQUIRED_PARAM_PLACEHOLDERS.get(name, "placeholder")
        return params

    def test_every_registered_capability_command_template_renders(self):
        """
        For every capability currently in capabilities.json, build a
        plausible params dict (schema defaults where declared, a
        placeholder for anything required-without-default) and confirm
        executor.render_command() does not raise. This is the direct
        regression test for the organize_files bug -- it would have failed
        loudly (KeyError) against the pre-fix command_template.
        """
        from ohmyshell.executor import render_command

        reg = registry_module.load()
        for cap in reg.all_capabilities():
            action = cap["action"]
            params = self._placeholder_params(reg, action)
            try:
                render_command(cap["command_template"], params)
            except KeyError as exc:
                pytest.fail(
                    f"{action}'s command_template failed to render: {exc!r}. "
                    f"A literal '{{...}}' in the template (e.g. bash's own "
                    f"${{...}} parameter expansion) must be escaped as '{{{{...}}}}' "
                    f"so str.format() treats it as literal text, not a placeholder."
                )

    def test_organize_files_template_specifically_survives_bash_brace_syntax(self):
        """
        Narrower, explicit regression test naming the exact bug: bash's
        `${f##*.}` extension-stripping syntax must appear UNCHANGED (still
        real bash syntax, not swallowed or mangled) in the rendered
        command -- proving the fix escaped the template's braces for
        str.format() without altering what bash itself will actually run.
        """
        from ohmyshell.executor import render_command

        reg = registry_module.load()
        template = reg.command_template_for("organize_files")
        cmd = render_command(template, {"target_dir": "/tmp/mydir", "by": "extension"})
        assert "${f##*.}" in cmd
        assert "/tmp/mydir" in cmd


def test_plan_steps_for_returns_registered_templates():
    reg = registry_module.load()
    steps = reg.plan_steps_for("clean_temp_files")
    assert steps == [
        "Scan {paths} for files older than {days} days",
        "Calculate total reclaimable space",
        "Move matched files to .trash/ (recoverable)",
    ]


def test_plan_steps_for_returns_none_when_not_registered(tmp_path):
    """A capability with no plan_steps field must return None, not raise or default silently."""
    entry = _valid_entry()  # _valid_entry() below has no "plan_steps" key
    path = _write(tmp_path, {"version": "1.0", "capabilities": [entry]})
    reg = registry_module.load(path)

    assert reg.plan_steps_for("do_thing") is None


# --- Integrity-check failure cases -----------------------------------------


def _write(tmp_path, data):
    path = tmp_path / "capabilities.json"
    path.write_text(json.dumps(data), encoding="utf-8")
    return path


def _valid_entry(action="do_thing", risk="low"):
    return {
        "action": action,
        "description": "does a thing",
        "risk": risk,
        "params_schema": {"type": "object", "properties": {}},
        "command_template": "echo hi",
    }


def test_load_raises_for_missing_file(tmp_path):
    missing = tmp_path / "does_not_exist.json"
    with pytest.raises(registry_module.RegistryError):
        registry_module.load(missing)


def test_load_raises_for_invalid_json(tmp_path):
    path = tmp_path / "capabilities.json"
    path.write_text("{ not valid json", encoding="utf-8")
    with pytest.raises(registry_module.RegistryError):
        registry_module.load(path)


def test_load_raises_when_top_level_missing_required_keys(tmp_path):
    path = _write(tmp_path, {"capabilities": [_valid_entry()]})  # missing "version"
    with pytest.raises(registry_module.RegistryError):
        registry_module.load(path)


def test_load_raises_when_capabilities_list_is_empty(tmp_path):
    path = _write(tmp_path, {"version": "1.0", "capabilities": []})
    with pytest.raises(registry_module.RegistryError):
        registry_module.load(path)


def test_load_raises_when_entry_missing_required_field(tmp_path):
    entry = _valid_entry()
    del entry["command_template"]
    path = _write(tmp_path, {"version": "1.0", "capabilities": [entry]})
    with pytest.raises(registry_module.RegistryError):
        registry_module.load(path)


def test_load_raises_when_risk_is_not_a_known_level(tmp_path):
    entry = _valid_entry(risk="extreme")  # not in low/medium/high
    path = _write(tmp_path, {"version": "1.0", "capabilities": [entry]})
    with pytest.raises(registry_module.RegistryError):
        registry_module.load(path)


def test_load_raises_on_duplicate_action_names(tmp_path):
    path = _write(
        tmp_path,
        {
            "version": "1.0",
            "capabilities": [_valid_entry(action="dup"), _valid_entry(action="dup")],
        },
    )
    with pytest.raises(registry_module.RegistryError):
        registry_module.load(path)


def test_load_raises_when_params_schema_itself_is_invalid(tmp_path):
    entry = _valid_entry()
    # "type": "not-a-real-type" is not a valid JSON Schema type keyword value.
    entry["params_schema"] = {"type": "not-a-real-type"}
    path = _write(tmp_path, {"version": "1.0", "capabilities": [entry]})
    with pytest.raises(registry_module.RegistryError):
        registry_module.load(path)


def test_load_accepts_minimal_valid_registry(tmp_path):
    path = _write(tmp_path, {"version": "1.0", "capabilities": [_valid_entry()]})
    reg = registry_module.load(path)
    assert len(reg) == 1
    assert reg.risk_for("do_thing") == "low"