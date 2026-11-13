#!/bin/sh
# Tests for scripts/install.sh (S29a). No network: the archive is made from a local binary and
# the download path talks to a throw-away HTTP server on 127.0.0.1.
#
#   scripts/test-install.sh [PATH-TO-oh-my-shell-BINARY]     (default: target/release/oh-my-shell)
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
binary=${1:-$root/target/release/oh-my-shell}
[ -x "$binary" ] || { echo "test-install.sh: no binary at $binary (cargo build --release -p omsh-cli)" >&2; exit 2; }
installer=$root/scripts/install.sh

case "$(uname -m)" in
    x86_64 | amd64) target=x86_64-unknown-linux-gnu ;;
    aarch64 | arm64) target=aarch64-unknown-linux-gnu ;;
    *) echo "test-install.sh: unsupported CPU" >&2; exit 2 ;;
esac

t=$(mktemp -d)
server_pid=""
cleanup() {
    [ -z "$server_pid" ] || kill "$server_pid" 2>/dev/null || true
    rm -rf "$t"
}
trap cleanup EXIT

[ -f "$root/THIRD-PARTY-LICENSES.html" ] || echo '<!doctype html><title>test</title>' > "$root/THIRD-PARTY-LICENSES.html"

failures=0
checks=0
ok() { checks=$((checks + 1)); printf 'ok   %s\n' "$1"; }
bad() { checks=$((checks + 1)); failures=$((failures + 1)); printf 'FAIL %s\n' "$1"; }
check() { # check "label" command...
    label=$1; shift
    if out=$("$@" 2>&1); then
        ok "$label"
    else
        bad "$label"
        [ -z "$out" ] || printf '%s\n' "$out" | sed 's/^/     | /'
    fi
}
refuses() { # refuses "label" "text the message must contain" command...
    label=$1; text=$2; shift 2
    if out=$("$@" 2>&1); then
        bad "$label (it succeeded)"
    elif printf '%s' "$out" | grep -q -- "$text"; then
        ok "$label"
    else
        bad "$label (message was: $out)"
    fi
}

# --- the release files -------------------------------------------------------------------
rel=$t/release
"$root/scripts/package-tarball.sh" "$target" "$binary" "$rel" >/dev/null
name=oh-my-shell-$target.tar.gz
(cd "$rel" && sha256sum "$name" > sha256.sum)
for member in oh-my-shell LICENSE README.md THIRD-PARTY-LICENSES.html; do
    check "the archive holds $member" sh -c "tar -tzf '$rel/$name' | grep -qx 'oh-my-shell-$target/$member'"
done

# --- a fresh install ------------------------------------------------------------------------
home=$t/home1; mkdir -p "$home"
bin=$home/.local/bin
HOME=$home sh "$installer" --archive "$rel/$name" --sums "$rel/sha256.sum" >"$t/out1" 2>&1 || { cat "$t/out1"; bad "fresh install ran"; }
check "fresh: the binary is installed and executable" test -x "$bin/oh-my-shell"
check "fresh: omsh is a relative link to oh-my-shell" test "$(readlink "$bin/omsh")" = oh-my-shell
check "fresh: the installed binary runs" sh -c "'$bin/oh-my-shell' --version | grep -q '^oh-my-shell '"
check "fresh: omsh runs through the link" sh -c "'$bin/omsh' --version | grep -q '^oh-my-shell '"
check "fresh: omsh ai status works through the link" sh -c "HOME='$home' DBUS_SESSION_BUS_ADDRESS=unix:path=/nonexistent '$bin/omsh' --no-config ai status | grep -q '^AI: '"
check "fresh: no temporary file is left in the bin directory" sh -c "test \"\$(ls -A '$bin' | wc -l)\" -eq 2"
check "fresh: it says how to list the shell and does not touch /etc/shells" grep -q "tee -a /etc/shells" "$t/out1"
check "fresh: it does not edit startup files" sh -c "test -z \"\$(ls -A '$home' | grep -v '^.local$')\""
check "fresh: it warns that the bin directory is not in PATH" grep -q "is not in your PATH" "$t/out1"

# --- upgrade over an existing install, a running copy keeps its file ----------------------------
before=$(stat -c %i "$bin/oh-my-shell")
HOME=$home sh "$installer" --archive "$rel/$name" --sums "$rel/sha256.sum" >"$t/out2" 2>&1 || bad "second install ran"
after=$(stat -c %i "$bin/oh-my-shell")
check "upgrade: the file was replaced by a rename (new inode)" test "$before" != "$after"
check "upgrade: omsh is still our link" test "$(readlink "$bin/omsh")" = oh-my-shell
check "upgrade: no warning about omsh" sh -c "! grep -q 'left as it is' '$t/out2'"

# --- a foreign omsh is kept ------------------------------------------------------------------------
home=$t/home2; mkdir -p "$home/.local/bin"
printf '#!/bin/sh\necho foreign\n' > "$home/.local/bin/omsh"; chmod 755 "$home/.local/bin/omsh"
HOME=$home sh "$installer" --archive "$rel/$name" --sums "$rel/sha256.sum" >"$t/out3" 2>&1 || bad "install next to a foreign omsh ran"
check "foreign omsh: it is kept" sh -c "'$home/.local/bin/omsh' | grep -q foreign"
check "foreign omsh: a warning says so" grep -q "left as it is" "$t/out3"
check "foreign omsh: oh-my-shell itself is installed" test -x "$home/.local/bin/oh-my-shell"

# --- refusals leave nothing behind ----------------------------------------------------------
home=$t/home3; mkdir -p "$home"
printf '%s  %s\n' "0000000000000000000000000000000000000000000000000000000000000000" "$name" > "$t/badsum"
refuses "a wrong checksum is refused" "checksum mismatch" env HOME="$home" sh "$installer" --archive "$rel/$name" --sums "$t/badsum"
check "wrong checksum: nothing was installed" test ! -e "$home/.local/bin/oh-my-shell"
printf '%s  %s\n' "$(sha256sum "$rel/$name" | cut -d' ' -f1)" "other.tar.gz" > "$t/othersum"
refuses "a sums file without the archive is refused" "does not list" env HOME="$home" sh "$installer" --archive "$rel/$name" --sums "$t/othersum"

# a binary that does not run on this system
mkdir -p "$t/broken/oh-my-shell-$target"
printf '#!/bin/sh\nexit 1\n' > "$t/broken/oh-my-shell-$target/oh-my-shell"
tar -C "$t/broken" -czf "$t/broken.tar.gz" "oh-my-shell-$target"
(cd "$t" && cp broken.tar.gz "$name" && sha256sum "$name" > broken.sum && rm "$name")
refuses "a binary that does not run is refused" "does not run" env HOME="$home" sh "$installer" --archive "$t/broken.tar.gz" --sums "$t/broken.sum"
check "broken binary: nothing was installed" test ! -e "$home/.local/bin/oh-my-shell"

# fake uname and getconf
fake=$t/fake; mkdir -p "$fake"
cat > "$fake/uname" <<'FAKE'
#!/bin/sh
case "$1" in -s) echo Linux ;; -m) echo "${FAKE_MACHINE:-x86_64}" ;; *) echo Linux ;; esac
FAKE
cat > "$fake/getconf" <<'FAKE'
#!/bin/sh
[ -n "${FAKE_GLIBC:-}" ] || exit 1
echo "$FAKE_GLIBC"
FAKE
chmod 755 "$fake/uname" "$fake/getconf"
refuses "an unsupported CPU is refused" "no release for the CPU" env PATH="$fake:$PATH" FAKE_MACHINE=riscv64 FAKE_GLIBC="glibc 2.39" HOME="$home" sh "$installer" --archive "$rel/$name" --sums "$rel/sha256.sum"
refuses "a system without glibc (musl) is refused with the way out" "cargo install --locked --git" env PATH="$fake:$PATH" HOME="$home" sh "$installer" --archive "$rel/$name" --sums "$rel/sha256.sum"
refuses "glibc 2.34 is too old" "glibc 2.35 or newer" env PATH="$fake:$PATH" FAKE_GLIBC="glibc 2.34" HOME="$home" sh "$installer" --archive "$rel/$name" --sums "$rel/sha256.sum"
# 2.35 and newer pass the glibc check (the function is tested directly)
glibc_probe() { OMSH_INSTALL_SOURCED=1 sh -c ". '$installer'; glibc_ok \"\$1\"" _ "$1"; }
check "glibc 2.35 is enough" glibc_probe "glibc 2.35"
check "glibc 2.39 is enough" glibc_probe "glibc 2.39"
check "glibc 3.0 is enough" glibc_probe "glibc 3.0"
check "glibc 2.9 is too old (numbers, not text)" sh -c "! OMSH_INSTALL_SOURCED=1 sh -c \". '$installer'; glibc_ok 'glibc 2.9'\""
check "an empty answer is not enough" sh -c "! OMSH_INSTALL_SOURCED=1 sh -c \". '$installer'; glibc_ok ''\""

# --- the tag in GitHub's JSON ----------------------------------------------------------------------
tag=$(OMSH_INSTALL_SOURCED=1 sh -c ". '$installer'; printf '%s' '{\"url\":\"x\",\"tag_name\": \"v1.2.3\",\"name\":\"y\"}' | tag_from_json")
check "the tag is read from the JSON" test "$tag" = v1.2.3

# --- the download path, against a local server -----------------------------------------------------
if command -v python3 >/dev/null 2>&1; then
    mkdir -p "$t/www"
    cp "$rel/$name" "$rel/sha256.sum" "$t/www/"
    python3 -u -m http.server 0 --bind 127.0.0.1 --directory "$t/www" >"$t/server.log" 2>&1 &
    server_pid=$!
    port=""
    i=0
    while [ -z "$port" ] && [ "$i" -lt 100 ]; do
        port=$(sed -n 's/.*port \([0-9]\{1,\}\).*/\1/p' "$t/server.log" | head -n 1)
        i=$((i + 1)); [ -n "$port" ] || sleep 0.1
    done
    if [ -n "$port" ] && { command -v curl >/dev/null 2>&1 || command -v wget >/dev/null 2>&1; }; then
        home=$t/home4; mkdir -p "$home"
        HOME=$home sh "$installer" --base-url "http://127.0.0.1:$port" >"$t/out4" 2>&1 || { cat "$t/out4"; bad "install from the local server ran"; }
        check "download: installed from the server and verified" sh -c "'$home/.local/bin/oh-my-shell' --version | grep -q '^oh-my-shell '"
        check "download: it said the checksum was ok" grep -q "Checksum ok" "$t/out4"
        # the server now serves a changed archive under the old checksum
        printf 'x' >> "$t/www/$name"
        home=$t/home5; mkdir -p "$home"
        refuses "download: a changed archive is refused" "checksum mismatch" env HOME="$home" sh "$installer" --base-url "http://127.0.0.1:$port"
        check "download: nothing was installed from the changed archive" test ! -e "$home/.local/bin/oh-my-shell"
    else
        echo "skip: no local server or no curl/wget; the download path was not tested"
    fi
else
    echo "skip: no python3; the download path was not tested"
fi
refuses "plain http to another host is refused" "https only" env HOME="$t/home6" sh "$installer" --base-url "http://example.com/dl"
refuses "an unknown option is refused" "unknown option" sh "$installer" --frobnicate
refuses "--archive without --sums is refused" "go together" sh "$installer" --archive /nonexistent
check "--help prints the usage and exits 0" sh -c "sh '$installer' --help | grep -q '^usage: install.sh'"

# --- syntax -----------------------------------------------------------------------------------
for s in "$installer" "$root/scripts/package-tarball.sh" "$0" "$root/packaging/deb/postinst" "$root/packaging/deb/postrm"; do
    if command -v dash >/dev/null 2>&1; then check "dash -n $(basename "$s")" dash -n "$s"; fi
    if command -v shellcheck >/dev/null 2>&1; then check "shellcheck $(basename "$s")" shellcheck -s sh "$s"; fi
done

printf '\n%s checks, %s failed\n' "$checks" "$failures"
[ "$failures" -eq 0 ]
