"""
Tests for router.py (Build Order Step 6) — raw/slash/natural-language classification.

`shutil.which` is mocked in most tests so classification is deterministic
regardless of what's actually installed on the machine running the suite.
"""

from unittest.mock import patch

from ohmyshell.router import InputKind, route


def _which_only(*known_executables):
    """Return a fake shutil.which that only recognizes the given names."""

    def fake_which(name):
        return f"/usr/bin/{name}" if name in known_executables else None

    return fake_which


# --- EMPTY ---------------------------------------------------------------------


def test_empty_string_is_empty():
    assert route("").kind == InputKind.EMPTY


def test_whitespace_only_is_empty():
    assert route("   \t  ").kind == InputKind.EMPTY


def test_empty_result_text_is_stripped():
    result = route("   ")
    assert result.text == ""


# --- SLASH_COMMAND ---------------------------------------------------------------


def test_slash_command_is_classified():
    assert route("/help").kind == InputKind.SLASH_COMMAND


def test_slash_command_with_args():
    assert route("/config set trash.retention_days 14").kind == InputKind.SLASH_COMMAND


def test_slash_command_takes_priority_over_shell_lookalike():
    """Even if what follows "/" resembles a path, a leading "/" always means slash command."""
    with patch("ohmyshell.router.shutil.which", _which_only()):
        assert route("/undo").kind == InputKind.SLASH_COMMAND


# --- RAW_SHELL: known executables on PATH ---------------------------------------


def test_known_executable_on_path_is_raw_shell():
    with patch("ohmyshell.router.shutil.which", _which_only("ls")):
        assert route("ls -la").kind == InputKind.RAW_SHELL


def test_unknown_first_word_not_on_path_is_not_raw_shell_by_that_rule_alone():
    with patch("ohmyshell.router.shutil.which", _which_only()):
        result = route("frobnicate the widget")
        assert result.kind == InputKind.NATURAL_LANGUAGE


# --- RAW_SHELL: builtins/keywords -------------------------------------------------


def test_cd_builtin_is_raw_shell_even_if_not_on_path():
    with patch("ohmyshell.router.shutil.which", _which_only()):
        assert route("cd Downloads").kind == InputKind.RAW_SHELL


def test_export_builtin_is_raw_shell():
    with patch("ohmyshell.router.shutil.which", _which_only()):
        assert route("export PATH=$PATH:/opt/bin").kind == InputKind.RAW_SHELL


# --- RAW_SHELL: shell-only punctuation --------------------------------------------


def test_pipe_marks_raw_shell():
    with patch("ohmyshell.router.shutil.which", _which_only()):
        assert route("cat file.txt | grep error").kind == InputKind.RAW_SHELL


def test_redirection_marks_raw_shell():
    with patch("ohmyshell.router.shutil.which", _which_only()):
        assert route("echo hello > out.txt").kind == InputKind.RAW_SHELL


def test_command_chaining_marks_raw_shell():
    with patch("ohmyshell.router.shutil.which", _which_only()):
        assert route("mkdir foo && cd foo").kind == InputKind.RAW_SHELL


def test_leading_dash_flag_marks_raw_shell():
    with patch("ohmyshell.router.shutil.which", _which_only()):
        assert route("-la").kind == InputKind.RAW_SHELL


def test_home_relative_path_marks_raw_shell():
    with patch("ohmyshell.router.shutil.which", _which_only()):
        assert route("~/projects/oh-my-shell").kind == InputKind.RAW_SHELL


# --- NATURAL_LANGUAGE: the default fallback ---------------------------------------


def test_plain_sentence_is_natural_language():
    with patch("ohmyshell.router.shutil.which", _which_only()):
        assert route("clean up temp files older than a week").kind == InputKind.NATURAL_LANGUAGE


def test_casual_phrasing_is_natural_language():
    with patch("ohmyshell.router.shutil.which", _which_only()):
        result = route("can you organize my downloads folder please")
        assert result.kind == InputKind.NATURAL_LANGUAGE


def test_question_is_natural_language():
    with patch("ohmyshell.router.shutil.which", _which_only()):
        assert route("what's using all my CPU right now").kind == InputKind.NATURAL_LANGUAGE


# --- text field always holds the stripped input -----------------------------------


def test_text_field_is_stripped_of_surrounding_whitespace():
    with patch("ohmyshell.router.shutil.which", _which_only()):
        result = route("  clean up temp files  ")
        assert result.text == "clean up temp files"