# Backward-compatibility fixtures

Archives produced by every published tarzan release, all wrapping the same
input: a bsdtar archive of `testdata/fixtures/tiny-tree` created with

```sh
COPYFILE_DISABLE=1 tar --format=pax --no-xattrs --uid 0 --gid 0 --uname '' --gname '' \
    -cf tiny.tar -C testdata/fixtures/tiny-tree .
```

`--format=pax` forces a PAX header per member so releases that read PAX
records (sub-second `mtime`, `atime`, `ctime`) have something to record.
The remaining flags keep host facts out of the archive: `--no-xattrs` drops
metadata such as macOS's `com.apple.provenance`, which cannot be restored on
other platforms, and the owner flags replace the author's uid/gid and account
name with root and empty names. `tests/fixture_hygiene.rs` fails if any
committed fixture carries such facts without an explicit allowlist entry.

Each fixture was written by the release binary installed from crates.io:

```sh
cargo install tarzan --version X.Y.Z --root /tmp/tarzan-X.Y.Z
/tmp/tarzan-X.Y.Z/bin/tarzan wrap tiny.tar -f testdata/compat/tarzan-vX.Y.Z.tar.zst
```

`tests/compat.rs` opens each one with the current code and checks that the
TOC still deserialises, the members and their contents are intact, the
recorded checksums verify, and the whole-archive hash in the footer matches.
The TOC schema is only ever extended with optional fields, so an archive from
any v2 release must keep opening for as long as the v2 format is supported.

| Fixture | Notes |
|---|---|
| `tarzan-v0.1.2.tar.zst` | v1 format (no footer, no checksums). Rejected by the reader with a message pointing at `zstd -d`; kept to lock in that message and to prove the file is still a valid zstd stream. |
| `tarzan-v0.2.0.tar.zst` | first v2 release: footer, `content_sha256`, XXHash64 |
| `tarzan-v0.2.1.tar.zst` | |
| `tarzan-v0.2.2.tar.zst` | PAX `size=` honoured when wrapping |
| `tarzan-v0.3.0.tar.zst` | adds `content_md5` |
| `tarzan-v0.4.0.tar.zst` | adds `mtime_ns`, `atime`, `ctime`, and the other optional metadata fields |

`golden/` holds the expected `list --json` and `info --json` output for
each v2 fixture; `tests/golden_toc.rs` asserts the current binary reproduces
it byte-for-byte on every CI platform. Regenerate after an intentional
output change with `UPDATE_GOLDEN=1 cargo test --test golden_toc`.

When a release changes what `wrap` records, add a fixture for it here, a row
to the table in `tests/compat.rs`, and run the golden update.
