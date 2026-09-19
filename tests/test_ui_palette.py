"""
Tests for ui/palette.py (Step 11 command-palette follow-up).

Drives OhMyShellCompleter directly with prompt_toolkit's own Document/
CompleteEvent types (the actual objects PromptSession would hand it at
runtime) rather than faking them, since both are simple, stable
prompt_toolkit value objects and using the real ones is the more honest
test of "does this Completer behave the way prompt_toolkit will actually
call it."
"""

from __future__ import annotations

from prompt_toolkit.completion import CompleteEvent
from prompt_toolkit.document import Document
from prompt_toolkit.styles import Style

from ohmyshell.ui.palette import COMMANDS, PALETTE_STYLE, OhMyShellCompleter


def _complete(text: str) -> list:
    completer = OhMyShellCompleter()
    document = Document(text, len(text))
    return list(completer.get_completions(document, CompleteEvent()))


class TestOhMyShellCompleter:
    def test_bare_slash_offers_every_command(self):
        completions = _complete("/")
        assert len(completions) == len(COMMANDS)
        assert {c.text for c in completions} == {name for name, _ in COMMANDS}

    def test_partial_command_filters_by_prefix(self):
        completions = _complete("/mo")
        assert [c.text for c in completions] == ["model"]

    def test_no_match_yields_no_completions(self):
        assert _complete("/zzzznotacommand") == []

    def test_completion_replaces_only_the_typed_portion(self):
        (completion,) = _complete("/mo")
        assert completion.start_position == -2  # len("mo")

    def test_completion_carries_display_and_description(self):
        (completion,) = _complete("/mo")
        # display/display_meta are FormattedText; str() renders their text.
        assert "/model" in str(completion.display)
        assert "model" in str(completion.display_meta).lower()

    def test_text_not_starting_with_slash_offers_nothing(self):
        assert _complete("clean up temp files") == []

    def test_slash_after_the_first_character_offers_nothing(self):
        """A "/" that isn't the start of the line (e.g. "what does a/b mean")
        must never trigger the palette -- only a line starting with "/" is a
        slash-command per router.py's own classification."""
        assert _complete("what does a/b mean") == []

    def test_space_after_command_name_stops_completion(self):
        """Once the user is past the command name and typing args/subcommands
        (e.g. "/model switch "), this completer offers nothing -- see the
        module docstring for why subcommand args aren't completed here."""
        assert _complete("/model switch ") == []

    def test_command_names_are_lowercase_and_unique(self):
        names = [name for name, _ in COMMANDS]
        assert names == [n.lower() for n in names]
        assert len(names) == len(set(names))

    def test_every_command_has_a_nonempty_description(self):
        for name, description in COMMANDS:
            assert description.strip(), f"{name} has an empty description"


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


class TestPaletteStyle:
    """
    Round 2 fix ("the ui too much ugly and not properly fit with the main
    shell ui"): the completion menu must be styled to match this app's own
    palette, not left on prompt_toolkit's stock grey defaults.
    """

    def test_is_a_style_instance(self):
        assert isinstance(PALETTE_STYLE, Style)

    def test_overrides_every_completion_menu_style_class(self):
        rules = dict(PALETTE_STYLE.style_rules)
        for class_name in (
            "completion-menu",
            "completion-menu.completion",
            "completion-menu.completion.current",
            "completion-menu.meta.completion",
            "completion-menu.meta.completion.current",
        ):
            assert class_name in rules, f"missing override for {class_name!r}"

    def test_does_not_use_prompt_toolkits_stock_grey_defaults(self):
        """
        prompt_toolkit's own built-in default is `bg:#bbbbbb #000000` for
        the menu and `bg:#999999 #000000` for the meta column (confirmed by
        reading prompt_toolkit/styles/defaults.py directly) -- this is the
        exact grey box the person flagged as not matching the shell's own
        cyan/dim look. Asserting our override isn't literally those same
        strings keeps this regression test honest about what actually
        changed, rather than just checking *something* is set.
        """
        rules = dict(PALETTE_STYLE.style_rules)
        assert rules["completion-menu"] != "bg:#bbbbbb #000000"
        assert rules["completion-menu.meta.completion"] != "bg:#999999 #000000"