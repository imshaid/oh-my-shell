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

# --- the tag in the address `releases/latest` redirects to ----------------------------------------
tag_probe() { OMSH_INSTALL_SOURCED=1 sh -c ". '$installer'; tag_from_location \"\$1\"" _ "$1"; }
check "the tag is read from the redirect address" test "$(tag_probe 'https://github.com/imshaid/oh-my-shell/releases/tag/v1.2.3')" = v1.2.3
check "a pre-release tag is read" test "$(tag_probe 'https://github.com/o/r/releases/tag/v1.0.0-rc.1')" = v1.0.0-rc.1
check "a line ending (CR LF) is not part of the tag" test "$(tag_probe "$(printf 'https://github.com/o/r/releases/tag/v1.2.3\r\n')")" = v1.2.3
check "no release yet (redirect to /releases) gives no tag" test -z "$(tag_probe 'https://github.com/o/r/releases')"
check "a tag that could change the address gives no tag" test -z "$(tag_probe 'https://github.com/o/r/releases/tag/../../x')$(tag_probe 'https://github.com/o/r/releases/tag/v1/2')$(tag_probe 'https://github.com/o/r/releases/tag/v1%202')"
# The helpers must not change the variables main() keeps (v0.1.0 shipped with glibc_ok
# overwriting `version`, which made the download address "v2 36").
cat > "$t/probe-version.sh" <<'PROBE'
. "$1"
version=0.1.1
glibc_ok 'glibc 2.39'
test "$version" = 0.1.1
PROBE
OMSH_INSTALL_SOURCED=1 check "glibc_ok leaves version alone" sh "$t/probe-version.sh" "$installer"
cat > "$t/probe-vars.sh" <<'PROBE'
. "$1"
names="version bin_dir system add_shells archive sums base_url libc target name work tag web location"
for n in $names; do eval "$n=keep"; done
glibc_ok 'glibc 2.39'
glibc_ok 'glibc 2.9' || true
tag_from_location 'https://github.com/o/r/releases/tag/v1.2.3' >/dev/null
OMSH_RELEASE_SERVER=http://127.0.0.1:1 release_web >/dev/null
target_for x86_64 >/dev/null
for n in $names; do
    eval "value=\$$n"
    [ "$value" = keep ] || { echo "$n was changed to $value"; exit 1; }
done
PROBE
OMSH_INSTALL_SOURCED=1 check "no helper changes a variable that main() keeps" sh "$t/probe-vars.sh" "$installer"
release_web_probe() { OMSH_RELEASE_SERVER=$1 OMSH_INSTALL_SOURCED=1 sh -c ". '$installer'; release_web"; }
check "no hook: GitHub" test "$(OMSH_INSTALL_SOURCED=1 sh -c ". '$installer'; release_web")" = https://github.com
check "the hook accepts 127.0.0.1 with a port" test "$(release_web_probe http://127.0.0.1:8123/)" = http://127.0.0.1:8123
check "the hook accepts localhost and [::1]" sh -c "OMSH_RELEASE_SERVER=http://localhost:9 OMSH_INSTALL_SOURCED=1 sh -c \". '$installer'; release_web\" >/dev/null && OMSH_RELEASE_SERVER='http://[::1]:9' OMSH_INSTALL_SOURCED=1 sh -c \". '$installer'; release_web\" >/dev/null"
for bad_hook in http://example.com https://github.com.evil.example http://127.0.0.2 http://localhost.evil.example http://127.0.0.1@evil.example http://127.0.0.1:80@evil.example http://127.0.0.1:80/x ftp://127.0.0.1 localhost; do
    check "the hook refuses $bad_hook" sh -c "! OMSH_RELEASE_SERVER='$bad_hook' OMSH_INSTALL_SOURCED=1 sh -c \". '$installer'; release_web\" >/dev/null 2>&1"
done

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

# --- the PATH note: the exact line for the user's shell, and no startup file is touched -------------
path_note() { # path_note SHELL -> the output of an install whose bin directory is not in PATH
    pn_home=$t/home-path-$(basename "$1"); mkdir -p "$pn_home"
    env HOME="$pn_home" SHELL="$1" sh "$installer" --archive "$rel/$name" --sums "$rel/sha256.sum" 2>&1
    test -z "$(find "$pn_home" -mindepth 1 -maxdepth 1 ! -name .local)"
}
fish_out=$(path_note /usr/bin/fish) || bad "PATH note, fish: a startup file was made"
if printf '%s\n' "$fish_out" | grep -q '^    fish_add_path ~/.local/bin$'; then ok "PATH note, fish: fish_add_path ~/.local/bin"; else bad "PATH note, fish: $fish_out"; fi
bash_out=$(path_note /bin/bash) || bad "PATH note, bash: a startup file was made"
# shellcheck disable=SC2016  # the line is searched for as written, with a literal $HOME
if printf '%s\n' "$bash_out" | grep -q 'Add this line to ~/.bashrc:' && printf '%s\n' "$bash_out" | grep -qF '    export PATH="$HOME/.local/bin:$PATH"'; then ok "PATH note, bash: the export line and the file"; else bad "PATH note, bash: $bash_out"; fi
zsh_out=$(path_note /usr/bin/zsh) || bad "PATH note, zsh: a startup file was made"
if printf '%s\n' "$zsh_out" | grep -q 'Add this line to ~/.zshrc:'; then ok "PATH note, zsh: the file"; else bad "PATH note, zsh: $zsh_out"; fi
other_out=$(path_note /usr/bin/tcsh) || bad "PATH note, other: a startup file was made"
if printf '%s\n' "$other_out" | grep -q "startup file:"; then ok "PATH note, other shells: the generic line"; else bad "PATH note, other: $other_out"; fi
if printf '%s\n' "$fish_out" | grep -q "does not edit any startup file"; then ok "PATH note: it says no startup file is edited"; else bad "PATH note: no promise"; fi
# On PATH already: no note.
home=$t/home-onpath; mkdir -p "$home"
env HOME="$home" PATH="$home/.local/bin:$PATH" sh "$installer" --archive "$rel/$name" --sums "$rel/sha256.sum" >"$t/out-onpath" 2>&1
check "a bin directory that is in PATH gets no note" sh -c "! grep -q 'is not in your PATH' '$t/out-onpath'"
check "the plain form has a measured start time" grep -Eq 'Verified: oh-my-shell .* (starts in [0-9]+\.[0-9] ms|runs)' "$t/out-onpath"
check "the plain form has no escape code" sh -c "! grep -q \"\$(printf '\\033')\" '$t/out-onpath'"

# --- on a terminal (needs `script` from util-linux) ----------------------------------------------------
if command -v script >/dev/null 2>&1; then
    esc=$(printf '\033')
    on_tty() { # on_tty NAME "env assignments"  -> $t/tty-NAME
        tt_name=$1; tt_env=$2
        tt_home=$t/home-tty-$tt_name; mkdir -p "$tt_home"
        script -qec "env HOME='$tt_home' TERM=xterm-256color $tt_env sh '$installer' --archive '$rel/$name' --sums '$rel/sha256.sum'" /dev/null > "$t/tty-$tt_name" 2>&1 || true
    }
    # The text of a terminal run with the escape codes and the carriage returns taken out.
    flat() { sed 's/\x1b\[[0-9;?]*[a-zA-Z]//g' "$1" | tr -d '\r'; }
    on_tty plain "LANG=C.UTF-8"
    check "terminal: the header is drawn" grep -q "Oh My Shell" "$t/tty-plain"
    check "terminal: the steps settle into marks" grep -q "✓" "$t/tty-plain"
    check "terminal: the measured start time is shown" grep -Eq "starts in [0-9]+\.[0-9] ms|runs" "$t/tty-plain"
    check "terminal: the closing box has the four commands" sh -c "grep -q 'omsh tour' '$t/tty-plain' && grep -q 'omsh ai setup' '$t/tty-plain' && grep -q 'omsh update' '$t/tty-plain' && grep -q '╭─ Next' '$t/tty-plain'"
    check "terminal: only the 16 colors (no RGB, no 256-color code)" sh -c "! grep -q -E '${esc}\\[[0-9;]*[34]8;[25];' '$t/tty-plain'"
    check "terminal: no background color" sh -c "! grep -q -E '${esc}\\[(4[0-7]|10[0-7])m' '$t/tty-plain'"
    check "terminal: a hidden cursor is always given back" sh -c "hides=\$(grep -c '${esc}\\[?25l' '$t/tty-plain'); shows=\$(grep -c '${esc}\\[?25h' '$t/tty-plain'); [ \"\$hides\" -le \"\$shows\" ]"

    on_tty nocolor "LANG=C.UTF-8 NO_COLOR=1"
    check "NO_COLOR: no color code, the marks stay" sh -c "! grep -q -E '${esc}\\[3[0-7]m' '$t/tty-nocolor' && grep -q '✓' '$t/tty-nocolor'"

    on_tty ascii "LANG=C LC_ALL=C"
    check "a locale that is not UTF-8: ASCII only" sh -c "! LC_ALL=C grep -q -P '[^\\x00-\\x7F]' '$t/tty-ascii'"
    flat "$t/tty-ascii" > "$t/flat-ascii"
    check "ASCII: the box and the marks are ASCII" sh -c "grep -q '+- Next' '$t/flat-ascii' && grep -q '^  + platform' '$t/flat-ascii' && grep -q '^  | omsh tour' '$t/flat-ascii'"

    on_tty still "LANG=C.UTF-8 OMSH_MOTION=off"
    check "OMSH_MOTION=off: no spinner and the cursor is never hidden" sh -c "! grep -q '${esc}\\[?25l' '$t/tty-still' && grep -q '✓' '$t/tty-still'"
    on_tty dumb "LANG=C.UTF-8 TERM=dumb"
    check "TERM=dumb: plain lines, no escape" sh -c "! grep -q '${esc}' '$t/tty-dumb' && grep -q 'Checksum ok' '$t/tty-dumb'"

    # A download with a size: a throttled server sends the archive in pieces with a Content-Length.
    if command -v python3 >/dev/null 2>&1 && command -v curl >/dev/null 2>&1; then
        cat > "$t/slow.py" <<'PYTHON'
import http.server, os, sys, time
root, mode = sys.argv[1], sys.argv[2]
class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        path = os.path.join(root, self.path.lstrip('/'))
        if not os.path.isfile(path):
            self.send_response(404); self.end_headers(); return
        data = open(path, 'rb').read()
        self.send_response(200)
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        if len(data) < 100000:
            self.wfile.write(data); return
        step = max(1, len(data) // 20)
        for i in range(0, len(data), step):
            self.wfile.write(data[i:i + step]); self.wfile.flush()
            if mode == 'stall' and i >= step * 3:
                time.sleep(120)
            time.sleep(0.12)
    def log_message(self, *args): pass
server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
print('port', server.server_address[1], flush=True)
server.serve_forever()
PYTHON
        slow_server() { # slow_server MODE -> sets slow_port and slow_pid
            python3 -u "$t/slow.py" "$t/slowwww" "$1" > "$t/slow.log" 2>&1 &
            slow_pid=$!
            slow_port=""; si=0
            while [ -z "$slow_port" ] && [ "$si" -lt 100 ]; do
                slow_port=$(sed -n 's/^port \([0-9]\{1,\}\)$/\1/p' "$t/slow.log" | head -n 1)
                si=$((si + 1)); [ -n "$slow_port" ] || sleep 0.1
            done
        }
        mkdir -p "$t/slowwww"; cp "$rel/$name" "$rel/sha256.sum" "$t/slowwww/"
        slow_server go
        home=$t/home-slow; mkdir -p "$home"
        script -qec "env HOME='$home' TERM=xterm-256color LANG=C.UTF-8 sh '$installer' --base-url http://127.0.0.1:$slow_port" /dev/null > "$t/tty-slow" 2>&1 || true
        kill "$slow_pid" 2>/dev/null || true
        check "download on a terminal: a percent and a speed were drawn" grep -Eq '[0-9]+%  [0-9.]+ (MB|kB)/s' "$t/tty-slow"
        flat "$t/tty-slow" > "$t/flat-slow"
        check "download on a terminal: the finished line says size, time and speed" grep -Eq 'downloaded +[0-9.]+ (MB|kB) in [0-9.]+ s \([0-9.]+ (MB|kB)/s\)' "$t/flat-slow"
        check "download on a terminal: it installed" test -x "$home/.local/bin/oh-my-shell"
        [ -x "$home/.local/bin/oh-my-shell" ] || { echo "     | what the installer showed:"; tail -n 8 "$t/flat-slow" | sed 's/^/     | /'; }

        # Ctrl+C in the middle of a download: the cursor comes back and no work folder is left.
        # A job started with & in a script has SIGINT ignored, and an ignored signal cannot be
        # trapped, so the installer is started by a small runner that gives it the normal one.
        cat > "$t/intr.py" <<'PYTHON'
import os, signal, subprocess, sys, time
out, cmd = sys.argv[1], sys.argv[2]
with open(out, 'wb') as sink:
    proc = subprocess.Popen(['script', '-qec', cmd, '/dev/null'], stdout=sink, stderr=sink,
                            preexec_fn=lambda: signal.signal(signal.SIGINT, signal.SIG_DFL))
    end = time.time() + 15
    while time.time() < end and b'%' not in open(out, 'rb').read():
        time.sleep(0.1)
    # The installer: `sh .../install.sh ...`; the oldest one is the script, the others its helpers.
    def is_installer(pid):
        try:
            argv = open('/proc/%s/cmdline' % pid, 'rb').read().split(b'\0')
        except OSError:
            return False
        return len(argv) > 2 and os.path.basename(argv[0]) == b'sh' and os.path.basename(argv[1]) == b'install.sh'
    found = sorted(int(p) for p in os.listdir('/proc') if p.isdigit() and is_installer(p))
    if found:
        os.kill(found[0], signal.SIGINT)
    try:
        proc.wait(timeout=15)
        sys.exit(0 if found else 1)
    except subprocess.TimeoutExpired:
        proc.kill()
        sys.exit(1)
PYTHON
        slow_server stall
        home=$t/home-int; mkdir -p "$home" "$t/tmp-int"
        if python3 "$t/intr.py" "$t/tty-int" "env HOME='$home' TMPDIR='$t/tmp-int' TERM=xterm-256color LANG=C.UTF-8 sh '$installer' --base-url http://127.0.0.1:$slow_port"; then inst_pid=found; else inst_pid=""; fi
        kill "$slow_pid" 2>/dev/null || true
        check "Ctrl+C during the download: the installer stopped" test -n "$inst_pid"
        check "Ctrl+C during the download: the cursor was hidden and is shown again" sh -c "[ \"\$(grep -c '${esc}\\[?25l' '$t/tty-int')\" -ge 1 ] && [ \"\$(grep -c '${esc}\\[?25h' '$t/tty-int')\" -ge 1 ]"
        check "Ctrl+C during the download: no work folder is left" sh -c "test -z \"\$(ls -A '$t/tmp-int')\""
        check "Ctrl+C during the download: nothing was installed" test ! -e "$home/.local/bin/oh-my-shell"
    else
        echo "skip: no python3 or curl; the download display was not tested"
    fi
else
    echo "skip: no script(1); the display on a terminal was not tested"
fi

# --- syntax -----------------------------------------------------------------------------------
for s in "$installer" "$root/scripts/package-tarball.sh" "$0" "$root/packaging/deb/postinst" "$root/packaging/deb/postrm"; do
    if command -v dash >/dev/null 2>&1; then check "dash -n $(basename "$s")" dash -n "$s"; fi
    if command -v shellcheck >/dev/null 2>&1; then check "shellcheck $(basename "$s")" shellcheck -s sh "$s"; fi
done

printf '\n%s checks, %s failed\n' "$checks" "$failures"
[ "$failures" -eq 0 ]
