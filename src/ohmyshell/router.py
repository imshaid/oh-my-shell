"""
Input Router (Build Order Step 6).

Classifies one line of user input as raw shell syntax, a slash command, or
natural language (Section 4.1/4.2). This module only classifies — it does
not execute or interpret anything itself; main.py dispatches based on the
Kind this module returns.

Classification heuristic:

1. Empty input -> EMPTY (main.py just reprints the prompt).
2. Starts with "/" -> SLASH_COMMAND.
3. Otherwise, treat it as RAW_SHELL if it looks like shell syntax:
   - first word is a real executable found on PATH (via shutil.which), or
     a recognized shell builtin/keyword (cd, exit, export, etc.), OR
   - contains shell-only punctuation that natural language essentially
     never uses conversationally: a pipe, redirection, command
     separator/chaining, a leading dash flag, or an explicit path/glob.
4. Anything else -> NATURAL_LANGUAGE — this is deliberately the default
   fallback (a plain sentence like "clean up temp files" has no PATH
   executable named "clean" and no shell-only punctuation, so it lands
   here), matching Section 4.1's flow where natural language is the
   catch-all path into the Intent Parser.

This is a heuristic, not a full shell-grammar parser — false positives/
negatives are possible at the margins (e.g. a natural-language sentence
that happens to start with a real command name, like "ls of files I have
are messy" starting with "ls"). Precision here isn't safety-critical: a
raw command that's actually natural language will just fail with a normal
shell error ("ls: cannot access 'of'..."), and the user retypes it; nothing
destructive executes without going through this same router either way.
"""

from __future__ import annotations

import shutil
from dataclasses import dataclass
from enum import Enum, auto

SHELL_BUILTINS_AND_KEYWORDS = {
    "cd",
    "exit",
    "export",
    "alias",
    "unalias",
    "source",
    "unset",
    "echo",
    "pwd",
    "history",
    "jobs",
    "fg",
    "bg",
    "wait",
    "trap",
    "read",
    "eval",
    "exec",
    "set",
    "shift",
    "test",
    "true",
    "false",
    "type",
    "which",
    "if",
    "then",
    "else",
    "fi",
    "for",
    "while",
    "do",
    "done",
    "function",
}

# Punctuation/operators that show up in shell syntax but essentially never
# in a conversational natural-language request.
SHELL_ONLY_MARKERS = ("|", ">", "<", "&&", "||", ";", "$(", "`", "~/", "./", "../")


class InputKind(Enum):
    EMPTY = auto()
    SLASH_COMMAND = auto()
    RAW_SHELL = auto()
    NATURAL_LANGUAGE = auto()


@dataclass(frozen=True)
class RoutedInput:
    """The classification result, plus the original (stripped) text."""

    kind: InputKind
    text: str


def _looks_like_shell_syntax(text: str, first_word: str) -> bool:
    if first_word in SHELL_BUILTINS_AND_KEYWORDS:
        return True
    if first_word.startswith("-"):
        return True
    if any(marker in text for marker in SHELL_ONLY_MARKERS):
        return True
    if shutil.which(first_word) is not None:
        return True
    return False


def route(raw_input: str) -> RoutedInput:
    """
    Classify one line of input.

    Args:
        raw_input: exactly what the user typed for this prompt turn
            (before any bracketed-paste handling, which is a separate
            concern — Section 8.3.2 — not yet implemented in this step).

    Returns:
        RoutedInput with `text` stripped of leading/trailing whitespace
        (the classifiers below all work on the stripped form; callers
        should use `RoutedInput.text`, not `raw_input`, going forward).
    """
    text = raw_input.strip()

    if not text:
        return RoutedInput(kind=InputKind.EMPTY, text=text)

    if text.startswith("/"):
        return RoutedInput(kind=InputKind.SLASH_COMMAND, text=text)

    first_word = text.split(maxsplit=1)[0]
    if _looks_like_shell_syntax(text, first_word):
        return RoutedInput(kind=InputKind.RAW_SHELL, text=text)

    return RoutedInput(kind=InputKind.NATURAL_LANGUAGE, text=text)