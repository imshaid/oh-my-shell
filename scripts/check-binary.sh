#!/bin/sh
# Guards on the release binary (S29a): it must run on the oldest system we promise.
#   scripts/check-binary.sh BINARY [MAX_GLIBC]
# 1. The highest glibc symbol version it needs is at most MAX_GLIBC (default 2.35: Ubuntu 22.04,
#    Debian 12). Release builds run on ubuntu-22.04 so that this holds.
# 2. It links only the libraries every glibc system has: libc, libm, libgcc_s, libpthread, libdl
#    and the dynamic loader. Anything else (libssl, libz, ...) would be missing on some
#    machines, so the check fails and names it.
set -eu
[ $# -ge 1 ] || { echo "usage: $0 BINARY [MAX_GLIBC]" >&2; exit 2; }
bin=$1
max=${2:-2.35}
[ -f "$bin" ] || { echo "check-binary.sh: $bin is not a file" >&2; exit 2; }
fail=0

needed=$(readelf -d "$bin" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p')
for lib in $needed; do
    case "$lib" in
        libc.so.6 | libm.so.6 | libgcc_s.so.1 | libpthread.so.0 | libdl.so.2 | \
        ld-linux-x86-64.so.2 | ld-linux-aarch64.so.1) ;;
        *) echo "check-binary.sh: $bin needs $lib, which not every system has" >&2; fail=1 ;;
    esac
done

highest=$(objdump -T "$bin" | grep -o 'GLIBC_[0-9][0-9.]*' | sed 's/^GLIBC_//' | sort -uV | tail -n 1)
if [ -n "$highest" ]; then
    newest=$(printf '%s\n%s\n' "$highest" "$max" | sort -V | tail -n 1)
    if [ "$newest" != "$max" ]; then
        echo "check-binary.sh: $bin needs glibc $highest; the limit is $max" >&2
        fail=1
    fi
fi

if [ "$fail" -eq 0 ]; then
    echo "check-binary.sh: ok (libraries: $(echo "$needed" | paste -sd, -); highest glibc symbol: ${highest:-none}, limit $max)"
fi
exit "$fail"
