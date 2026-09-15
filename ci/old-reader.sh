#!/usr/bin/env bash
# Forward-compatibility probe: can an *old* tarzan release read an archive
# written by the *current* build?
#
#   ci/old-reader.sh OLD_TARZAN_BINARY NEW_TARZAN_BINARY
#
# tarzan does not promise forward compatibility. This script exists so the
# day a change breaks old readers, CI says so and the change lands knowingly.
# The archive is the tiny-tree fixture wrapped with the current build; the
# old binary must list the same members, verify both ways, and extract
# content byte-identical to the fixture tree.
set -euo pipefail

old=$1
new=$2
root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

echo "old reader: $("$old" --version)"
echo "new writer: $("$new" --version)"

# Host-neutral tar of the fixture: PAX format, no xattrs, owner normalised
# to root with no names (same intent as testdata/compat/README.md). GNU tar
# and bsdtar spell the owner flags differently. TAR can override the binary
# for local testing (e.g. TAR=gtar on macOS).
TAR=${TAR:-tar}
export COPYFILE_DISABLE=1
if "$TAR" --version 2>/dev/null | grep -q 'GNU tar'; then
    owner_flags=(--owner=0 --group=0 --numeric-owner)
else
    owner_flags=(--uid 0 --gid 0 --uname '' --gname '')
fi
"$TAR" --format=pax --no-xattrs "${owner_flags[@]}" \
    -cf "$work/tiny.tar" -C "$root/testdata/fixtures/tiny-tree" .

"$new" wrap "$work/tiny.tar" -f "$work/new.tar.zst"

echo "--- old reader: info"
"$old" info -f "$work/new.tar.zst"

echo "--- old reader: list must match new reader"
"$old" list -f "$work/new.tar.zst" | sort > "$work/old.list"
"$new" list -f "$work/new.tar.zst" | sort > "$work/new.list"
diff "$work/old.list" "$work/new.list"

echo "--- old reader: verify (full and --quick)"
"$old" verify -f "$work/new.tar.zst"
"$old" verify --quick -f "$work/new.tar.zst"

echo "--- old reader: extract must reproduce the fixture tree"
mkdir "$work/out"
"$old" extract -f "$work/new.tar.zst" -C "$work/out"
diff -r "$root/testdata/fixtures/tiny-tree" "$work/out"

echo "--- old reader: cat"
"$old" cat -f "$work/new.tar.zst" ./README.txt | cmp - "$root/testdata/fixtures/tiny-tree/README.txt"

echo "OK: $(basename "$old") reads archives from $(basename "$new")"
