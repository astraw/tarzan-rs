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

# Host-neutral tar of the fixture (same flags as testdata/compat/README.md).
export COPYFILE_DISABLE=1
tar --format=pax --no-xattrs --uid 0 --gid 0 --uname '' --gname '' \
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
