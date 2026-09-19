#!/usr/bin/env bash
#
# Oh My Shell — install script (Build Order Step 13, half of "wizard.py + install.sh").
#
# Section 10.1's own line: "wizard.py + install.sh — first-run experience".
# wizard.py (the Python module) handles hardware detection and picking a
# starting model once the app itself can run; this script handles getting
# the app TO a runnable state in the first place — the step before
# wizard.py can even be imported:
#   1. check for a usable Python (>=3.11, per pyproject.toml's requires-python)
#   2. check for Ollama (this project's primary/local-first model runtime,
#      per the blueprint's own architecture decision — Google AI Studio API
#      is opt-in fallback only, never assumed or installed by this script)
#   3. create a venv and install the package (editable install, matching
#      the dev workflow already used throughout this project:
#      `pip install -e ".[dev]"`)
#   4. tell the user how to run it, and that the in-app wizard (wizard.py)
#      will handle model selection on first launch.
#
# This script deliberately does NOT install Ollama itself or pull any model
# — those are the user's own system-level choices (which model, how much
# disk/RAM to commit), matching wizard.py's own scope note that model
# recommendation only picks a name, never auto-downloads it silently.
#
# Scope note (Section 16 Rule 5): the exact expected end-user install UX
# (e.g. a one-liner `curl | bash`, distro package, etc.) was never specified
# anywhere retrieved from the blueprint — this script is the straightforward
# "clone the repo, run this script" version, consistent with how the whole
# project has been developed and delivered so far (the user copy-pasting
# files into a real git checkout).

set -euo pipefail

# --- Colors (fall back to no color if not a terminal) -----------------------
if [ -t 1 ]; then
    BOLD=$'\033[1m'
    GREEN=$'\033[32m'
    YELLOW=$'\033[33m'
    RED=$'\033[31m'
    RESET=$'\033[0m'
else
    BOLD=""; GREEN=""; YELLOW=""; RED=""; RESET=""
fi

info()  { printf '%s\n' "${BOLD}==>${RESET} $1"; }
ok()    { printf '%s\n' "${GREEN}✓${RESET} $1"; }
warn()  { printf '%s\n' "${YELLOW}⚠${RESET} $1"; }
fail()  { printf '%s\n' "${RED}✗${RESET} $1"; exit 1; }

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$REPO_ROOT"

info "Installing Oh My Shell from $REPO_ROOT"

# --- 1. Python version check --------------------------------------------------
PYTHON_BIN="${PYTHON_BIN:-python3}"
if ! command -v "$PYTHON_BIN" >/dev/null 2>&1; then
    fail "python3 not found. Install Python 3.11 or newer first."
fi

PY_VERSION="$("$PYTHON_BIN" -c 'import sys; print(f"{sys.version_info.major}.{sys.version_info.minor}")')"
PY_OK="$("$PYTHON_BIN" -c 'import sys; print(1 if sys.version_info >= (3, 11) else 0)')"
if [ "$PY_OK" != "1" ]; then
    fail "Python $PY_VERSION found, but Oh My Shell needs Python 3.11+."
fi
ok "Python $PY_VERSION found"

# --- 2. Ollama check (local-first primary runtime; never auto-installed) ----
if command -v ollama >/dev/null 2>&1; then
    ok "Ollama found ($(ollama --version 2>/dev/null || echo 'version unknown'))"
else
    warn "Ollama not found on PATH."
    warn "Oh My Shell's natural-language features need a local Ollama runtime."
    warn "Install it yourself from https://ollama.com, then re-run this script"
    warn "(or just start Oh My Shell later — raw shell commands work without it)."
fi

# --- 3. Virtual environment + editable install -------------------------------
VENV_DIR="$REPO_ROOT/.venv"
if [ -d "$VENV_DIR" ]; then
    ok "Virtual environment already exists at $VENV_DIR"
else
    info "Creating virtual environment at $VENV_DIR"
    "$PYTHON_BIN" -m venv "$VENV_DIR"
    ok "Virtual environment created"
fi

# shellcheck disable=SC1091
if [ -f "$VENV_DIR/bin/activate" ]; then
    source "$VENV_DIR/bin/activate"
else
    fail "Could not find $VENV_DIR/bin/activate — venv creation may have failed."
fi

info "Installing Oh My Shell (editable) and its dependencies"
pip install --upgrade pip >/dev/null
pip install -e "$REPO_ROOT" >/dev/null
ok "Oh My Shell installed"

chmod +x "$REPO_ROOT/bin/oh-my-shell"

# --- 4. Done ------------------------------------------------------------------
echo
ok "Setup complete."
echo
echo "  To start Oh My Shell:"
echo "    source $VENV_DIR/bin/activate   (or .venv/bin/activate.fish for fish shell)"
echo "    oh-my-shell"
echo
echo "  First launch will run a quick setup wizard to pick a starting model"
echo "  based on your hardware — you can change it any time with /model."