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
#   1. download the latest GitHub Release's source tarball and verify its
#      SHA256 checksum against checksums.txt (also a release asset, built
#      by .github/workflows/release.yml — see that file's own comment for
#      why the checksum isn't hardcoded here) before extracting anything
#      (or use an existing local checkout via OMSH_REPO_DIR)
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
REPO_SLUG="imshaid/oh-my-shell"
API_BASE="https://api.github.com/repos/$REPO_SLUG"

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
    if ! command -v curl >/dev/null 2>&1; then
        fail "curl not found. Install curl first."
    fi
    if ! command -v sha256sum >/dev/null 2>&1; then
        fail "sha256sum not found (usually part of coreutils)."
    fi
    ok "git, curl, sha256sum found"
fi

# --- Fetching ------------------------------------------------------------------
section "Fetching"

# OMSH_REPO_DIR lets a developer point this script at an existing local
# checkout instead of downloading fresh (used for local testing of this
# script itself; a curl-pipe run always takes the download-and-verify path
# below).
if [ -n "${OMSH_REPO_DIR:-}" ]; then
    REPO_ROOT="$(cd "$OMSH_REPO_DIR" && pwd)"
    ok "Using existing checkout at $REPO_ROOT"
else
    INSTALL_DIR="${OMSH_INSTALL_DIR:-$HOME/.local/share/oh-my-shell}"

    # Already-installed-before recovery: if `git pull --ff-only` fails
    # specifically because local and remote history have diverged (e.g. a
    # maintainer force-pushed a rewritten history -- a rebase, or a
    # `git filter-repo` run to strip an accidentally-committed file), a
    # plain re-run of this installer used to leave the user stuck with a
    # raw git error and no obvious next step. Detected by the same
    # "Not possible to fast-forward" substring /update's own
    # _reclone()/run_self_update() (update_check.py) key off of, so both
    # recovery paths treat the same failure the same way. Falling through
    # to `rm -rf "$INSTALL_DIR"` here is safe -- it only discards the repo
    # checkout, never ~/.oh-my-shell/ (config.json + .env), so this is a
    # clean fresh install of the current release, not a data loss.
    if [ -d "$INSTALL_DIR/.git" ]; then
        PULL_LOG="$(mktemp)"
        if git -C "$INSTALL_DIR" pull --ff-only >"$PULL_LOG" 2>&1; then
            ok "Updating existing checkout"
            rm -f "$PULL_LOG"
            REPO_ROOT="$INSTALL_DIR"
        elif grep -q "Not possible to fast-forward" "$PULL_LOG"; then
            warn "Existing checkout's history has diverged from the latest release — reinstalling fresh."
            rm -f "$PULL_LOG"
            rm -rf "$INSTALL_DIR"
        else
            fail "Updating existing checkout (see below)"$'\n'"$(cat "$PULL_LOG")"
        fi
    fi

    # Reaching here with INSTALL_DIR/.git still absent means either this is
    # a genuine first install, or the diverged-history recovery above just
    # rm -rf'd a broken checkout -- both cases want the same fresh-download
    # path below. REPO_ROOT is already set and this block skipped entirely
    # when the ff-only pull above succeeded normally.
    if [ ! -d "$INSTALL_DIR/.git" ]; then
        WORK_DIR="$(mktemp -d)"
        trap 'rm -rf "$WORK_DIR"' EXIT

        TAG="$(curl -fsSL "$API_BASE/releases/latest" | "$PYTHON_BIN" -c 'import json,sys; print(json.load(sys.stdin)["tag_name"])')"
        if [ -z "$TAG" ]; then
            fail "Could not determine the latest release tag from GitHub."
        fi
        ok "Latest release: $TAG"

        TARBALL_URL="https://github.com/$REPO_SLUG/archive/refs/tags/$TAG.tar.gz"
        CHECKSUMS_URL="https://github.com/$REPO_SLUG/releases/download/$TAG/checksums.txt"

        run_step "Downloading source ($TAG)" \
            "curl -fsSL -o '$WORK_DIR/source.tar.gz' '$TARBALL_URL'"
        run_step "Downloading checksums.txt" \
            "curl -fsSL -o '$WORK_DIR/checksums.txt' '$CHECKSUMS_URL'"

        # checksums.txt (built by .github/workflows/release.yml) has the
        # form "<sha256>  source.tar.gz" -- match it against our download
        # by hash alone, since our local filename doesn't need to match.
        EXPECTED_SHA="$(awk '{print $1; exit}' "$WORK_DIR/checksums.txt")"
        ACTUAL_SHA="$(sha256sum "$WORK_DIR/source.tar.gz" | awk '{print $1}')"
        if [ -z "$EXPECTED_SHA" ] || [ "$EXPECTED_SHA" != "$ACTUAL_SHA" ]; then
            fail "Checksum mismatch -- download may be corrupted or tampered with. Expected $EXPECTED_SHA, got $ACTUAL_SHA."
        fi
        ok "Checksum verified"

        run_step "Extracting" "mkdir -p '$WORK_DIR/extracted' && tar -xzf '$WORK_DIR/source.tar.gz' -C '$WORK_DIR/extracted' --strip-components=1"

        rm -rf "$INSTALL_DIR"
        mkdir -p "$(dirname "$INSTALL_DIR")"
        mv "$WORK_DIR/extracted" "$INSTALL_DIR"

        # A plain tarball has no .git directory, which would break /update's
        # `git pull --ff-only` on every future run. Turning it into a real
        # clone (fetch the tag, point local `main` at it, then fetch+track
        # origin/main so a plain `git pull` knows what to fast-forward
        # against -- a shallow `fetch --depth 1 origin <tag>` alone does NOT
        # set that tracking info, which was confirmed by testing: without
        # this last step, /update's `git pull --ff-only` fails with "There
        # is no tracking information for the current branch") gives it real
        # git history to fast-forward from next time, at the cost of one
        # more network round trip during install -- a fair trade since
        # install only happens once.
        run_step "Setting up git for future updates" \
            "git -C '$INSTALL_DIR' init -q && git -C '$INSTALL_DIR' remote add origin '$REPO_URL' && git -C '$INSTALL_DIR' fetch --depth 1 origin '$TAG' -q && git -C '$INSTALL_DIR' checkout -q FETCH_HEAD -- . && git -C '$INSTALL_DIR' branch -q -f main FETCH_HEAD && git -C '$INSTALL_DIR' symbolic-ref HEAD refs/heads/main && git -C '$INSTALL_DIR' remote set-branches origin main && git -C '$INSTALL_DIR' fetch origin main -q && git -C '$INSTALL_DIR' branch -q --set-upstream-to=origin/main main"

        rm -rf "$WORK_DIR"
        trap - EXIT
        REPO_ROOT="$INSTALL_DIR"
    fi
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