"""Tests for ui/prompt.py (Build Order Step 11)."""

from __future__ import annotations

from pathlib import Path

from ohmyshell import config as config_module
from ohmyshell.ui.prompt import (
    AI_ACTIVE_ICON,
    DEFAULT_ICON,
    render_prompt,
    render_prompt_plain,
)


def _cfg(**overrides) -> dict:
    cfg = config_module.default_config()
    for dotted_key, value in overrides.items():
        config_module.set_value(cfg, dotted_key, value)
    return cfg


class TestRenderPrompt:
    def test_includes_folder_name(self, tmp_path):
        text = render_prompt(_cfg(), cwd=tmp_path)
        assert tmp_path.name in text.plain

    def test_uses_default_icon_when_ai_not_active(self, tmp_path):
        text = render_prompt(_cfg(), ai_active=False, cwd=tmp_path)
        assert DEFAULT_ICON in text.plain
        assert AI_ACTIVE_ICON not in text.plain

    def test_uses_ai_active_icon_when_ai_active(self, tmp_path):
        text = render_prompt(_cfg(), ai_active=True, cwd=tmp_path)
        assert AI_ACTIVE_ICON in text.plain
        assert DEFAULT_ICON not in text.plain

    def test_omits_model_tag_for_default_model(self, tmp_path):
        cfg = _cfg()
        text = render_prompt(cfg, cwd=tmp_path)
        default_model = config_module.default_config()["model"]["active"]
        assert f"({default_model})" not in text.plain

    def test_includes_model_tag_for_non_default_model(self, tmp_path):
        cfg = _cfg(**{"model.active": "phi4-mini"})
        text = render_prompt(cfg, cwd=tmp_path)
        assert "(phi4-mini)" in text.plain

    def test_root_directory_falls_back_to_slash(self, tmp_path):
        # Path("/").name == "" — render_prompt should fall back to "/" like
        # main.py's own plain-text _render_prompt does.
        text = render_prompt(_cfg(), cwd=Path("/"))
        assert "/" in text.plain


class TestRenderPromptPlain:
    def test_returns_plain_string(self, tmp_path):
        result = render_prompt_plain(_cfg(), cwd=tmp_path)
        assert isinstance(result, str)
        assert tmp_path.name in result

    def test_matches_render_prompt_plain_attribute(self, tmp_path):
        cfg = _cfg()
        assert render_prompt_plain(cfg, cwd=tmp_path) == render_prompt(cfg, cwd=tmp_path).plain

    def test_no_rich_markup_leaks_into_plain_string(self, tmp_path):
        result = render_prompt_plain(_cfg(), ai_active=True, cwd=tmp_path)
        assert "[" not in result or "magenta" not in result  # no raw style tags