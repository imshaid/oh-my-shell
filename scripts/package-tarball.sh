#!/bin/sh
# Makes the release archive of one target (S29a) and its checksum line:
#   scripts/package-tarball.sh TARGET BINARY OUTDIR
# The archive holds oh-my-shell-TARGET/{oh-my-shell,LICENSE,README.md,THIRD-PARTY-LICENSES.html}.
# THIRD-PARTY-LICENSES.html (made by cargo about) must exist in the repository root.
set -eu
[ $# -eq 3 ] || { echo "usage: $0 TARGET BINARY OUTDIR" >&2; exit 2; }
target=$1
binary=$2
out=$3
root=$(cd "$(dirname "$0")/.." && pwd)
for f in "$binary" "$root/LICENSE" "$root/README.md" "$root/THIRD-PARTY-LICENSES.html"; do
    [ -f "$f" ] || { echo "package-tarball.sh: missing $f" >&2; exit 1; }
done
mkdir -p "$out"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
dir="$work/oh-my-shell-$target"
mkdir "$dir"
cp "$binary" "$dir/oh-my-shell"
chmod 755 "$dir/oh-my-shell"
cp "$root/LICENSE" "$root/README.md" "$root/THIRD-PARTY-LICENSES.html" "$dir/"
name="oh-my-shell-$target.tar.gz"
tar --owner=0 --group=0 --numeric-owner -C "$work" -czf "$out/$name" "oh-my-shell-$target"
(cd "$out" && sha256sum "$name" > "$name.sha256")
echo "$out/$name"
