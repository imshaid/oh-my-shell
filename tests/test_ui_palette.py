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

    def test_commands_are_listed_alphabetically_by_name(self):
        """The person explicitly asked for lexicographic ordering (a person
        scanning the palette should be able to find a command by letter),
        distinct from /help's own HELP_TEXT, which keeps its own
        frequency-of-use ordering -- see the module docstring."""
        names = [name for name, _ in COMMANDS]
        assert names == sorted(names)

    def test_a_bare_slash_lists_commands_in_alphabetical_order(self):
        names = [name for name, _ in visible_commands("/")]
        assert names == sorted(names)


def _toolbar_plain_text(fragments: list[tuple[str, str]]) -> str:
    """Test helper: concatenate a render_toolbar_text() result's text
    pieces back into one plain string, the same way prompt_toolkit itself
    would when actually drawing the fragments -- lets most assertions
    below stay about *content* without caring about style boundaries."""
    return "".join(text for _style, text in fragments)


class TestRenderToolbarText:
    """
    Round 4 (post-Build-Order, "I want to add color in everywhere"):
    render_toolbar_text now returns a list of (style, text) tuples
    (prompt_toolkit's own AnyFormattedText shape) instead of a bare str,
    so the command name and description carry this app's own ACCENT/
    MUTED colors -- see the module docstring's "Round 4" note for the
    full reversal-of-Round-3 story.
    """

    def test_no_match_renders_empty_list(self):
        assert render_toolbar_text("clean up temp files") == []
        assert render_toolbar_text("") == []

    def test_matches_render_as_one_line_per_command(self):
        fragments = render_toolbar_text("/")
        text = _toolbar_plain_text(fragments)
        lines = text.split("\n")
        assert len(lines) == len(COMMANDS)

    def test_each_line_carries_its_command_name_and_description(self):
        fragments = render_toolbar_text("/mo")
        text = _toolbar_plain_text(fragments)
        assert "/model" in text
        assert "Show or switch the active model" in text

    def test_output_is_a_list_of_style_text_tuples(self):
        fragments = render_toolbar_text("/")
        assert isinstance(fragments, list)
        assert fragments  # non-empty for a bare "/"
        for style, text in fragments:
            assert isinstance(style, str)
            assert isinstance(text, str)

    def test_command_name_is_styled_with_this_apps_accent_color(self):
        """
        The whole point of Round 4: the command name must carry this
        app's own fixed ACCENT hex (ui/theme.py), not an empty/plain
        style -- matching how /help's own output colors the same piece.
        """
        from ohmyshell.ui.theme import ACCENT

        fragments = render_toolbar_text("/mo")
        name_styles = [style for style, text in fragments if "/model" in text]
        assert name_styles
        assert all(ACCENT in style for style in name_styles)

    def test_description_is_styled_with_this_apps_muted_color(self):
        from ohmyshell.ui.theme import MUTED

        fragments = render_toolbar_text("/mo")
        desc_styles = [
            style for style, text in fragments if "Show or switch the active model" in text
        ]
        assert desc_styles
        assert all(MUTED in style for style in desc_styles)


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

        # Regression fix (found via manual end-to-end testing): this test's
        # own `expected` set used to be a SECOND hand-maintained literal
        # list (mirroring meta_commands.py's dispatch() table "by eye",
        # exactly the drift risk this module's own docstring warns about
        # for COMMANDS itself) -- and that second copy had ALSO drifted: it
        # omitted "quit", so this "sync" test gave false confidence while
        # COMMANDS was actually missing a real, working command
        # (meta_commands.EXIT_COMMANDS = {"exit", "quit"}, both accepted by
        # dispatch()/main.py's own EXIT_COMMANDS, but only "exit" was ever
        # in the palette). Non-exit commands still aren't importable as
        # data from meta_commands.py (see this module's own docstring for
        # why), so those stay a literal list here, but the EXIT_COMMANDS
        # portion is now read directly off the real source instead of
        # retyped, so it can't drift out of sync with it again.
        # "update" is dispatchable too, just not through
        # meta_commands.dispatch() itself -- main.py intercepts it before
        # reaching that table, since it shells out to git/pip rather than
        # formatting another module's return value (see meta_commands.py's
        # own module docstring for why it's the one listed exception).
        expected_non_exit = {
            "help", "model", "history", "undo", "trash", "log",
            "capabilities", "explain", "stats", "system", "config",
            "clear", "update",
        }
        expected = expected_non_exit | meta_commands.EXIT_COMMANDS
        palette_names = {name for name, _ in COMMANDS}
        assert palette_names == expected
        assert meta_commands.EXIT_COMMANDS <= palette_names