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

REPO_URL="https://github.com/imshaid/oh-my-shell.git"

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

if [ -n "${OMSH_REPO_DIR:-}" ]; then
    ok "git not required (using existing checkout)"
else
    if ! command -v git >/dev/null 2>&1; then
        fail "git not found. Install git first."
    fi
    ok "git found"
fi

# --- Fetching ------------------------------------------------------------------
section "Fetching"

# OMSH_REPO_DIR lets a developer point this script at an existing local
# checkout instead of cloning fresh (used for local testing of this script
# itself; a curl-pipe run always takes the clone path).
if [ -n "${OMSH_REPO_DIR:-}" ]; then
    REPO_ROOT="$(cd "$OMSH_REPO_DIR" && pwd)"
    ok "Using existing checkout at $REPO_ROOT"
else
    INSTALL_DIR="${OMSH_INSTALL_DIR:-$HOME/.local/share/oh-my-shell}"
    if [ -d "$INSTALL_DIR/.git" ]; then
        run_step "Updating existing checkout" "git -C '$INSTALL_DIR' pull --ff-only"
    else
        run_step "Cloning imshaid/oh-my-shell" "git clone --depth 1 '$REPO_URL' '$INSTALL_DIR'"
    fi
    REPO_ROOT="$INSTALL_DIR"
fi
cd "$REPO_ROOT"

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

# --- Linking ------------------------------------------------------------------
section "Linking"

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
        ok "Linked $BIN_TARGET"
        if ! grep -qxF "$SHELL_ENTRY" /etc/shells 2>/dev/null; then
            if echo "$SHELL_ENTRY" | sudo tee -a /etc/shells >/dev/null; then
                ok "Registered in /etc/shells"
            else
                warn "Could not register $SHELL_ENTRY in /etc/shells — skip chsh, or add it manually."
            fi
        else
            ok "Already registered in /etc/shells"
        fi
    else
        warn "Could not create $BIN_TARGET (sudo declined or unavailable) — skipping system-wide symlink."
    fi
else
    warn "sudo not found — skipping /usr/local/bin symlink and /etc/shells registration."
fi

# --- Done ------------------------------------------------------------------
echo
printf '  %s%s✓ Setup complete%s\n' "$BOLD" "$SUCCESS" "$RESET"
echo
if [ -e "$BIN_TARGET" ]; then
    printf '  %sRun%s  %soh-my-shell%s  %sto start.%s\n' "$MUTED" "$RESET" "$BOLD" "$RESET" "$MUTED" "$RESET"
    printf '  %sTo set it as your login shell:%s chsh -s %s\n' "$MUTED" "$RESET" "$BIN_TARGET"
else
    printf '  %sTo start:%s\n' "$MUTED" "$RESET"
    printf '    source %s/bin/activate   (or .venv/bin/activate.fish for fish shell)\n' "$VENV_DIR"
    printf '    oh-my-shell\n'
fi
section "Get your free API key"
printf '  %s1.%s Open %shttps://aistudio.google.com/apikey%s\n' "$MUTED" "$RESET" "$BOLD" "$RESET"
printf '  %s2.%s Sign in with a Google account\n' "$MUTED" "$RESET"
printf '  %s3.%s Click %s\"Create API key\"%s (no card required, free tier)\n' "$MUTED" "$RESET" "$BOLD" "$RESET"
printf '  %s4.%s Copy the key — Oh My Shell asks for it on first launch\n' "$MUTED" "$RESET"
echo