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