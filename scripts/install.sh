#!/bin/sh
# Installer for Oh My Shell (blueprint §18, S29a).
#
#   curl --proto '=https' --tlsv1.2 -fsSL \
#     https://github.com/imshaid/oh-my-shell/releases/latest/download/install.sh | sh
#
# What it does: downloads the release archive for this machine from GitHub, checks it against
# the release's sha256.sum, tries the new binary (--version), and puts it in ~/.local/bin (or
# /usr/local/bin with --system) with a short `omsh` link. What it never does: run chsh, edit
# your shell's startup files, or change /etc/shells unless you pass --add-to-shells.
#
# On a terminal it shows a header, a spinner for each step that settles into a mark, a real
# download line (percent and speed) and the measured start time of the new program. A pipe, a
# file or TERM=dumb gets one plain line for each step and no escape codes. Colors are the
# terminal's own 16 (no RGB, no 256-color index, no background); NO_COLOR removes them, a locale
# that is not UTF-8 gives ASCII symbols, OMSH_MOTION=off (or reduced; also CI and ssh) removes
# the spinner. It asks no question: with `curl | sh` standard input is the script itself.
#
# Options:
#   --version X.Y.Z     install that release instead of the latest
#   --system            install to /usr/local/bin (uses sudo when not root)
#   --bin-dir DIR       install to DIR
#   --add-to-shells     add the shell to /etc/shells (uses sudo when not root)
#   --archive FILE --sums FILE   use local files, no download (for tests)
#   --base-url URL      directory that holds the assets (https, or http on this computer)
#   -h, --help          this text
set -eu

REPO="imshaid/oh-my-shell"
MIN_GLIBC_MAJOR=2
MIN_GLIBC_MINOR=35

say() { printf '%s\n' "$*"; }
warn() { printf 'install.sh: %s\n' "$*" >&2; }
# A step that is still drawn (a spinner) is wiped first, so the message starts on a clean line.
die() { ui_abort; warn "$*"; exit 1; }

usage() {
    cat <<'TEXT'
usage: install.sh [options]

Installs Oh My Shell from the latest GitHub release into ~/.local/bin.

  --version X.Y.Z     install that release instead of the latest
  --system            install to /usr/local/bin (uses sudo when not root)
  --bin-dir DIR       install to DIR
  --add-to-shells     add the shell to /etc/shells (uses sudo when not root)
  --archive FILE --sums FILE   use local files, no download (for tests)
  --base-url URL      directory that holds the assets (https, or http on this computer)
  -h, --help          this text

It checks the download against the release's sha256.sum, runs the new binary once, and
makes an `omsh` link. It never runs chsh and never edits your shell's startup files.
TEXT
}

have() { command -v "$1" >/dev/null 2>&1; }

# The tag in the address that GitHub redirects `releases/latest` to:
# ".../releases/tag/v0.1.1" gives "v0.1.1". Nothing is printed for anything else (no release
# yet redirects to ".../releases"), or for a tag that is not a short plain word.
tag_from_location() {
    loc_text=$(printf '%s' "$1" | tr -d '\r\n')
    case "$loc_text" in
        */releases/tag/*) loc_tag=${loc_text##*/releases/tag/} ;;
        *) return 0 ;;
    esac
    case "$loc_tag" in
        '' | *[!A-Za-z0-9._+-]*) return 0 ;;
    esac
    [ "${#loc_tag}" -le 64 ] || return 0
    printf '%s\n' "$loc_tag"
}

# The web address releases come from: GitHub, or (for tests) a server on this computer named
# by OMSH_RELEASE_SERVER, the same hook `omsh update` has. Nothing else replaces GitHub.
release_web() {
    web_port=0
    web_value=${OMSH_RELEASE_SERVER:-}
    if [ -z "$web_value" ]; then
        printf '%s\n' "https://github.com"
        return 0
    fi
    web_value=${web_value%/}
    case "$web_value" in http://* | https://*) ;; *) return 1 ;; esac
    web_rest=${web_value#*://}
    # Only an address and a port: no path, no user name, nothing after the port.
    case "$web_rest" in */* | *@* | *\?* | *\#* | *\\* | *[!A-Za-z0-9.:\[\]]*) return 1 ;; esac
    case "$web_rest" in
        127.0.0.1 | localhost | \[::1\]) ;;
        127.0.0.1:* | localhost:*) web_port=${web_rest#*:} ;;
        \[::1\]:*) web_port=${web_rest#*\]:} ;;
        *) return 1 ;;
    esac
    case "${web_port-0}" in '' | *[!0-9]*) return 1 ;; esac
    printf '%s\n' "$web_value"
}

# "x86_64-unknown-linux-gnu" or "aarch64-unknown-linux-gnu"; empty when there is no release.
target_for() {
    case "$1" in
        x86_64 | amd64) echo x86_64-unknown-linux-gnu ;;
        aarch64 | arm64) echo aarch64-unknown-linux-gnu ;;
        *) echo "" ;;
    esac
}

# Succeeds when "glibc 2.39" (the output of getconf) is new enough.
glibc_ok() {
    # Own variable names (glibc_*): POSIX sh has no `local`, and main() keeps `version`.
    glibc_numbers=$(printf '%s' "$1" | sed -n 's/^glibc[[:space:]]\{1,\}\([0-9]\{1,\}\)\.\([0-9]\{1,\}\).*/\1 \2/p')
    [ -n "$glibc_numbers" ] || return 1
    glibc_major=${glibc_numbers% *}
    glibc_minor=${glibc_numbers#* }
    [ "$glibc_major" -gt "$MIN_GLIBC_MAJOR" ] && return 0
    [ "$glibc_major" -eq "$MIN_GLIBC_MAJOR" ] && [ "$glibc_minor" -ge "$MIN_GLIBC_MINOR" ]
}

# Refuses an address that is not https (plain http only for a server on this computer).
check_address() {
    case "$1" in
        https://* | http://127.0.0.1[:/]* | http://localhost[:/]* | http://\[::1\][:/]*) ;;
        *) die "refusing to download from $1 (https only)" ;;
    esac
}

fetch() { # fetch URL FILE
    check_address "$1"
    if have curl; then
        fetch_proto='=https'
        case "$1" in http://*) fetch_proto='=http' ;; esac
        curl --proto "$fetch_proto" --tlsv1.2 -fsSL -o "$2" "$1" || die "download failed: $1"
    elif have wget; then
        wget -q -O "$2" "$1" || die "download failed: $1"
    else
        die "needs curl or wget"
    fi
}

# Where URL ($1) redirects to, without following it. Prints nothing when it does not redirect.
# This is how the latest release is found: github.com/<repo>/releases/latest answers with a
# redirect to .../releases/tag/<tag>. The GitHub API is not used: it allows an address only 60
# requests an hour.
redirect_of() { # redirect_of URL
    check_address "$1"
    if have curl; then
        fetch_proto='=https'
        case "$1" in http://*) fetch_proto='=http' ;; esac
        curl --proto "$fetch_proto" --tlsv1.2 -fsS -o /dev/null -w '%{redirect_url}' "$1" \
            || die "cannot reach $1"
    elif have wget; then
        # wget stops at the redirect (exit status 8) and, with -S, prints the headers.
        wget_headers=$(wget --max-redirect=0 -S -O /dev/null "$1" 2>&1 || true)
        printf '%s\n' "$wget_headers" | sed -n 's/^[[:space:]]*[Ll]ocation:[[:space:]]*//p' | head -n 1
    else
        die "needs curl or wget"
    fi
}

# --- the display ------------------------------------------------------------------------------
# Variables of the display start with ui_ (POSIX sh has no `local`, and main() keeps its own).

# Decides, once, how things are shown. The display is "fancy" only when standard output is a
# terminal that is not dumb.
ui_init() {
    ui_fancy=0; ui_unicode=0; ui_motion=0
    ui_cursor_hidden=0; ui_spin_pid=""
    ui_green=""; ui_cyan=""; ui_dim=""; ui_off=""
    if [ -t 1 ] && [ -n "${TERM:-}" ] && [ "${TERM:-}" != dumb ]; then
        ui_fancy=1
    fi
    [ "$ui_fancy" -eq 1 ] || return 0
    ui_off=$(printf '\033[0m')
    ui_dim=$(printf '\033[2m')
    if [ -z "${NO_COLOR:-}" ]; then
        ui_green=$(printf '\033[32m')
        ui_cyan=$(printf '\033[36m')
    fi
    case "${LC_ALL:-${LC_CTYPE:-${LANG:-}}}" in
        *[Uu][Tt][Ff]-8* | *[Uu][Tt][Ff]8*) ui_unicode=1 ;;
    esac
    case "${OMSH_MOTION:-}" in
        on | full) ui_motion=1 ;;
        reduced | off) ui_motion=0 ;;
        *)
            # Not said: the same rule as the shell. No motion in CI, and less over ssh.
            ui_motion=1
            case "${CI:-}" in '' | 0 | false | no) ;; *) ui_motion=0 ;; esac
            [ -z "${SSH_CONNECTION:-}" ] || ui_motion=0
            ;;
    esac
}

# The mark of a finished step and of a failed one.
ui_mark() { # ui_mark ok|fail
    if [ "$ui_unicode" -eq 1 ]; then
        if [ "$1" = ok ]; then printf '%s' '✓'; else printf '%s' '✘'; fi
    else
        if [ "$1" = ok ]; then printf '%s' '+'; else printf '%s' 'x'; fi
    fi
}

# Centiseconds since boot, from /proc/uptime (this installer is for Linux). 0 if unreadable.
ui_cs() {
    read -r cs_up _ < /proc/uptime 2>/dev/null || { printf '0'; return 0; }
    cs_value="${cs_up%.*}${cs_up#*.}"
    while [ "${cs_value#0}" != "$cs_value" ]; do cs_value=${cs_value#0}; done
    printf '%s' "${cs_value:-0}"
}

# "1.9 MB", "512 kB" or "40 B", from a number of bytes.
ui_size() {
    if [ "$1" -ge 1000000 ]; then
        size_tenths=$(($1 / 100000))
        printf '%d.%d MB' $((size_tenths / 10)) $((size_tenths % 10))
    elif [ "$1" -ge 1000 ]; then
        printf '%d kB' $(($1 / 1000))
    else
        printf '%d B' "$1"
    fi
}

# "1.9 MB/s" from bytes per second.
ui_rate() { printf '%s/s' "$(ui_size "$1")"; }

# A bar of 14 cells for a percentage.
ui_bar() {
    bar_full=$(($1 * 14 / 100)); bar_i=0
    while [ "$bar_i" -lt 14 ]; do
        if [ "$bar_i" -lt "$bar_full" ]; then
            if [ "$ui_unicode" -eq 1 ]; then printf '%s' '█'; else printf '%s' '#'; fi
        else
            if [ "$ui_unicode" -eq 1 ]; then printf '%s' '░'; else printf '%s' '.'; fi
        fi
        bar_i=$((bar_i + 1))
    done
}

# One frame of the spinner.
ui_frame() {
    if [ "$ui_unicode" -eq 1 ]; then
        case $(($1 % 10)) in
            0) printf '%s' '⠋' ;; 1) printf '%s' '⠙' ;; 2) printf '%s' '⠹' ;; 3) printf '%s' '⠸' ;;
            4) printf '%s' '⠼' ;; 5) printf '%s' '⠴' ;; 6) printf '%s' '⠦' ;; 7) printf '%s' '⠧' ;;
            8) printf '%s' '⠇' ;; *) printf '%s' '⠏' ;;
        esac
    else
        case $(($1 % 4)) in
            0) printf '%s' '|' ;; 1) printf '%s' '/' ;; 2) printf '%s' '-' ;; *) printf '\134' ;;
        esac
    fi
}

# What the download has done so far: the bar, the percent and the speed when the server said
# how big the file is (its Content-Length, from the headers kept in $2), else the bytes so far
# and the speed. Nothing is guessed. $1 is the file being written, $3 the start time.
ui_progress_text() {
    [ -n "$1" ] || return 0
    pt_size=$({ wc -c < "$1"; } 2>/dev/null || echo 0)
    pt_size=$((pt_size + 0))
    pt_total=$(sed -n 's/^[[:space:]]*[Cc]ontent-[Ll]ength:[[:space:]]*\([0-9]\{1,\}\).*/\1/p' "$2" 2>/dev/null | tail -n 1)
    case "$pt_total" in ????????????*) pt_total="" ;; esac
    pt_cs=$(($(ui_cs) - $3))
    [ "$pt_cs" -gt 0 ] || pt_cs=1
    pt_rate=$(ui_rate $((pt_size * 100 / pt_cs)))
    if [ -n "$pt_total" ] && [ "$pt_total" -gt 0 ]; then
        pt_pct=$((pt_size * 100 / pt_total))
        [ "$pt_pct" -le 100 ] || pt_pct=100
        printf '%s %3d%%  %s' "$(ui_bar "$pt_pct")" "$pt_pct" "$pt_rate"
    else
        printf '%s  %s' "$(ui_size "$pt_size")" "$pt_rate"
    fi
}

# Runs in the background until it is killed: about ten frames a second, one write for each.
ui_spin_loop() { # ui_spin_loop LABEL [FILE HEADERS]
    spin_i=0
    spin_t0=$(ui_cs)
    while :; do
        spin_more=""
        [ -z "${2:-}" ] || spin_more="   $(ui_progress_text "$2" "$3" "$spin_t0")"
        printf '\r\033[2K  %s%s%s %s%s%s%s' "$ui_cyan" "$(ui_frame "$spin_i")" "$ui_off" "$1" "$ui_dim" "$spin_more" "$ui_off"
        spin_i=$((spin_i + 1))
        sleep 0.1 2>/dev/null || sleep 1
    done
}

# The cursor is hidden only while a spinner runs, and only after the traps that give it back.
ui_spin_start() { # ui_spin_start LABEL [FILE HEADERS]
    [ "$ui_motion" -eq 1 ] || return 0
    printf '\033[?25l'
    ui_cursor_hidden=1
    ui_spin_loop "$1" "${2:-}" "${3:-}" &
    ui_spin_pid=$!
}

# Stops the spinner, gives the cursor back and wipes the line.
ui_spin_stop() {
    if [ -n "${ui_spin_pid:-}" ]; then
        kill "$ui_spin_pid" 2>/dev/null || true
        wait "$ui_spin_pid" 2>/dev/null || true
        ui_spin_pid=""
        printf '\r\033[2K'
    fi
    if [ "${ui_cursor_hidden:-0}" -eq 1 ]; then
        printf '\033[?25h'
        ui_cursor_hidden=0
    fi
}

# Called by die: wipes a spinner that is still drawn.
ui_abort() {
    [ "${ui_fancy:-0}" -eq 1 ] || return 0
    ui_spin_stop
}

# A step begins. Fancy: the spinner. Plain: the line $2, if there is one.
ui_begin() { # ui_begin LABEL [PLAIN LINE]
    if [ "$ui_fancy" -eq 1 ]; then
        ui_spin_start "$1"
    elif [ -n "${2:-}" ]; then
        say "$2"
    fi
}

# A step is done. Fancy: the mark, the label and the detail. Plain: the line $3, if there is one.
ui_done() { # ui_done LABEL DETAIL [PLAIN LINE]
    if [ "$ui_fancy" -eq 1 ]; then
        ui_spin_stop
        printf '  %s%s%s %s%-12s%s %s\n' "$ui_green" "$(ui_mark ok)" "$ui_off" "$ui_dim" "$1" "$ui_off" "$2"
    elif [ -n "${3:-}" ]; then
        say "$3"
    fi
}

# The release file, with a progress line on a terminal.
fetch_progress() { # fetch_progress URL FILE LABEL
    check_address "$1"
    fp_head="$work/headers"
    : > "$fp_head"
    if have curl; then
        fp_proto='=https'
        case "$1" in http://*) fp_proto='=http' ;; esac
        curl --proto "$fp_proto" --tlsv1.2 -fsSL -D "$fp_head" -o "$2" "$1" &
    elif have wget; then
        wget -S -o "$fp_head" -O "$2" "$1" &
    else
        die "needs curl or wget"
    fi
    dl_pid=$!
    ui_spin_stop
    ui_spin_start "$3" "$2" "$fp_head"
    fp_started=$(ui_cs)
    fp_status=0
    wait "$dl_pid" || fp_status=$?
    dl_pid=""
    ui_spin_stop
    [ "$fp_status" -eq 0 ] || die "download failed: $1"
    fp_bytes=$({ wc -c < "$2"; } 2>/dev/null || echo 0)
    fp_bytes=$((fp_bytes + 0))
    fp_cs=$(($(ui_cs) - fp_started))
    [ "$fp_cs" -gt 0 ] || fp_cs=1
    fp_secs=$((fp_cs / 100)).$(((fp_cs % 100) / 10))
    ui_done "downloaded" "$(ui_size "$fp_bytes") in ${fp_secs} s ($(ui_rate $((fp_bytes * 100 / fp_cs))))"
}

# The mean time of `PROGRAM --version` over ten runs, in milliseconds with one decimal, measured
# here with `date +%s%N`. Prints nothing when that is not possible (no nanoseconds, or a run
# that fails). It includes the cost of starting the program from a shell script, so it reads a
# little higher than a benchmark does.
measure_start() { # measure_start PROGRAM
    ms_a=$(date +%s%N 2>/dev/null || true)
    case "$ms_a" in '' | *[!0-9]*) return 0 ;; esac
    ms_i=0
    while [ "$ms_i" -lt 10 ]; do
        "$1" --version >/dev/null 2>&1 || return 0
        ms_i=$((ms_i + 1))
    done
    ms_b=$(date +%s%N 2>/dev/null || true)
    case "$ms_b" in '' | *[!0-9]*) return 0 ;; esac
    ms_ns=$(((ms_b - ms_a) / 10))
    [ "$ms_ns" -gt 0 ] || return 0
    printf '%d.%d' $((ms_ns / 1000000)) $(((ms_ns % 1000000) / 100000))
}

# The line to add for the user's shell when the bin directory is not in PATH. It is only shown:
# no startup file is ever edited.
path_fix() { # path_fix DIR
    # Written with ~ or $HOME when the directory is under the home folder.
    pf_home=${HOME:-}
    pf_tilde=$1
    pf_var=$1
    if [ -n "$pf_home" ]; then
        # shellcheck disable=SC2088  # a literal ~ is wanted: it is text shown to the person
        case "$1" in
            "$pf_home"/*) pf_tilde='~/'"${1#"$pf_home"/}"; pf_var="\$HOME/${1#"$pf_home"/}" ;;
        esac
    fi
    case "${SHELL:-}" in
        */fish)
            say "  Add it for fish:"
            say "    fish_add_path $pf_tilde"
            ;;
        */zsh)
            say "  Add this line to ~/.zshrc:"
            say "    export PATH=\"$pf_var:\$PATH\""
            ;;
        */bash)
            say "  Add this line to ~/.bashrc:"
            say "    export PATH=\"$pf_var:\$PATH\""
            ;;
        *)
            say "  Add this line to your shell's startup file:"
            say "    export PATH=\"$pf_var:\$PATH\""
            ;;
    esac
    say "  (This script does not edit any startup file.)"
}

# The closing box: only text of ours, so it has a right edge. One line at a time on a terminal
# that allows motion, about 30 ms each.
ui_box() {
    box_edge_top='╭─'; box_edge_end='─╮'; box_side='│'; box_bottom_l='╰─'; box_bottom_r='─╯'; box_fill='─'
    if [ "$ui_unicode" -ne 1 ]; then
        box_edge_top='+-'; box_edge_end='-+'; box_side='|'; box_bottom_l='+-'; box_bottom_r='-+'; box_fill='-'
    fi
    # Every row is 62 columns: the edges (2 + 2), then 58 between them.
    printf '\n'
    box_pause
    printf '  %s%s Next %s%s%s\n' "$ui_dim" "$box_edge_top" "$(box_fill_n 52)" "$box_edge_end" "$ui_off"
    box_pause
    box_row "omsh" "try it now (your login shell is unchanged)"
    box_row "omsh tour" "a one-minute walk through what it does"
    box_row "omsh ai setup" "optional: connect an AI model"
    box_row "omsh update" "install a newer release, checked first"
    printf '  %s%s%s%s%s\n' "$ui_dim" "$box_bottom_l" "$(box_fill_n 58)" "$box_bottom_r" "$ui_off"
}

# N copies of the rule character (counted, because the shell may count bytes and not columns).
box_fill_n() {
    box_n=0
    while [ "$box_n" -lt "$1" ]; do
        printf '%s' "$box_fill"
        box_n=$((box_n + 1))
    done
}

box_pause() { [ "$ui_motion" -ne 1 ] || sleep 0.03 2>/dev/null || true; }

box_row() { # box_row COMMAND TEXT
    printf '  %s%s%s %s%-14s%s %-43s %s%s%s\n' "$ui_dim" "$box_side" "$ui_off" "$ui_cyan" "$1" "$ui_off" "$2" "$ui_dim" "$box_side" "$ui_off"
    box_pause
}

# Gives the cursor back, stops the helpers and removes the work folder, on every way out.
cleanup() {
    if [ -n "${ui_spin_pid:-}" ]; then
        kill "$ui_spin_pid" 2>/dev/null || true
        ui_spin_pid=""
        printf '\r\033[2K'
    fi
    if [ -n "${dl_pid:-}" ]; then kill "$dl_pid" 2>/dev/null || true; dl_pid=""; fi
    if [ "${ui_cursor_hidden:-0}" -eq 1 ]; then printf '\033[?25h'; ui_cursor_hidden=0; fi
    if [ -n "${work:-}" ]; then rm -rf "$work"; fi
}

# The checksum that sha256.sum ($1) lists for the file called $2.
wanted_sum() {
    awk -v name="$2" '{ file = $2; sub(/^\*/, "", file); if (file == name) { print $1; exit } }' "$1"
}

main() {
    version=""
    bin_dir=""
    system=0
    add_shells=0
    archive=""
    sums=""
    base_url=""
    work=""
    dl_pid=""
    ui_init
    # Before anything is drawn: every way out gives the cursor back and removes the work folder.
    trap cleanup EXIT
    trap 'cleanup; exit 129' HUP
    trap 'cleanup; exit 130' INT
    trap 'cleanup; exit 143' TERM
    while [ $# -gt 0 ]; do
        case "$1" in
            --version) [ $# -ge 2 ] || die "--version needs a value"; version=${2#v}; shift 2 ;;
            --system) system=1; shift ;;
            --bin-dir) [ $# -ge 2 ] || die "--bin-dir needs a value"; bin_dir=$2; shift 2 ;;
            --add-to-shells) add_shells=1; shift ;;
            --archive) [ $# -ge 2 ] || die "--archive needs a value"; archive=$2; shift 2 ;;
            --sums) [ $# -ge 2 ] || die "--sums needs a value"; sums=$2; shift 2 ;;
            --base-url) [ $# -ge 2 ] || die "--base-url needs a value"; base_url=${2%/}; shift 2 ;;
            -h | --help) usage; exit 0 ;;
            *) die "unknown option: $1 (try --help)" ;;
        esac
    done
    if { [ -n "$archive" ] && [ -z "$sums" ]; } || { [ -z "$archive" ] && [ -n "$sums" ]; }; then
        die "--archive and --sums go together"
    fi

    if [ "$ui_fancy" -eq 1 ]; then
        printf '\n  %sOh My Shell%s%sinstaller\n\n' "$ui_cyan" "$ui_off" "$(if [ "$ui_unicode" -eq 1 ]; then printf ' · '; else printf ' - '; fi)"
    fi

    [ "$(uname -s)" = Linux ] || die "Oh My Shell runs on Linux only"
    target=$(target_for "$(uname -m)")
    [ -n "$target" ] || die "no release for the CPU type $(uname -m) (x86_64 and aarch64 only)"
    libc=$(getconf GNU_LIBC_VERSION 2>/dev/null || true)
    if ! glibc_ok "$libc"; then
        die "this release needs glibc $MIN_GLIBC_MAJOR.$MIN_GLIBC_MINOR or newer (this system: ${libc:-not glibc}). Build from source instead: cargo install --locked --git https://github.com/$REPO omsh-cli"
    fi
    have sha256sum || die "needs sha256sum"
    have tar || die "needs tar"
    ui_done "platform" "linux $(uname -m), ${libc}"

    # Where the binary goes.
    sudo=""
    if [ -z "$bin_dir" ]; then
        if [ "$system" -eq 1 ]; then
            bin_dir=/usr/local/bin
        else
            [ -n "${HOME:-}" ] || die "HOME is not set"
            bin_dir=$HOME/.local/bin
        fi
    fi
    if [ "$(id -u)" -ne 0 ]; then
        mkdir -p "$bin_dir" 2>/dev/null || true
        if [ ! -w "$bin_dir" ]; then
            have sudo || die "$bin_dir is not writable and sudo is missing"
            sudo=sudo
        fi
    fi

    work=$(mktemp -d) || die "cannot make a temporary directory"

    name="oh-my-shell-$target.tar.gz"
    if [ -n "$archive" ]; then
        cp "$archive" "$work/$name"
        cp "$sums" "$work/sha256.sum"
        # The name in the sums file is the release name, whatever the local file is called.
        ui_done "files" "local archive, no download"
    else
        web=$(release_web) || die "OMSH_RELEASE_SERVER is set, but it is not a server on this computer (localhost, 127.0.0.1 or ::1); GitHub is not replaced"
        tag=""
        if [ -z "$base_url" ]; then
            if [ -z "$version" ]; then
                ui_begin "looking for the latest release" "Looking for the latest release..."
                location=$(redirect_of "$web/$REPO/releases/latest")
                tag=$(tag_from_location "$location")
                [ -n "$tag" ] || die "cannot read the latest release (is there one yet?)"
                ui_done "release" "$tag"
            else
                tag="v$version"
                ui_done "release" "$tag"
            fi
            base_url="$web/$REPO/releases/download/$tag"
        fi
        if [ "$ui_fancy" -eq 1 ]; then
            fetch_progress "$base_url/$name" "$work/$name" "downloading ${tag:-$name}"
        else
            say "Downloading $name from $base_url"
            fetch "$base_url/$name" "$work/$name"
        fi
        fetch "$base_url/sha256.sum" "$work/sha256.sum"
    fi

    want=$(wanted_sum "$work/sha256.sum" "$name")
    [ -n "$want" ] || die "sha256.sum does not list $name"
    have_sum=$(sha256sum "$work/$name" | awk '{ print $1 }')
    if [ "$want" != "$have_sum" ]; then
        die "checksum mismatch for $name (wanted $want, got $have_sum); nothing was installed"
    fi
    ui_done "checksum" "sha256 OK" "Checksum ok."

    tar -xzf "$work/$name" -C "$work" || die "cannot unpack $name"
    new="$work/oh-my-shell-$target/oh-my-shell"
    if [ ! -f "$new" ] || [ -L "$new" ]; then
        die "the archive does not hold oh-my-shell-$target/oh-my-shell"
    fi
    chmod 755 "$new"
    shown=$("$new" --version 2>&1) || die "the new binary does not run on this system: $shown"
    case "$shown" in
        oh-my-shell\ *) ;;
        *) die "the new binary says something unexpected: $shown" ;;
    esac

    # Copy next to the final name, then rename: a running shell keeps its old file.
    $sudo mkdir -p "$bin_dir"
    tmp="$bin_dir/.oh-my-shell.$$"
    $sudo cp "$new" "$tmp"
    $sudo chmod 755 "$tmp"
    $sudo mv -f "$tmp" "$bin_dir/oh-my-shell"
    ui_done "installed" "$bin_dir/oh-my-shell" "Installed $shown to $bin_dir/oh-my-shell"

    # The time the program takes to start, measured here and never guessed.
    started=$(measure_start "$bin_dir/oh-my-shell")
    if [ -n "$started" ]; then
        ui_done "verified" "starts in $started ms (mean of 10 runs)" "Verified: $shown starts in $started ms (mean of 10 runs, timed by this script)"
    else
        ui_done "verified" "$shown runs" "Verified: $shown runs"
    fi

    # The short name. A file called omsh that is not our link is somebody else's.
    link="$bin_dir/omsh"
    if [ ! -e "$link" ] && [ ! -L "$link" ]; then
        $sudo ln -s oh-my-shell "$link"
        ui_done "short name" "$link" "Made the short name: $link"
    elif [ "$(readlink "$link" 2>/dev/null || true)" = oh-my-shell ]; then
        :
    else
        warn "$link exists and is not a link to oh-my-shell; left as it is"
    fi

    shell_path="$bin_dir/oh-my-shell"
    if [ "$add_shells" -eq 1 ]; then
        if grep -qx "$shell_path" /etc/shells 2>/dev/null; then
            say "$shell_path is already in /etc/shells."
        else
            if [ "$(id -u)" -ne 0 ]; then
                have sudo || die "cannot add to /etc/shells without root or sudo"
                printf '%s\n' "$shell_path" | sudo tee -a /etc/shells >/dev/null
            else
                printf '%s\n' "$shell_path" >>/etc/shells
            fi
            say "Added $shell_path to /etc/shells."
        fi
    else
        say "To list it as a login shell (needed for chsh), run:"
        say "  echo $shell_path | sudo tee -a /etc/shells"
    fi

    on_path=1
    case ":${PATH:-}:" in
        *":$bin_dir:"*) ;;
        *)
            on_path=0
            say ""
            say "Note: $bin_dir is not in your PATH."
            path_fix "$bin_dir"
            ;;
    esac
    if [ "$ui_fancy" -eq 1 ]; then
        ui_box
        if [ "$on_path" -eq 0 ]; then
            say ""
            say "  Until it is in your PATH, start it with: $shell_path"
        fi
    else
        say ""
        say "Try it:  $shell_path"
        say "To use it, set the command of your terminal's profile to $shell_path first."
        say "Do not run chsh until you have used it for a while; this script never does."
    fi
}

if [ -z "${OMSH_INSTALL_SOURCED:-}" ]; then
    main "$@"
fi
