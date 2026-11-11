#!/bin/sh
# Gate C (blueprint section 21): run the whole test suite with the network switched off.
#
# Run it as your normal user, not with sudo. It asks for sudo once, only to make a network
# namespace that has just a loopback interface (`unshare --net`), and then drops back to your own
# user and group ids before any test runs, so nothing in target/ becomes owned by root.
#
# Three phases, all in this one file:
#   1. (no argument)            you: build the tests with the network on, then call sudo
#   2. --in-namespace UID GID   root, in the empty network namespace: bring loopback up, drop
#                               to UID and GID
#   3. --tests UID GID          you again, offline: prove the network is off, run the tests

set -eu

die() {
    echo "test-offline: $*" >&2
    exit 1
}

phase=${1:-outer}

case "$phase" in
outer)
    [ "$(id -u)" -ne 0 ] || die "run this as your normal user, not as root or with sudo"
    for tool in sudo unshare setpriv ip cargo bash getent timeout; do
        command -v "$tool" >/dev/null 2>&1 || die "$tool is needed and was not found"
    done
    script=$(readlink -f "$0")
    cd "$(dirname "$script")/.."

    # The ids are taken here, as you. Inside `sudo` they would be root's.
    uid=$(id -u)
    gid=$(id -g)
    name=$(id -un)

    echo "test-offline: building the tests (network still on)"
    cargo test --workspace --locked --no-run

    # sudo clears the environment, so what cargo and the PTY tests need is passed on by name.
    set -- "HOME=$HOME" "PATH=$PATH" "USER=$name" "LOGNAME=$name" "TERM=${TERM:-xterm-256color}"
    for var in CARGO_HOME RUSTUP_HOME RUSTUP_TOOLCHAIN LANG LC_ALL; do
        value=$(printenv "$var" || true)
        if [ -n "$value" ]; then
            set -- "$@" "$var=$value"
        fi
    done
    echo "test-offline: entering a network namespace with sudo"
    exec sudo env "$@" unshare --net "$script" --in-namespace "$uid" "$gid"
    ;;

--in-namespace)
    [ "$(id -u)" -eq 0 ] || die "phase 2 must run as root inside sudo"
    uid=${2:?missing uid}
    gid=${3:?missing gid}
    [ "$uid" -ne 0 ] || die "refusing to run the tests as root (uid $uid)"
    PATH="/usr/sbin:/usr/bin:/sbin:/bin:$PATH"
    ip link set lo up
    script=$(readlink -f "$0")
    exec setpriv --reuid="$uid" --regid="$gid" --init-groups "$script" --tests "$uid" "$gid"
    ;;

--tests)
    uid=${2:?missing uid}
    gid=${3:?missing gid}
    # The tests must run as the real user. If setpriv did not drop root, stop before cargo can
    # write a root-owned file into target/.
    [ "$(id -u)" -ne 0 ] || die "the tests would run as root; stopping"
    [ "$(id -u)" = "$uid" ] || die "the user id is $(id -u), expected $uid; stopping"
    [ "$(id -g)" = "$gid" ] || die "the group id is $(id -g), expected $gid; stopping"
    PATH="/usr/sbin:/usr/bin:/sbin:/bin:$PATH"

    # The network must really be off: only loopback, nothing reachable, no name lookup.
    others=$(ip -o link show | grep -v ': lo:' || true)
    [ -z "$others" ] || die "the namespace has more than loopback: $others"
    ip -o addr show dev lo | grep -q '127.0.0.1' || die "loopback is not up"
    if timeout 5 bash -c 'exec 3<>/dev/tcp/192.0.2.1/80' 2>/dev/null; then
        die "a connection to 192.0.2.1 worked: the network is not off"
    fi
    if getent hosts example.com >/dev/null 2>&1; then
        die "example.com can be looked up: the network is not off"
    fi
    echo "test-offline: network is off (loopback only), running as uid $uid gid $gid"
    exec cargo test --workspace --locked --offline
    ;;

*)
    die "unknown argument: $phase"
    ;;
esac
