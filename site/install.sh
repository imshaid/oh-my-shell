#!/usr/bin/env bash
#
# Oh My Shell — install script (Build Order Step 13, half of "wizard.py + install.sh").
#
# Usage:
#   curl -fsSL https://oh-my-shell.pages.dev/install.sh | bash
#
# wizard.py (the Python module) handles first-run API key setup once the
# app itself can run; this script handles getting the app TO a runnable
# state in the first place:
#   1. clone the repo (or use an existing local checkout via OMSH_REPO_DIR)
#   2. check for a usable Python (>=3.11, per pyproject.toml's requires-python)
#   3. create a venv and install the package (editable install)
#   4. symlink the entrypoint into /usr/local/bin and register it in
#      /etc/shells so it can be set as a login shell via chsh (Section 8.1)
#   5. tell the user how to run it, and that the in-app wizard (wizard.py)
#      will handle API key setup on first launch.

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

REPO_URL="https://github.com/imshaid/oh-my-shell.git"

# --- 0. Get the repo onto disk ------------------------------------------------
# OMSH_REPO_DIR lets a developer point this script at an existing local
# checkout instead of cloning fresh (used for local testing of this script
# itself; a curl-pipe run always takes the clone path).
if [ -n "${OMSH_REPO_DIR:-}" ]; then
    REPO_ROOT="$(cd "$OMSH_REPO_DIR" && pwd)"
    info "Using existing checkout at $REPO_ROOT"
else
    if ! command -v git >/dev/null 2>&1; then
        fail "git not found. Install git first."
    fi
    INSTALL_DIR="${OMSH_INSTALL_DIR:-$HOME/.local/share/oh-my-shell}"
    if [ -d "$INSTALL_DIR/.git" ]; then
        info "Updating existing checkout at $INSTALL_DIR"
        git -C "$INSTALL_DIR" pull --ff-only
    else
        info "Cloning Oh My Shell into $INSTALL_DIR"
        git clone --depth 1 "$REPO_URL" "$INSTALL_DIR"
    fi
    REPO_ROOT="$INSTALL_DIR"
fi
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

# --- 2. Virtual environment + editable install -------------------------------
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

# --- 3. Symlink into /usr/local/bin + register in /etc/shells ----------------
# Symlinked to the venv's own `oh-my-shell` entry point (pip-installed via
# pyproject.toml's [project.scripts]) rather than bin/oh-my-shell, so the
# system-wide command runs with the correct interpreter and dependencies
# without requiring the venv to be activated first. bin/oh-my-shell (which
# shells out to system `python3 -m ohmyshell.main`) stays the dev-workflow
# entrypoint for a checkout with the venv already active.
#
# A login shell must be listed in /etc/shells before `chsh -s` will accept
# it — this step is what makes that possible. Both edits need root, so they
# are skipped (with a clear message) when sudo isn't available or the user
# declines, rather than failing the whole install over an optional step.
BIN_TARGET="/usr/local/bin/oh-my-shell"
SHELL_ENTRY="$BIN_TARGET"

if command -v sudo >/dev/null 2>&1; then
    if sudo ln -sf "$VENV_DIR/bin/oh-my-shell" "$BIN_TARGET" 2>/dev/null; then
        ok "Linked $BIN_TARGET -> $VENV_DIR/bin/oh-my-shell"
        if ! grep -qxF "$SHELL_ENTRY" /etc/shells 2>/dev/null; then
            if echo "$SHELL_ENTRY" | sudo tee -a /etc/shells >/dev/null; then
                ok "Registered $SHELL_ENTRY in /etc/shells"
            else
                warn "Could not register $SHELL_ENTRY in /etc/shells — skip chsh, or add it manually."
            fi
        else
            ok "$SHELL_ENTRY already registered in /etc/shells"
        fi
    else
        warn "Could not create $BIN_TARGET (sudo declined or unavailable) — skipping system-wide symlink."
    fi
else
    warn "sudo not found — skipping /usr/local/bin symlink and /etc/shells registration."
fi

# --- 4. Done ------------------------------------------------------------------
echo
ok "Setup complete."
echo
echo "  To start Oh My Shell:"
echo "    source $VENV_DIR/bin/activate   (or .venv/bin/activate.fish for fish shell)"
echo "    oh-my-shell"
echo
if [ -e "$BIN_TARGET" ]; then
    echo "  Or, since it's on your PATH now:"
    echo "    oh-my-shell"
    echo
    echo "  To set it as your login shell: chsh -s $BIN_TARGET"
    echo
fi
echo "  First launch will run a quick setup wizard asking for your Google AI"
echo "  Studio API key — get one free at https://aistudio.google.com/apikey"