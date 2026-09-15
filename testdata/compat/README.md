# Backward-compatibility fixtures

Archives produced by every published tarzan release, all wrapping the same
input: a bsdtar archive of `testdata/fixtures/tiny-tree` created with

```sh
COPYFILE_DISABLE=1 tar -cf tiny.tar -C testdata/fixtures/tiny-tree .
```

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
| `tarzan-v0.4.0.tar.zst` | adds `mtime_ns`, `xattrs`, and the other optional metadata fields |

When a release changes what `wrap` records, add a fixture for it here and a
row to the table in `tests/compat.rs`.
