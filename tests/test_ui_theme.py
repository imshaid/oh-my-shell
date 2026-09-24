"""Tests for ui/theme.py -- OMSH_THEME, themed_console, bordered_console."""

from __future__ import annotations

import io

from ohmyshell.ui.theme import (
    OMSH_THEME,
    bordered_console,
    themed_console,
)


def _real(width: int = 60):
    buf = io.StringIO()
    console = themed_console(force_terminal=True, color_system="truecolor", no_color=False, file=buf, width=width)
    return buf, console


class TestOmshTheme:
    def test_semantic_style_names_are_registered(self):
        for name in (
            "omsh.accent",
            "omsh.accent_dim",
            "omsh.active",
            "omsh.success",
            "omsh.warning",
            "omsh.danger",
            "omsh.muted",
            "omsh.path",
            "omsh.risk.low",
            "omsh.risk.medium",
            "omsh.risk.high",
        ):
            assert name in OMSH_THEME.styles
            assert OMSH_THEME.styles[name].color is not None

    def test_no_bg_style_is_registered(self):
        """The fixed-full-window-background feature was tried across
        several revisions and explicitly abandoned by the user ("not
        solve, ok, give up it, just properly color the syntax based on
        the nord theme") -- there is no longer any "omsh.bg" style, and
        this app's chrome renders directly against the person's own
        terminal background."""
        assert "omsh.bg" not in OMSH_THEME.styles


class TestThemedConsole:
    def test_terminal_console_carries_no_forced_background(self):
        buf, real = _real()
        real.print("plain")
        assert "\x1b[48;2;" not in buf.getvalue()

    def test_non_terminal_console_gets_no_background(self):
        buf = io.StringIO()
        console = themed_console(file=buf, width=60, force_terminal=False)
        console.print("plain")
        assert "\x1b[48;2;" not in buf.getvalue()


class TestBorderedConsole:
    def test_printed_text_carries_no_background_escape(self):
        """Bordered-turn chrome no longer pins its own fixed background
        (see ui/theme.py's own module docstring, "THIRD major revision"):
        oh-my-shell's palette now maps onto the person's own Nord-themed
        terminal instead of fighting it with a competing painted
        background, so a bordered Console's output should carry no "48;2;"
        (background) SGR code at all -- only foreground ("38;2;") codes
        from OMSH_THEME's own colors."""
        buf, real = _real()
        bc = bordered_console(real)
        bc.print("hello world")
        rendered = buf.getvalue()
        assert "\x1b[48;2;" not in rendered

    def test_bordered_console_construction_does_not_mutate_the_real_console(self):
        buf, real = _real()
        real.print("plain")
        before = buf.getvalue()

        buf2, real2 = _real()
        bordered_console(real2)  # building the bordered console must not mutate `real2`
        real2.print("plain")
        after = buf2.getvalue()

        assert before == after
        assert "▏" not in after

    def test_bar_still_renders(self):
        buf, real = _real()
        bc = bordered_console(real)
        bc.print("hello")
        assert "▏" in buf.getvalue()

    def test_non_terminal_console_gets_no_background_escape(self):
        buf = io.StringIO()
        real = themed_console(file=buf, width=60, force_terminal=False)
        bc = bordered_console(real)
        bc.print("hello")
        assert "\x1b[48;2;" not in buf.getvalue()