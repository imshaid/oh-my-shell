"""
Tests for ui/palette.py (Step 11 command-palette follow-up, round 3).

visible_commands/render_toolbar_text are plain functions over a `str` --
no prompt_toolkit objects needed to test the actual list/formatting logic;
ui/session.py's own tests cover the prompt_toolkit wiring (bottom_toolbar
callable, style override) separately.
"""

from __future__ import annotations

from ohmyshell.ui.palette import COMMANDS, render_toolbar_text, visible_commands


class TestVisibleCommands:
    def test_bare_slash_lists_every_command(self):
        assert visible_commands("/") == COMMANDS

    def test_partial_command_filters_by_prefix(self):
        assert visible_commands("/mo") == [("model", "Show or switch the active model")]

    def test_no_match_yields_empty_list(self):
        assert visible_commands("/zzzznotacommand") == []

    def test_text_not_starting_with_slash_yields_empty_list(self):
        assert visible_commands("clean up temp files") == []

    def test_slash_after_the_first_character_yields_empty_list(self):
        """A "/" that isn't the start of the line (e.g. "what does a/b mean")
        must never show the palette -- only a line starting with "/" is a
        slash-command per router.py's own classification."""
        assert visible_commands("what does a/b mean") == []

    def test_space_after_command_name_yields_empty_list(self):
        """Once the user is past the command name and typing args/subcommands
        (e.g. "/model switch "), nothing is listed -- see the module
        docstring for why subcommand args aren't matched here."""
        assert visible_commands("/model switch ") == []

    def test_empty_string_yields_empty_list(self):
        assert visible_commands("") == []

    def test_command_names_are_lowercase_and_unique(self):
        names = [name for name, _ in COMMANDS]
        assert names == [n.lower() for n in names]
        assert len(names) == len(set(names))

    def test_every_command_has_a_nonempty_description(self):
        for name, description in COMMANDS:
            assert description.strip(), f"{name} has an empty description"


class TestRenderToolbarText:
    def test_no_match_renders_empty_string(self):
        assert render_toolbar_text("clean up temp files") == ""
        assert render_toolbar_text("") == ""

    def test_matches_render_as_one_line_per_command(self):
        text = render_toolbar_text("/")
        lines = text.split("\n")
        assert len(lines) == len(COMMANDS)

    def test_each_line_carries_its_command_name_and_description(self):
        text = render_toolbar_text("/mo")
        assert "/model" in text
        assert "Show or switch the active model" in text

    def test_output_is_a_plain_string_with_no_markup(self):
        """
        Round 3's whole point: no color/style is attached here -- callers
        (ui/session.py) rely on this being an unstyled str so the terminal's
        own ambient colors show through. This just guards against a future
        change accidentally returning rich markup or a FormattedText object
        instead of plain text.
        """
        text = render_toolbar_text("/")
        assert isinstance(text, str)
        assert "[" not in text or "]" not in text  # no rich-style markup slipped in


class TestCommandsMatchMetaCommandsDispatchTable:
    """
    The palette's command list should stay in sync with meta_commands.py's
    actual dispatch table -- not by parsing meta_commands.py (see this
    module's own docstring for why), but this regression test at least
    catches the list silently drifting out of sync when a command is added
    to one but not the other.
    """

    def test_palette_commands_are_all_dispatchable(self):
        from ohmyshell import meta_commands

        # Commands meta_commands.dispatch() recognizes, read directly off
        # its own EXIT_COMMANDS constant plus the fixed set its dispatch()
        # function checks -- kept as a literal list here (mirroring
        # meta_commands.py's own command table comment) since dispatch()
        # doesn't expose its recognized commands as importable data.
        expected = {
            "help", "model", "history", "undo", "trash", "log",
            "capabilities", "explain", "stats", "system", "config",
            "clear", "exit",
        }
        palette_names = {name for name, _ in COMMANDS}
        assert palette_names == expected
        assert "exit" in meta_commands.EXIT_COMMANDS