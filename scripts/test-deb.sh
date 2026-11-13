#!/bin/sh
# Installs the .deb, checks what it does, and removes it again (S29a). Needs root or sudo and
# apt (Debian, Ubuntu). It changes the machine: run it in CI or in a container.
#   scripts/test-deb.sh path/to/oh-my-shell_X.Y.Z-1_amd64.deb
set -eu
if [ $# -ne 1 ] || [ ! -f "$1" ]; then
    echo "usage: $0 FILE.deb" >&2
    exit 2
fi
deb=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
sudo=""
[ "$(id -u)" -eq 0 ] || sudo=sudo
export DEBIAN_FRONTEND=noninteractive

fail=0
ok() { printf 'ok   %s\n' "$1"; }
bad() { printf 'FAIL %s\n' "$1"; fail=1; }
check() { label=$1; shift; if "$@" >/dev/null 2>&1; then ok "$label"; else bad "$label"; fi; }

# apt installs the dependencies (ca-certificates) too.
$sudo apt-get install -y --no-install-recommends "$deb" >/dev/null
check "the binary is installed" test -x /usr/bin/oh-my-shell
check "the shell is listed in /etc/shells" grep -qx /usr/bin/oh-my-shell /etc/shells
check "omsh is a link to oh-my-shell" test "$(readlink /usr/bin/omsh)" = oh-my-shell
check "--version runs" sh -c "/usr/bin/oh-my-shell --version | grep -q '^oh-my-shell '"
check "omsh --version runs through the link" sh -c "/usr/bin/omsh --version | grep -q '^oh-my-shell '"
check "-c goes to /bin/sh" sh -c "test \"\$(/usr/bin/oh-my-shell -c 'echo ok')\" = ok"
check "omsh ai status works" sh -c "HOME=\$(mktemp -d) DBUS_SESSION_BUS_ADDRESS=unix:path=/nonexistent /usr/bin/omsh --no-config ai status | grep -q '^AI: '"
# dpkg lists the file even where the system drops /usr/share/doc (the Ubuntu container image does).
check "the license list is part of the package" sh -c "dpkg -L oh-my-shell | grep -qx /usr/share/doc/oh-my-shell/THIRD-PARTY-LICENSES.html"
check "ca-certificates is a dependency" sh -c "dpkg-deb -f '$deb' Depends | grep -q ca-certificates"
check "chsh was not run (no user has the shell)" sh -c "! getent passwd | cut -d: -f7 | grep -qx /usr/bin/oh-my-shell"

$sudo apt-get remove -y oh-my-shell >/dev/null
check "remove: the binary is gone" test ! -e /usr/bin/oh-my-shell
check "remove: the link is gone" test ! -L /usr/bin/omsh
check "remove: the line in /etc/shells is gone" sh -c "! grep -qx /usr/bin/oh-my-shell /etc/shells"

[ "$fail" -eq 0 ] && echo "deb: all checks passed"
exit "$fail"
