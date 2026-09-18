"""Tests for config.py (Build Order Step 2) — load/save, defaults, dot-notation get/set."""

import json

import pytest

from ohmyshell import config as config_module


@pytest.fixture
def isolated_config(tmp_path, monkeypatch):
    """Point CONFIG_DIR/CONFIG_PATH at a temp dir so tests never touch the real ~/.oh-my-shell/."""
    fake_dir = tmp_path / ".oh-my-shell"
    fake_path = fake_dir / "config.json"
    monkeypatch.setattr(config_module, "CONFIG_DIR", fake_dir)
    monkeypatch.setattr(config_module, "CONFIG_PATH", fake_path)
    return fake_dir, fake_path


def test_default_config_matches_blueprint_shape():
    defaults = config_module.default_config()
    assert defaults["model"]["active"] == "qwen3:8b"
    assert defaults["model"]["available"] == [
        "qwen3:8b",
        "qwen3.5:4b",
        "phi4-mini",
        "lfm2.5-8b-a1b",
    ]
    assert defaults["safety"]["safe_mode"] is False
    assert defaults["safety"]["danger_classifier_sensitivity"] == "normal"
    assert defaults["trash"]["retention_days"] == 8
    assert defaults["ui"]["quiet"] is False
    assert defaults["ui"]["verbose"] is False
    assert defaults["log"]["level"] == "normal"
    assert defaults["discussion"]["soft_limit_turns"] == 5


def test_default_config_returns_independent_copies():
    a = config_module.default_config()
    b = config_module.default_config()
    a["trash"]["retention_days"] = 999
    assert b["trash"]["retention_days"] == 8


def test_load_creates_file_with_defaults_on_first_run(isolated_config):
    fake_dir, fake_path = isolated_config
    assert not fake_path.exists()

    loaded = config_module.load()

    assert fake_path.exists()
    assert loaded == config_module.default_config()
    on_disk = json.loads(fake_path.read_text(encoding="utf-8"))
    assert on_disk == config_module.default_config()


def test_load_reads_existing_file(isolated_config):
    fake_dir, fake_path = isolated_config
    fake_dir.mkdir(parents=True)
    custom = config_module.default_config()
    custom["trash"]["retention_days"] = 30
    fake_path.write_text(json.dumps(custom), encoding="utf-8")

    loaded = config_module.load()

    assert loaded["trash"]["retention_days"] == 30


def test_load_fills_in_missing_keys_from_older_config(isolated_config):
    fake_dir, fake_path = isolated_config
    fake_dir.mkdir(parents=True)
    # Simulate an older config.json missing a newer top-level section entirely,
    # and missing one nested key inside an existing section.
    partial = {
        "model": {"active": "qwen3:8b", "available": ["qwen3:8b"]},
        "safety": {"safe_mode": True},  # missing danger_classifier_sensitivity
        # trash, ui, log, discussion sections entirely absent
    }
    fake_path.write_text(json.dumps(partial), encoding="utf-8")

    loaded = config_module.load()

    assert loaded["safety"]["safe_mode"] is True  # preserved
    assert loaded["safety"]["danger_classifier_sensitivity"] == "normal"  # filled in
    assert loaded["trash"]["retention_days"] == 8  # whole section filled in
    assert loaded["discussion"]["soft_limit_turns"] == 5


def test_load_raises_on_invalid_json(isolated_config):
    fake_dir, fake_path = isolated_config
    fake_dir.mkdir(parents=True)
    fake_path.write_text("{ not valid json", encoding="utf-8")

    with pytest.raises(config_module.ConfigError):
        config_module.load()


def test_load_raises_when_top_level_is_not_an_object(isolated_config):
    fake_dir, fake_path = isolated_config
    fake_dir.mkdir(parents=True)
    fake_path.write_text(json.dumps([1, 2, 3]), encoding="utf-8")

    with pytest.raises(config_module.ConfigError):
        config_module.load()


def test_save_writes_readable_json(isolated_config):
    fake_dir, fake_path = isolated_config
    cfg = config_module.default_config()
    cfg["ui"]["verbose"] = True

    config_module.save(cfg)

    assert fake_path.exists()
    assert json.loads(fake_path.read_text(encoding="utf-8"))["ui"]["verbose"] is True


def test_get_reads_nested_dot_key():
    cfg = config_module.default_config()
    assert config_module.get(cfg, "trash.retention_days") == 8
    assert config_module.get(cfg, "model.active") == "qwen3:8b"


def test_get_raises_keyerror_for_missing_path():
    cfg = config_module.default_config()
    with pytest.raises(KeyError):
        config_module.get(cfg, "trash.nonexistent")
    with pytest.raises(KeyError):
        config_module.get(cfg, "nonexistent.section")


def test_set_value_updates_nested_dot_key():
    cfg = config_module.default_config()
    config_module.set_value(cfg, "trash.retention_days", 14)
    assert cfg["trash"]["retention_days"] == 14
    # example straight from Section 5.5's /config set illustration
    config_module.set_value(cfg, "trash.retention_days", 14)
    assert config_module.get(cfg, "trash.retention_days") == 14


def test_set_value_raises_keyerror_for_missing_intermediate_path():
    cfg = config_module.default_config()
    with pytest.raises(KeyError):
        config_module.set_value(cfg, "nonexistent.key", 1)