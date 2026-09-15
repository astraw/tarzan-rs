# Backward-compatibility fixtures

Archives produced by every published tarzan release, all wrapping the same
input: a bsdtar archive of `testdata/fixtures/tiny-tree` created with

```sh
COPYFILE_DISABLE=1 tar --format=pax --no-xattrs -cf tiny.tar -C testdata/fixtures/tiny-tree .
```

`--format=pax` forces a PAX header per member so releases that read PAX
records (sub-second `mtime`, `atime`, `ctime`) have something to record.
`--no-xattrs` keeps host metadata such as macOS's `com.apple.provenance`
out of the archive: it cannot be restored on other platforms, and the
fixtures must extract cleanly everywhere CI runs.

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

When a release changes what `wrap` records, add a fixture for it here and a
row to the table in `tests/compat.rs`.
