# tarzan 🌿

[![Crates.io](https://img.shields.io/crates/v/tarzan.svg)](https://crates.io/crates/tarzan)
[![Documentation](https://docs.rs/tarzan/badge.svg)](https://docs.rs/tarzan/)
[![Crate License](https://img.shields.io/crates/l/tarzan.svg)](https://crates.io/crates/tarzan)
[![build](https://github.com/astraw/tarzan-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/astraw/tarzan-rs/actions?query=branch%3Amain)

**Tar Archive with Random-access Zstd And iNdex**

`tarzan` is a command-line tool for creating and extracting `.tar.zst` archives that
are fully seekable and self-indexed. It divides the archive into independently
compressed chunks — with chunk boundaries and size tunable to balance compression ratio
against random-access granularity — and embeds a table of contents (TOC) directly
inside the compressed stream as a zstd skippable frame. The underlying tar data is
preserved bit-for-bit; the archive can be decompressed by standard zstd tools, though
doing so discards the indexing and seekability that tarzan provides.

```sh
# Wrap any existing tar stream — drop-in for gzip or zstd
tar -cf - ./my-project | tarzan wrap -f my-project.tar.zst

# List contents instantly — no decompression, reads TOC only
tarzan list -f my-project.tar.zst

# Extract a single file — decompresses only the relevant chunks
tarzan cat -f my-project.tar.zst src/main.rs
```

The CLI follows tar's flag conventions where they overlap: `-f`/`--file`
names the archive, `-v` is verbose, `-C` selects a directory. Subcommands
have tar-style short aliases (`tarzan t` for `list`). See [What we don't
copy from tar](#what-we-dont-copy-from-tar) for the bits we leave behind.

---

## Why tarzan?

Standard `.tar.gz` and `.tar.zst` archives are sequential. To find a file near the
end, you decompress everything before it. For large archives this is slow, wasteful,
and makes random access effectively impossible without external tooling.

`tarzan` solves this with four ideas:

**1. Tunable chunk compression.** The archive is divided into independently compressed
zstd frames at configurable chunk boundaries. Chunk size is a tuneable tradeoff:
smaller chunks mean finer-grained random access but lower compression ratio (less
cross-chunk redundancy); larger chunks compress better but require decompressing more
data to reach a given file. The default of 4MB is a reasonable starting point; the
right value depends on your workload and access patterns, and benchmarking your
specific archive contents is recommended.

**2. Embedded TOC.** A table of contents — containing filenames, permissions,
ownership, sizes, and per-chunk byte offsets — is stored in a zstd skippable frame
appended to the archive. Any compliant zstd decoder silently ignores skippable frames,
so the archive is fully readable by `zstd -d | tar x` with no special support.

**3. Leading identity frame.** The first 14 bytes of every tarzan archive are a small
zstd skippable frame containing the ASCII identifier `TRZN` followed by a format
version byte. This allows `file(1)` and other format sniffers to identify tarzan
archives unambiguously, distinct from plain `.tar.zst` or other zstd-based formats.
Standard zstd tools skip this frame silently.

**4. Fixed-size trailing footer.** The last 38 bytes of every tarzan archive are a
small zstd skippable frame containing the TOC's byte offset, its size, and an
XXHash64 of every byte before the footer. Readers seek directly to the TOC in a
single operation regardless of archive size — no scanning. The hash gives
`tarzan verify --quick` a way to validate the whole archive in one sequential
read, without decompressing anything. Per-file integrity is layered on top: every
data frame carries zstd's own XXHash64 content checksum (caught at decompress
time), and by default every regular-file TOC entry records a `content_sha256`
(in the same format `sha256sum` produces) and a `content_md5` — so you can
compare against an on-disk copy or an S3 ETag (for single-PUT uploads)
without running tarzan. If needed for throughput-sensitive wraps, each can be
disabled independently (`--disable-sha256`, `--disable-md5`).

The result is an archive where:
- The original tar data is stored bit-for-bit intact inside the compressed stream
- Standard tools (`zstd -d | tar x`, `tar --zstd -xf`) can decompress it fully,
  but do so as a sequential scan, losing the indexing and random-access benefits
- Tools that understand the tarzan format can list contents without decompression
  and extract individual files by seeking directly to their chunks

---

## Installation

`tarzan` is a single crate that provides both the `tarzan` command-line binary
and the embeddable library (see [Library usage](#library-usage)).

### From crates.io

```sh
cargo install tarzan
```

### From source

```sh
git clone https://github.com/astraw/tarzan-rs
cd tarzan-rs
cargo build --release
# binary at ./target/release/tarzan
```

### Pre-built binaries

Pre-built binaries for Linux (x86_64, aarch64), macOS (x86_64, Apple Silicon),
and Windows (x86_64) are available on the
[releases page](https://github.com/astraw/tarzan-rs/releases).

Windows builds are provided but **untested**, and have two known limitations:
extracting an archive that contains symlink members fails on those entries, and
Unix permission bits are not restored. (`list -v` also shows timestamps in UTC
rather than local time on Windows.) Linux and macOS are the tested platforms.

---

## Usage

### `tarzan wrap` — compress an existing tar stream

The primary entry point for pipeline use. Reads a raw tar stream from stdin (or a
file) and writes a tarzan-formatted `.tar.zst` to stdout (or `-f`).

The input tar is a positional argument; the output archive is `-f`/`--file`,
mirroring `tar -cf out.tar`. Use `-` (or omit) for stdin/stdout.

```sh
# From stdin to stdout
tar -cf - ./dir | tarzan wrap > archive.tar.zst

# From a file to a file
tarzan wrap archive.tar -f archive.tar.zst

# With explicit output path
tar -cf - ./dir | tarzan wrap -f archive.tar.zst

# Control chunk size (default: 4MB)
tar -cf - ./dir | tarzan wrap --chunk-size 1M -f archive.tar.zst

# Set zstd compression level (default: 3)
tar -cf - ./dir | tarzan wrap --level 9 -f archive.tar.zst

# Skip per-file SHA-256 generation in the TOC
tar -cf - ./dir | tarzan wrap --disable-sha256 -f archive.tar.zst

# git archive integration
git archive HEAD | tarzan wrap -f release.tar.zst

# Remote backup
ssh user@host "tar -cf - /data" | tarzan wrap -f backup.tar.zst

# Verbose: list each member to stderr as it is wrapped
tar -cf - ./dir | tarzan wrap -v -f archive.tar.zst
```

For safety, `wrap` refuses to write the binary archive directly to a
terminal: if `-f` is omitted and stdout is a TTY, it errors out. Pipe
the output, redirect to a file, or pass `-f`.

By default `wrap` computes both `content_sha256` and `content_md5` for regular
files. Use `--disable-sha256` and/or `--disable-md5` to skip one or both.

### Creating archives from files

tarzan does not implement its own filesystem walker. Use the system
`tar` to produce the tar stream, and pipe it into `tarzan wrap`:

```sh
# A whole directory
tar -cf - ./my-project | tarzan wrap -f my-project.tar.zst

# Multiple paths
tar -cf - ./src ./docs ./README.md | tarzan wrap -f bundle.tar.zst

# Change source directory, like `tar -C`
tar -cf - -C ./build . | tarzan wrap -f build.tar.zst

# Exclude patterns (tar's own --exclude)
tar -cf - --exclude='*.o' --exclude='target/*' ./my-project \
    | tarzan wrap -f archive.tar.zst

# git archive integration
git archive HEAD | tarzan wrap -f release.tar.zst

# Remote backup
ssh user@host "tar -cf - /data" | tarzan wrap -f backup.tar.zst
```

This composition is deliberate: real tar handles hard links, sparse
files, xattrs, ACLs, long path/link names (PAX/GNU extensions), and
device files correctly. Re-implementing that surface inside tarzan would
either replicate tar poorly or shell out to it anyway, so we lean on
the canonical `tar | tarzan wrap` pipeline instead.

`wrap` parses the tar structure needed for indexing, including GNU and PAX
extension records, but preserves the input bytes verbatim. Vendor-specific
metadata that tarzan does not interpret therefore remains available to a
native tar extractor after decompression.

### `tarzan list` — list contents

Reads only the TOC skippable frame. Fast regardless of archive size.
Aliased as `tarzan t` (tar style) and `tarzan ls`.

```sh
# Paths only, one per line
tarzan list -f archive.tar.zst

# tar-style short alias
tarzan t -f archive.tar.zst

# Long format: mode, owner/group, size, mtime, path — like `tar -tvf`.
# Symlink and hard-link entries show their target as `path -> target`.
tarzan list -v -f archive.tar.zst

# Show -v timestamps in UTC instead of local time, like `tar --utc -tvf`
tarzan list -v --utc -f archive.tar.zst

# Filter by directory prefix, exact path, or shell glob (positional args)
tarzan list -f archive.tar.zst src/
tarzan list -f archive.tar.zst '*.toml'
tarzan list -v -f archive.tar.zst src/main.rs Cargo.toml

# Machine-readable JSON (respects positional filters)
tarzan list --json -f archive.tar.zst
```

Long-format output:
```text
drwxr-xr-x 1000/1000         0 B  2024-11-03 14:20  ./
-rw-r--r-- 1000/1000      4.2 KB  2024-11-03 14:22  src/main.rs
-rw-r--r-- 1000/1000     12.1 KB  2024-11-03 14:22  src/lib.rs
lrwxrwxrwx 1000/1000         0 B  2024-11-03 14:22  src/current -> main.rs
-rw-r--r-- 1000/1000      1.1 KB  2024-11-03 14:20  Cargo.toml
```

Owner is shown numerically (`uid/gid`) rather than as resolved names —
the TOC stores numbers, and resolving them against the *reader's*
`/etc/passwd` would be misleading.

Timestamps are shown in local time, like `tar -tvf`; pass `--utc` for
UTC. The stored `mtime` is a timezone-independent Unix timestamp, so only
the display differs.

`--json` emits the TOC as a pretty-printed JSON array. Each entry
carries path, type, size, mode, uid, gid, mtime, tar_offset, optional
link target, optional SHA-256 and MD5 checksums, and chunks. Some
entries also carry optional additive metadata (all within the v2
archive schema):

- `mtime_ns`, `atime`, `atime_ns`, `ctime`, `ctime_ns`
- `uname`, `gname`
- `xattrs` (from PAX `SCHILY.xattr.*` / `LIBARCHIVE.xattr.*`)
- `path_bytes`, `link_target_bytes` (for non-UTF-8 names)
- `raw_type_byte` (for entries reported as `type: "other"`)

Example:

```json
[
  {
    "path": "src/main.rs",
    "type": "file",
    "size": 4301,
    "mode": 420,
    "uid": 1000,
    "gid": 1000,
    "mtime": 1730643742,
    "tar_offset": 1024,
    "content_sha256": "a948904f2f0f479b8f936f2b38bf5e9e2c5a7b5b5e7e3f7b6e5d4c3b2a19080f",
    "content_md5": "1b374e3a9f0c8d7b6e5a4f3c2d1b0a9e",
    "chunks": [
      {
        "compressed_offset": 1024,
        "compressed_size": 1891,
        "uncompressed_size": 4301
      }
    ]
  }
]
```

`content_sha256` is the SHA-256 of the file's bytes — no tar header, no
padding — in the same format `sha256sum` prints. `content_md5` is the MD5
of the same bytes, provided for interoperability with systems that expose
MD5 checksums (e.g. S3 ETags for single-PUT uploads). `wrap` computes both
by default; either field may be omitted if wrapping used
`--disable-sha256` and/or `--disable-md5`. Note that S3 ETags for multipart
uploads are not the plain MD5 of the object and will not match this field.
For cryptographic integrity, use `content_sha256`.

To check whether your local copy of an archived file matches what was
recorded at wrap time:

```sh
tarzan list --json -f archive.tar.zst \
  | jq -r '.[] | select(.content_sha256) | "\(.content_sha256)  \(.path)"' \
  > archive.sha256sums
sha256sum -c archive.sha256sums
```

Each entry in `chunks` locates one member's bytes inside a compressed
frame. A member larger than the chunk size spans several chunks; small
members are packed together to share a frame, and `frame_offset`
(omitted when zero) then gives the member's offset within that frame's
decompressed data.

Pipe through `jq` to slice out fields you don't want (for example
`jq 'map(del(.chunks))'`).

### `tarzan extract` — extract files

Aliased as `tarzan x` (tar style). Refuses to write members whose path is
absolute or contains `..`, so extraction always stays inside the
destination directory.

```sh
# Extract everything to the current directory
tarzan extract -f archive.tar.zst

# Extract to a specific directory
tarzan extract -f archive.tar.zst -C /tmp/out

# Extract specific files (decompresses only relevant chunks)
tarzan extract -f archive.tar.zst src/main.rs src/lib.rs

# Extract a directory subtree
tarzan extract -f archive.tar.zst src/

# Drop leading path components, like `tar --strip-components`
tarzan extract -f archive.tar.zst -C build --strip-components 1

# Skip members by shell-glob pattern (repeatable)
tarzan extract -f archive.tar.zst --exclude '*.o' --exclude 'target/*'

# Print each member as it is extracted
tarzan x -v -f archive.tar.zst

# Do not restore recorded mtimes (extracted files get the current time)
tarzan extract -f archive.tar.zst --no-mtime

# Survive bit-rot: log and skip members whose data won't decompress,
# rather than aborting the whole extraction
tarzan extract -f archive.tar.zst --skip-bad-chunks
```

Restored on extract: file contents, directory hierarchy, Unix permission
bits, symlinks (Unix only), hard links, xattrs from PAX records (Unix only),
and mtime on files, symlinks, and
directories. Directory mtimes are applied in a deferred pass after all
children are written, so creating a child doesn't bump the parent's
timestamp back; hard links are likewise reconstructed in a second pass
once their target file is on disk. If a hard link's target member is not
part of the extraction — for example a path filter selects the link but
not its target — the link is skipped with a warning. `--no-mtime` skips
timestamp restoration entirely. Character/block devices and FIFOs are
still skipped with a warning.

For macOS-created archives, length-delimited binary PAX values are supported;
`LIBARCHIVE.xattr.*` values are base64-decoded, while raw
`SCHILY.xattr.*` values take precedence when both encodings name the same
attribute. AppleDouble `._*` companions are preserved and indexed as ordinary
members, but tarzan does not apply their Finder metadata or resource-fork
semantics to the paired file.

For workflows that need full fidelity — device files, FIFOs, ACLs,
sparse files, or native AppleDouble/resource-fork handling — fall back to
standard tooling. Every tarzan archive is a valid zstd stream:

```sh
zstd -d archive.tar.zst | tar x
# or
tar --zstd -xf archive.tar.zst
```

You give up tarzan's random-access seeking but get real tar's full
coverage of the long tail. The trade is: `tarzan extract` is the fast
path for the common case; `tar --zstd -xf` is the complete path.

### `tarzan cat` — stream a single file to stdout

Seeks directly to the file using the TOC; decompresses only its chunks.

```sh
tarzan cat -f archive.tar.zst src/main.rs

# Pipe into another tool
tarzan cat -f archive.tar.zst data/records.csv | awk -F, '{print $2}'
```

Only regular-file entries work — hard-link entries reference another
member rather than holding their own bytes, and will error. For
full-fidelity single-file extraction via standard tools:

```sh
tar --zstd -xOf archive.tar.zst path/in/archive
```

That path scans sequentially rather than seeking, but resolves hard
links the way real tar does.

### `tarzan info` — show archive metadata

Reads only the TOC frame, so it runs in constant time regardless of
archive size.

```sh
tarzan info -f archive.tar.zst

# Machine-readable JSON object
tarzan info --json -f archive.tar.zst
```

```text
Format:          tarzan v2
File:            archive.tar.zst
Size:            487.2 MB
Uncompressed:    2.3 GB
Ratio:           21.1% (archive / uncompressed)
Data frames:     486.4 MB (sum of compressed frames)
Members:         1847
Chunks:          4203
Avg chunk size:  574.5 KB (uncompressed)
Identity frame:  TRZN v2
TOC frame:       312.0 KB at offset 487204816
```

With `--json`, the same data is emitted as an object (`ratio` and
`avg_chunk_size_bytes` are `null` for an empty archive):

```json
{
  "format_version": 1,
  "identity_version": 1,
  "file": "archive.tar.zst",
  "size_bytes": 510656512,
  "uncompressed_bytes": 2480619520,
  "data_frame_bytes": 509939712,
  "ratio": 0.2058,
  "members": 1847,
  "chunks": 4203,
  "avg_chunk_size_bytes": 590201,
  "toc_offset": 487204816,
  "toc_frame_bytes": 319488
}
```

Some fields the legacy README example referenced are intentionally
omitted: the archive does not record a creation timestamp, and the
chunk-size argument is a wrap-time tunable rather than archive metadata
(use `Avg chunk size` as an observed proxy).

### `tarzan verify` — verify checksums

Silent on success by default; exits non-zero on mismatch. Pass `-v`
to also print an `OK` line per verified item.

By default `verify` walks the TOC, extracts each regular file's content,
and compares its SHA-256 against the `content_sha256` recorded at wrap
time. zstd's per-frame XXHash64 checksum is verified automatically along
the way. With `--quick`, the per-file work is skipped entirely; the
archive is re-hashed once with XXHash64 and compared against the value
stored in the trailing footer — one sequential read, no decompression.

If an archive was created with `--disable-sha256`, full `verify` reports that
no SHA-256 checksums are present and exits successfully unless mismatches are
found for entries that do carry checksums.

```sh
# Full per-file verification (decompresses every chunk)
tarzan verify -f archive.tar.zst

# Verify a specific file's content hash
tarzan verify -f archive.tar.zst src/main.rs

# Show per-member OK lines
tarzan verify -v -f archive.tar.zst

# Whole-archive integrity check (fast; one sequential read)
tarzan verify --quick -f archive.tar.zst
```

The two modes catch different things. `--quick` catches any byte-level
damage to the archive file (including stray bytes appended after the
original) but doesn't, by itself, detect every kind of zstd-level
corruption — zstd's own per-frame checksum only fires during
decompression. Full verify catches per-file mismatches at the cost of
decompressing every frame.

---

## File format and Rust API

The file format specification (frame layout, magic numbers, TOC schema, zstd
compatibility) and the Rust library API are documented in the
[crate module documentation on docs.rs](https://docs.rs/tarzan).

### Identifying tarzan archives

The identity frame occupies the first 14 bytes of every tarzan archive.
`xxd -l 14` reveals it without any special tooling:

```sh
xxd -l 14 archive.tar.zst
# 00000000: 542a 4d18 0600 0000 5452 5a4e 0102       T*M.....TRZN..
#           └── 0x184D2A54 ──┘           └TRZN┘  └── version byte (v2)
#           zstd skippable magic   tarzan identifier at offset 8
```

A `file(1)` magic pattern is also distributed at
[contrib/tarzan.magic](contrib/tarzan.magic). Use the `MAGIC=` environment
variable rather than `-m` — on macOS, `-m` augments the compiled system magic
database, which then wins on strength over the tarzan pattern:

```sh
MAGIC=contrib/tarzan.magic file archive.tar.zst
# archive.tar.zst: tarzan archive v2
```

---

## What we don't copy from tar

tarzan borrows tar's flag conventions where they overlap, but deliberately
skips a few of its older ergonomics:

- **Bundled short flags (`-xvf`).** tar lets you mash mode and option letters
  together as a single argument; modern argument parsers don't, and the form
  is widely considered tar's most arcane bit. tarzan accepts `-x -v -f` style
  spacing only.
- **Mode-flag entry point (`tar -cf`).** tar selects its operation with a flag
  letter on the root command. tarzan uses subcommands (`tarzan wrap`,
  `tarzan list`, ...) for better discoverability and shell tab-completion;
  tar-style short aliases (`tarzan t`) cover the muscle-memory case.
- **A separate `create` verb / filesystem walker.** `wrap` reads an existing
  tar stream and adds the tarzan envelope; the canonical archive-creation
  workflow is `tar -cf - ... | tarzan wrap -f out.tar.zst`. We do not
  re-implement `tar -c` ourselves — real tar already handles hard links,
  sparse files, xattrs, long path names, and device files correctly, and
  a partial in-tree walker would silently mishandle those long-tail
  cases. See [Creating archives from files](#creating-archives-from-files).
- **Compression-format flags (`-z`, `-j`, `-J`, `--zstd`).** A tarzan archive
  is always zstd, so a compression selector would only ever take one value.
- **Mandatory archive flag with no positional fallback.** GNU tar accepts
  `tar tf archive.tar` only because of bundling; without bundling, an archive
  always needs `-f`. tarzan uses `-f`/`--file` uniformly, but with subcommands
  the form stays consistent rather than depending on whether you remembered
  to merge letters.

---

## Comparison

| | tar.gz | tar.zst | tarzan | zip |
|---|---|---|---|---|
| List without full decompress | ✗ | ✗ | ✓ | ✓ |
| Extract one file efficiently | ✗ | ✗ | ✓ | ✓ |
| Streamable creation | ✓ | ✓ | ✓ | ✗ |
| Standard tool compatible | ✓ | ✓ | ✓ | ✓ |
| Compression ratio | good | better | good† | ok |
| Decompression speed | slow | fast | fast | ok |
| Self-describing format | ✗ | ✗ | ✓ | ✓ |
| Per-file integrity checksums | ✗ | ✗ | ✓ | optional |
| Whole-archive integrity hash | ✗ | ✗ | ✓ | ✗ |

† Slightly lower than monolithic `.tar.zst` due to per-frame independent compression,
which loses redundancy across frame boundaries. Small members are packed together so
redundancy is still captured within a frame; for most archives the difference is under 5%.

---

## What happens when bits flip

Independent zstd frames give tarzan crash isolation: damage to one data frame
takes out one member (or a handful of small members that share a frame), not
the whole archive. Damage to the metadata regions is more severe — they are
single-copy by design — but the underlying tar data is still recoverable
through standard tools.

| Damaged region | What tarzan does | Fallback that still works |
|---|---|---|
| Identity frame (first 14 B) | `tarzan open` rejects the file as not a tarzan archive | `zstd -d archive.tar.zst \| tar x` |
| One data frame | only the affected member(s) fail to extract; zstd's per-frame XXHash64 checksum catches the corruption during decompression, with the per-member SHA-256 as a second line of defense at the file-content level | `tarzan extract --skip-bad-chunks` to keep going past it |
| TOC frame | open rejects the file (TOC won't decompress) | `zstd -d \| tar x` for full recovery |
| Footer | open rejects the file | `zstd -d \| tar x` for full recovery |
| Just the hash bytes in the footer | open succeeds; `tarzan verify --quick` reports the mismatch | full per-chunk verify still works |

For the only case where partial recovery is interesting — bit-rot inside one
data frame — `tarzan extract --skip-bad-chunks` logs the bad member to stderr,
removes the partial output file, and continues with the remaining members.
Without the flag, the first unreadable chunk aborts the whole extract; that's
the safer default for backups where you'd rather notice a problem than
silently end up with a partial restore.

If you care about long-term archive durability, pair tarzan with a filesystem
that detects bit-rot (ZFS, btrfs with checksums) or external redundancy
(par2, replicated backups). tarzan won't reconstruct lost bytes — its job is
to detect corruption and isolate the blast radius.

---

## Library usage

The `tarzan` crate exposes a library API for embedding tarzan support in other
tools. Add it to your `Cargo.toml`:

```toml
[dependencies]
tarzan = "0.2"
```

### Cargo features

| Feature | Default | Description |
|---|---|---|
| `zstd-sys` | ✓ | Links the zstd C library via `zstd-sys`. Best compression performance. |
| `pure-rust` | | Pure-Rust zstd via `zstd-pure-rs`. No C toolchain required; useful for cross-compilation. |

Exactly one of the two features must be active. To opt into the pure-Rust build,
disable the default features and enable `pure-rust`:

```toml
[dependencies]
tarzan = { version = "0.2", default-features = false, features = ["pure-rust"] }
```

Full API documentation — including format details and usage examples — is on
[docs.rs/tarzan](https://docs.rs/tarzan).

---

## Relationship to zstd:chunked

tarzan is inspired by the `zstd:chunked` format used by the container ecosystem
(Podman, CRI-O, Fedora container images). That format solves the same core problem —
seekable, indexed, compressed tar archives — but is designed around OCI container image
layers and is not officially documented outside its reference implementation in
[containers/storage](https://github.com/containers/storage).

tarzan takes the same architectural approach — independent chunk compression, JSON TOC
in a skippable frame, full backward compatibility — and applies it to general-purpose
archiving with a clean, documented, versioned format specification.

tarzan archives are not wire-compatible with zstd:chunked, but the ideas are directly
borrowed from that project. Credit to Giuseppe Scrivano and the containers/storage
contributors.

---

## Releasing

Releases are managed by [release-plz](https://release-plz.dev) and
[cargo-dist](https://github.com/axodotdev/cargo-dist).

### Checking semver compatibility

When running [`cargo semver-checks`](https://github.com/obi1kenobi/cargo-semver-checks), always pass `--default-features`:

```sh
cargo semver-checks --default-features
```

Without it, `cargo semver-checks` may activate mutually-incompatible features (e.g. both `zstd-sys` and `pure-rust`) and fail to compile.

### How it fits together

- **release-plz** opens a "Release PR" on every push to `main`, bumps
  `Cargo.toml`, regenerates `CHANGELOG.md`, publishes to crates.io, and pushes
  a semver git tag.
- **cargo-dist** watches for semver tag pushes and builds the platform binaries,
  then creates the GitHub Release with them attached.

The critical detail: GitHub Actions **will not** trigger a workflow run from
events (including tag pushes) that are caused by the built-in `GITHUB_TOKEN`.
release-plz must therefore use a Personal Access Token (PAT) to push the tag so
that GitHub treats it as a real user event and wakes up cargo-dist.

### Required secrets

| Secret | Purpose |
|---|---|
| `RELEASE_PLZ_TOKEN` | PAT with `contents: write` and `pull-requests: write` — used by release-plz so its tag push triggers cargo-dist |
| `CARGO_REGISTRY_TOKEN` | crates.io API token for publishing |

### Normal release flow

**Step 1 — merge conventional commits to `main`.**
Every push to `main` triggers the `release-plz` workflow, which opens (or
updates) a Release PR.

**Step 2 — merge the Release PR.**
release-plz publishes to [crates.io](https://crates.io/crates/tarzan) and
pushes a semver git tag (e.g. `v0.2.0`) authenticated with `RELEASE_PLZ_TOKEN`.

**Step 3 — binaries build automatically.**
The tag push triggers the cargo-dist Release workflow, which cross-compiles and
uploads pre-built archives for:

| Target | Archive |
|---|---|
| Linux x86_64 | `tarzan-x86_64-unknown-linux-gnu.tar.gz` |
| Linux aarch64 | `tarzan-aarch64-unknown-linux-gnu.tar.gz` |
| macOS x86_64 | `tarzan-x86_64-apple-darwin.tar.gz` |
| macOS Apple Silicon | `tarzan-aarch64-apple-darwin.tar.gz` |
| Windows x86_64 | `tarzan-x86_64-pc-windows-msvc.zip` |

All archives include the binary, `README.md`, `LICENSE-MIT`, `LICENSE-APACHE`,
and `THIRD-PARTY-LICENSES`. The completed release appears on the
[releases page](https://github.com/astraw/tarzan-rs/releases).

### Recovering a release that reached crates.io but has no GitHub Release

This happens when release-plz pushed the tag using `GITHUB_TOKEN` (before the
PAT was configured) — cargo-dist never saw the event. The tag already exists on
the remote, so a plain push is rejected. Delete and re-push it to re-trigger:

```sh
git push origin :refs/tags/v0.1.1   # delete the remote tag
git push origin v0.1.1              # re-push; triggers cargo-dist
```

Replace `v0.1.1` with the actual tag name (`git ls-remote --tags origin` lists
what is there).

---

## AI-assisted development

`tarzan` was developed with substantial AI assistance. The implementation was
generated iteratively using large language models — primarily Claude Opus 4.7 and
Claude Sonnet 4.6 (Anthropic), with a small number of early commits from Gemma 4
31B (Google) — under continuous direction and review by the human author. Every
commit records the contributing model in its subject line.

**Validation.** Correctness is validated through:

- An automated test suite (`cargo test`) covering wrapping, listing, extracting,
  verifying, error paths, and round-trip integrity
- CI that runs tests on Linux, macOS, and Windows on every push
- Iterative testing against real tar archives during development, with the human
  author reviewing each change before it was committed

**Known gaps.** Coverage is thinner in a few areas:

- **Windows** — builds pass CI but the platform is otherwise untested in practice
- **Performance** — no formal benchmarks against comparable tools (pixz, zip,
  plain tar.zst) have been run on realistic workloads
- **Long-tail tar features** — sparse files, xattrs, device files, and ACLs are
  delegated to the `tar | tarzan wrap` pipeline rather than handled internally;
  that delegation path is not independently tested

---

## Contributing

Contributions are welcome. Please read [CONTRIBUTING.md](CONTRIBUTING.md) before
opening a pull request.

Areas of particular interest:
- Windows support (currently untested)
- Ratarmount backend using the embedded TOC
- Benchmarks against pixz, zip, and plain tar.zst on realistic workloads
- Submission of the magic pattern to the upstream `file` database

---

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](./LICENSE-APACHE))
- MIT License ([LICENSE-MIT](./LICENSE-MIT))

at your option.

tarzan binaries statically include the zstd C library. The zstd C library is
under a dual BSD/GPLv2 license. Full license texts for zstd and every other
dependency compiled into tarzan are in
[THIRD-PARTY-LICENSES](./THIRD-PARTY-LICENSES), which is bundled in every
release archive.
