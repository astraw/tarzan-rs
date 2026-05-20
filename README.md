# tarzan 🌿

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

`tarzan` solves this with three ideas:

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

**3. Leading identity frame.** The first bytes of every tarzan archive are a small
zstd skippable frame containing the ASCII identifier `TRZN` followed by a format
version byte. This allows `file(1)` and other format sniffers to identify tarzan
archives unambiguously, distinct from plain `.tar.zst` or other zstd-based formats.
Standard zstd tools skip this frame silently.

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

Pre-built static binaries for Linux (x86_64, aarch64) and macOS (x86_64, Apple
Silicon) are available on the [releases page](https://github.com/astraw/tarzan-rs/releases).

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
carries path, type, size, mode, uid, gid, mtime, optional link target,
and chunk offsets:

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
    "chunks": [
      {
        "compressed_offset": 1024,
        "compressed_size": 1891,
        "uncompressed_size": 4301,
        "sha256": "e3b0c44298fc1c149afb..."
      }
    ]
  }
]
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
```

Restored on extract: file contents, directory hierarchy, Unix permission
bits, symlinks (Unix only), hard links, and mtime on files, symlinks, and
directories. Directory mtimes are applied in a deferred pass after all
children are written, so creating a child doesn't bump the parent's
timestamp back; hard links are likewise reconstructed in a second pass
once their target file is on disk. If a hard link's target member is not
part of the extraction — for example a path filter selects the link but
not its target — the link is skipped with a warning. `--no-mtime` skips
timestamp restoration entirely. Character/block devices and FIFOs are
still skipped with a warning.

For workflows that need full fidelity — device files, FIFOs, xattrs/ACLs,
sparse files — fall back to standard tooling. Every tarzan archive is a
valid zstd stream:

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
Format:          tarzan v1
File:            archive.tar.zst
Size:            487.2 MB
Uncompressed:    2.3 GB
Ratio:           21.1% (archive / uncompressed)
Data frames:     486.4 MB (sum of compressed frames)
Members:         1847
Chunks:          4203
Avg chunk size:  574.5 KB (uncompressed)
Identity frame:  TRZN v1
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

### `tarzan verify` — verify chunk checksums

Silent on success by default; exits non-zero on mismatch. Pass `-v`
to also print an `OK` line per verified member.

```sh
# Verify all chunk SHA-256s
tarzan verify -f archive.tar.zst

# Verify a specific file
tarzan verify -f archive.tar.zst src/main.rs

# Show per-member OK lines
tarzan verify -v -f archive.tar.zst
```

---

## File Format

A tarzan archive is a valid zstd stream consisting of three sections:

```text
┌─────────────────────────────────────────────────────────┐
│  Identity frame (skippable)                             │
│  Magic: 0x184D2A54  Content: "TRZN" + version byte      │
├─────────────────────────────────────────────────────────┤
│  Compressed data frames                                  │
│  Independent zstd frames sized around --chunk-size.     │
│  Large members are split across several frames; small  │
│  members are packed together to share a frame.          │
├─────────────────────────────────────────────────────────┤
│  TOC frame (skippable)                                  │
│  Magic: 0x184D2A54  Content: zstd-compressed JSON TOC   │
│  Located at the end; found by scanning from EOF.        │
└─────────────────────────────────────────────────────────┘
```

The skippable frame magic number `0x184D2A54` is used for both the identity frame and
the TOC frame; they are distinguished by position (first vs last) and by a type byte
in the frame payload.

The zstd spec defines any value in `0x184D2A50`–`0x184D2A5F` as a skippable frame and
assigns no meaning to the low nibble. Producers may use any value in the range, and
per the spec other tools may legally use the same magic number — so tarzan-aware
readers identify tarzan frames via the `TRZN` ASCII identifier at the start of the
payload, not by the magic number alone.

The specific value `0x184D2A54` was chosen because (1) it avoids `0x184D2A5E`, which
the [zstd seekable format extension][seekable] uses, and (2) zstd frames are
little-endian on disk, so `0x184D2A54` is written as the byte sequence `54 2A 4D 18`
— the first byte of every tarzan archive is ASCII `T`, which then continues into the
`TRZN` payload identifier eight bytes later. A hex dump of any tarzan archive begins
with a literal `T`.

[seekable]: https://github.com/facebook/zstd/blob/dev/contrib/seekable_format/zstd_seekable_compression_format.md

### TOC schema

The TOC is a zstd-compressed JSON object. Abridged example:

```json
{
  "tarzan_version": 1,
  "members": [
    {
      "path": "src/main.rs",
      "type": "file",
      "size": 4301,
      "mode": "0o644",
      "uid": 1000,
      "gid": 1000,
      "mtime": 1730643742,
      "chunks": [
        {
          "compressed_offset": 1024,
          "compressed_size": 1891,
          "uncompressed_size": 4301,
          "sha256": "e3b0c44298fc1c149afb..."
        }
      ]
    }
  ]
}
```

Each chunk also carries an optional `frame_offset` (omitted when zero):
when small members are packed into a shared frame, it records where the
member's bytes begin within that frame's decompressed data.

Full schema documentation is in [docs/format.md](docs/format.md).

### Compatibility

A tarzan archive can be decompressed by any standard zstd implementation:

```sh
# Both of these work on any tarzan archive
zstd -d archive.tar.zst | tar x
tar --zstd -xf archive.tar.zst
```

The identity frame and TOC frame are silently skipped by standard zstd. The
decompressed tar stream is bit-for-bit identical to what you would have gotten from
plain `tar -cf`. What you lose by going through standard tools is tarzan's indexing:
listing contents requires a full sequential decompression pass, and extracting a
single file requires decompressing everything before it. The tar data itself is
never altered.

### `file(1)` recognition

A magic pattern for tarzan archives is distributed with this repository at
[contrib/tarzan.magic](contrib/tarzan.magic) and has been submitted to the upstream
`file` database. To use it locally before it ships in your distro:

```sh
file -m contrib/tarzan.magic archive.tar.zst
# archive.tar.zst: tarzan archive v1
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

† Slightly lower than monolithic `.tar.zst` due to per-frame independent compression,
which loses redundancy across frame boundaries. Small members are packed together so
redundancy is still captured within a frame; for most archives the difference is under 5%.

---

## Library usage

The `tarzan` crate exposes a library API for embedding tarzan support in other tools.

```toml
[dependencies]
tarzan = "0.1"
```

```rust,no_run
use tarzan::{TarzanReader, WrapOptions};
use std::fs::File;
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Wrap an existing tar stream
    let input = File::open("archive.tar")?;
    let output = File::create("archive.tar.zst")?;
    let opts = WrapOptions::default().chunk_size(4 * 1024 * 1024);
    tarzan::wrap(input, output, opts)?;

    // Read the TOC without decompression
    let reader = TarzanReader::open(Path::new("archive.tar.zst"))?;
    for member in reader.members() {
        println!("{} ({} bytes)", member.path, member.size);
    }

    // Extract a single file
    let mut out = File::create("main.rs")?;
    reader.extract_member("src/main.rs", &mut out)?;
    Ok(())
}
```

Full API documentation is on [docs.rs/tarzan](https://docs.rs/tarzan).

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

## Design decisions

### TOC sidecar mode (considered, deferred)

A natural extension of the embedded TOC is to also serialize it as a standalone file
(e.g. `archive.tar.toc`) that accompanies a plain `.tar` — enabling random access
without the zstd wrapper, including for tape workflows. This is intentionally
deferred from v1.

Why deferred:

- *Drift.* Sidecar files get separated from their data through copy, move, or
  transfer. A stale sidecar fails silently unless every read verifies a whole-tar
  hash, which is an O(n) scan that partly defeats the point of having an index.
- *Schema bifurcation.* Per-member offsets mean different things in embedded mode
  (compressed chunk offsets) vs. sidecar mode (uncompressed tar byte offsets). The
  format would have to express "this field is valid only in mode X" rules and
  ship two parsing paths.
- *Crowded prior art.* [ratarmount](https://github.com/mxmlnkn/ratarmount) already
  ships a SQLite-based tar index. Users who want random access to plain tar have a
  deployed solution; introducing a competing format needs a stronger motivation
  than "we could."
- *Pitch dilution.* tarzan's value proposition is "drop-in seekable `.tar.zst`,
  standard tools still work." A sidecar mode reframes tarzan as a generic tar
  index format and pulls it into a different and more crowded design space.
- *Tape is not really solved by a TOC file alone.* Useful tape random access needs
  blocking-factor and (for multi-volume) volume-boundary metadata, not just member
  offsets. Claiming tape support without that would be misleading.

**Forward-compatibility reservations.** The v1 TOC schema is nevertheless designed
so a sidecar variant remains feasible later without breaking v1 readers:

- Every member entry carries `tar_offset` (uncompressed byte offset of the member
  header in the tar stream). This is independently useful for verification and is
  the field any future sidecar would need.
- A top-level `target` field (default `"embedded"`) is reserved. Readers must
  reject unknown values, so adding `"sidecar"` later is not a breaking change.
- Top-level `tar_sha256` and `tar_size` are reserved as optional fields, to be
  populated by future sidecars so readers can detect drift loudly rather than
  silently using stale offsets.

No file extension or on-disk sidecar layout is specified at this time — once
documented, it has to be supported.

### Why not GNU tar's `--index-file`

`tar --index-file=FILE` is sometimes proposed as the natural sidecar format, but it
is the wrong reference point. It redirects the `-v` listing to a file — bare paths
at `-v`, `ls -l`-style lines at `-vv`:

```text
drwxr-xr-x andrew/wheel      0 2026-05-18 16:29 ./
-rw-r--r-- andrew/wheel     10 2026-05-18 16:29 ./b.txt
-rw-r--r-- andrew/wheel      6 2026-05-18 16:29 ./sub/c.txt
```

There are no byte offsets, no checksums, no schema, no versioning, and no extension
hook. The file tells you *what* is in the archive, not *where*, so it cannot serve
as a seek index. Reusing the format would either ship a sidecar that does not
actually enable seeking, or extend it past the point of any compatibility with GNU
tar. [ratarmount](https://github.com/mxmlnkn/ratarmount)'s SQLite index is the
closest existing format that actually solves the random-access problem and is the
better reference if a sidecar mode is ever revisited.

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

MIT. See [LICENSE](./LICENSE).
