"""Tests for ui/prompt.py (Build Order Step 11)."""

from __future__ import annotations

from pathlib import Path

from ohmyshell import config as config_module
from ohmyshell.ui.prompt import (
    AI_ACTIVE_ICON,
    DEFAULT_ICON,
    render_prompt,
    render_prompt_ansi,
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
        cfg = _cfg(**{"model.active": "gemini-3.1-flash-lite"})
        text = render_prompt(cfg, cwd=tmp_path)
        assert "(gemini-3.1-flash-lite)" in text.plain

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


class TestRenderPromptAnsi:
    """
    render_prompt_ansi (Step 11's prompt_toolkit bridge) must produce a
    string containing real ANSI escape sequences (so prompt_toolkit's
    ANSI() wrapper has something to parse), while still containing the
    same plain text content as render_prompt_plain -- the escapes decorate
    the text, they don't replace it.
    """

    def test_contains_ansi_escape_codes(self, tmp_path):
        result = render_prompt_ansi(_cfg(), cwd=tmp_path)
        assert "\x1b[" in result

    def test_plain_text_content_still_present(self, tmp_path):
        result = render_prompt_ansi(_cfg(), cwd=tmp_path)
        assert tmp_path.name in result
        assert DEFAULT_ICON in result

    def test_ai_active_icon_present_when_requested(self, tmp_path):
        result = render_prompt_ansi(_cfg(), ai_active=True, cwd=tmp_path)
        assert AI_ACTIVE_ICON in result

    def test_returns_a_string(self, tmp_path):
        result = render_prompt_ansi(_cfg(), cwd=tmp_path)
        assert isinstance(result, str)

    def test_folder_name_itself_carries_a_color_escape(self, tmp_path):
        """
        Regression test (post-Build-Order, found via real-terminal
        testing): a composite style STRING mixing a plain attribute with a
        ui/theme.py "omsh.*" theme name ("bold omsh.path", the folder
        name's original style) silently rendered completely unstyled --
        no escape codes at all -- while the icon right next to it (styled
        with a single "omsh.*" name, no composite) rendered its color
        fine. `test_contains_ansi_escape_codes` above didn't catch this:
        it only asserted SOME escape appeared anywhere in the string, and
        the icon's own escape was enough to satisfy that even with the
        folder name completely unstyled. This test anchors specifically
        to the folder name's own immediate neighborhood in the string, so
        a future regression of the same kind (a composite omsh.* style
        silently losing its escape codes) fails here even if some other
        part of the prompt still has color.
        """
        result = render_prompt_ansi(_cfg(), cwd=tmp_path)
        folder_index = result.index(tmp_path.name)
        # The real bug produced a plain string with NO escape byte
        # anywhere before the folder name; a correctly styled folder name
        # has its escape sequence immediately preceding it.
        assert "\x1b[" in result[:folder_index]