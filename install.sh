#!/usr/bin/env bash
#
# Oh My Shell — install script (Build Order Step 13, half of "wizard.py + install.sh").
#
# For a developer who already has this repo checked out: run this from the
# repo root (./install.sh). It creates a venv and installs the package into
# it (editable install) so the app can run.
#
# For a curl-pipe install (clones the repo first), see site/install.sh —
# that's the version hosted at https://oh-my-shell.pages.dev/install.sh.
#
# wizard.py (the Python module) handles first-run API key setup once the
# app itself can run; this script handles getting the app TO a runnable
# state in the first place:
#   1. check for a usable Python (>=3.11, per pyproject.toml's requires-python)
#   2. create a venv and install the package (editable install)
#   3. tell the user how to run it, and that the in-app wizard (wizard.py)
#      will handle API key setup on first launch.
#
# The UI below (color palette, section headers, spinners) mirrors the Nord
# accent family the app itself uses (see ui/theme.py's "omsh.*" styles), so
# the installer and the app share one visual identity. It degrades to plain
# text automatically when stdout isn't a terminal (piped to a log file, CI).

set -euo pipefail

# --- Colors (Nord-inspired, matches ui/theme.py's omsh.* accent family) -----
if [ -t 1 ]; then
    BOLD=$'\033[1m'
    RESET=$'\033[0m'
    ACCENT=$'\033[38;5;110m'   # omsh.accent  (nord blue)
    SUCCESS=$'\033[38;5;114m'  # omsh.success (nord green)
    WARNING=$'\033[38;5;222m'  # omsh.warning (nord yellow)
    DANGER=$'\033[38;5;168m'   # omsh.danger  (nord red)
    MUTED=$'\033[38;5;245m'    # omsh.muted   (gray)
    TTY=1
else
    BOLD=""; RESET=""; ACCENT=""; SUCCESS=""; WARNING=""; DANGER=""; MUTED=""
    TTY=0
fi

section() { printf '\n%s\n' "  ${BOLD}${ACCENT}$1${RESET}"; }
ok()      { printf '  %s%s%s %s\n' "$SUCCESS" "✓" "$RESET" "$1"; }
warn()    { printf '  %s%s%s %s\n' "$WARNING" "⚠" "$RESET" "$1"; }
fail()    { printf '  %s%s%s %s\n' "$DANGER" "✗" "$RESET" "$1"; exit 1; }

# Runs "$2" (a shell command string) in the background with a spinner next
# to label "$1", then prints a final ✓/✗ line in its place. Falls back to a
# plain "label... done" line when stdout isn't a terminal. The command's own
# stdout/stderr are captured and only shown on failure, so a normal install
# stays a clean, uncluttered ✓ list.
run_step() {
    local label="$1" cmd="$2" logfile
    logfile="$(mktemp)"

    if [ "$TTY" != "1" ]; then
        printf '  ...%s\n' "$label"
        if bash -c "$cmd" >"$logfile" 2>&1; then
            ok "$label"
        else
            fail "$label (see below)"$'\n'"$(cat "$logfile")"
        fi
        rm -f "$logfile"
        return
    fi

    bash -c "$cmd" >"$logfile" 2>&1 &
    local pid=$!
    local frames='⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏'
    local i=0
    while kill -0 "$pid" 2>/dev/null; do
        local frame="${frames:i%10:1}"
        printf '\r\033[K  %s%s%s %s' "$ACCENT" "$frame" "$RESET" "$label"
        i=$((i + 1))
        sleep 0.08
    done
    printf '\r\033[K'

    if wait "$pid"; then
        ok "$label"
    else
        fail "$label — output:"$'\n'"$(cat "$logfile")"
    fi
    rm -f "$logfile"
}

banner() {
    printf '\n%s\n' "  ${BOLD}${ACCENT}✦ Oh My Shell${RESET}  ${MUTED}— installer${RESET}"
    printf '%s\n' "  ${MUTED}────────────────────────────────────────${RESET}"
}

banner

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$REPO_ROOT"

# --- Environment --------------------------------------------------------------
section "Environment"

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
ok "Installing from $REPO_ROOT"

# --- Installing ------------------------------------------------------------------
section "Installing"

VENV_DIR="$REPO_ROOT/.venv"
if [ -d "$VENV_DIR" ]; then
    ok "Virtual environment already exists"
else
    run_step "Creating virtual environment" "'$PYTHON_BIN' -m venv '$VENV_DIR'"
fi

# shellcheck disable=SC1091
if [ -f "$VENV_DIR/bin/activate" ]; then
    source "$VENV_DIR/bin/activate"
else
    fail "Could not find $VENV_DIR/bin/activate — venv creation may have failed."
fi

run_step "Installing dependencies" "pip install --upgrade pip && pip install -e '$REPO_ROOT'"
chmod +x "$REPO_ROOT/bin/oh-my-shell"

# --- Done ------------------------------------------------------------------
echo
printf '  %s%s✓ Setup complete%s\n' "$BOLD" "$SUCCESS" "$RESET"
echo
printf '  %sTo start Oh My Shell:%s\n' "$MUTED" "$RESET"
printf '    source %s/bin/activate   (or .venv/bin/activate.fish for fish shell)\n' "$VENV_DIR"
printf '    oh-my-shell\n'
echo

section "Get your free API key"
printf '  %s1.%s Open %shttps://aistudio.google.com/apikey%s\n' "$MUTED" "$RESET" "$BOLD" "$RESET"
printf '  %s2.%s Sign in with a Google account\n' "$MUTED" "$RESET"
printf '  %s3.%s Click %s"Create API key"%s (no card required, free tier)\n' "$MUTED" "$RESET" "$BOLD" "$RESET"
printf '  %s4.%s Copy the key — Oh My Shell asks for it on first launch\n' "$MUTED" "$RESET"
echo