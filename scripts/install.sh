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
die() { warn "$*"; exit 1; }

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

    [ "$(uname -s)" = Linux ] || die "Oh My Shell runs on Linux only"
    target=$(target_for "$(uname -m)")
    [ -n "$target" ] || die "no release for the CPU type $(uname -m) (x86_64 and aarch64 only)"
    libc=$(getconf GNU_LIBC_VERSION 2>/dev/null || true)
    if ! glibc_ok "$libc"; then
        die "this release needs glibc $MIN_GLIBC_MAJOR.$MIN_GLIBC_MINOR or newer (this system: ${libc:-not glibc}). Build from source instead: cargo install --locked --git https://github.com/$REPO omsh-cli"
    fi
    have sha256sum || die "needs sha256sum"
    have tar || die "needs tar"

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
    # shellcheck disable=SC2064
    trap "rm -rf '$work'" EXIT HUP INT TERM

    name="oh-my-shell-$target.tar.gz"
    if [ -n "$archive" ]; then
        cp "$archive" "$work/$name"
        cp "$sums" "$work/sha256.sum"
        # The name in the sums file is the release name, whatever the local file is called.
    else
        web=$(release_web) || die "OMSH_RELEASE_SERVER is set, but it is not a server on this computer (localhost, 127.0.0.1 or ::1); GitHub is not replaced"
        if [ -z "$base_url" ]; then
            if [ -z "$version" ]; then
                say "Looking for the latest release..."
                location=$(redirect_of "$web/$REPO/releases/latest")
                tag=$(tag_from_location "$location")
                [ -n "$tag" ] || die "cannot read the latest release (is there one yet?)"
            else
                tag="v$version"
            fi
            base_url="$web/$REPO/releases/download/$tag"
        fi
        say "Downloading $name from $base_url"
        fetch "$base_url/$name" "$work/$name"
        fetch "$base_url/sha256.sum" "$work/sha256.sum"
    fi

    want=$(wanted_sum "$work/sha256.sum" "$name")
    [ -n "$want" ] || die "sha256.sum does not list $name"
    have_sum=$(sha256sum "$work/$name" | awk '{ print $1 }')
    if [ "$want" != "$have_sum" ]; then
        die "checksum mismatch for $name (wanted $want, got $have_sum); nothing was installed"
    fi
    say "Checksum ok."

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
    say "Installed $shown to $bin_dir/oh-my-shell"

    # The short name. A file called omsh that is not our link is somebody else's.
    link="$bin_dir/omsh"
    if [ ! -e "$link" ] && [ ! -L "$link" ]; then
        $sudo ln -s oh-my-shell "$link"
        say "Made the short name: $link"
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

    case ":${PATH:-}:" in
        *":$bin_dir:"*) ;;
        *) say "Note: $bin_dir is not in your PATH. Add it to your shell's startup file." ;;
    esac
    say ""
    say "Try it:  $shell_path"
    say "To use it, set the command of your terminal's profile to $shell_path first."
    say "Do not run chsh until you have used it for a while; this script never does."
}

if [ -z "${OMSH_INSTALL_SOURCED:-}" ]; then
    main "$@"
fi
