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
```

### `tarzan create` — create an archive from files

```sh
# Create from a directory
tarzan create -f archive.tar.zst ./my-project

# Multiple paths
tarzan create -f archive.tar.zst ./src ./docs ./README.md

# Write to stdout
tarzan create -f - ./my-project > archive.tar.zst

# Exclude patterns
tarzan create -f archive.tar.zst ./my-project --exclude '*.o' --exclude target/
```

### `tarzan list` — list contents

Reads only the TOC skippable frame. Fast regardless of archive size.
Aliased as `tarzan t` (tar style) and `tarzan ls`.

```sh
tarzan list -f archive.tar.zst

# Long format (permissions, size, mtime) — equivalent to `tar -tvf`
tarzan list -v -f archive.tar.zst

# tar-style short alias
tarzan t -f archive.tar.zst

# Filter by path prefix
tarzan list -f archive.tar.zst src/

# Machine-readable JSON
tarzan list --json -f archive.tar.zst
```

Example output:
```
src/main.rs                  4.2 KB   2024-11-03 14:22
src/lib.rs                   12.1 KB  2024-11-03 14:22
src/format/mod.rs            8.7 KB   2024-11-01 09:14
src/format/toc.rs            6.3 KB   2024-11-01 09:14
tests/roundtrip.rs           3.1 KB   2024-10-28 16:55
Cargo.toml                   1.1 KB   2024-11-03 14:20
README.md                    9.4 KB   2024-11-03 15:01
```

### `tarzan extract` — extract files

```sh
# Extract everything
tarzan extract -f archive.tar.zst

# Extract to a specific directory
tarzan extract -f archive.tar.zst -C /tmp/out

# Extract specific files (decompresses only relevant chunks)
tarzan extract -f archive.tar.zst src/main.rs src/lib.rs

# Extract a directory subtree
tarzan extract -f archive.tar.zst src/
```

### `tarzan cat` — stream a single file to stdout

Seeks directly to the file using the TOC; decompresses only its chunks.

```sh
tarzan cat -f archive.tar.zst src/main.rs

# Pipe into another tool
tarzan cat -f archive.tar.zst data/records.csv | awk -F, '{print $2}'
```

### `tarzan info` — show archive metadata

```sh
tarzan info -f archive.tar.zst
```

```
Format:          tarzan v1
Created:         2024-11-03 15:01:22 UTC
Members:         1,847
Uncompressed:    2.31 GB
Compressed:      487 MB  (21.1%)
Chunks:          4,203
Chunk size:      4 MB (default)
TOC size:        312 KB
TOC offset:      487,204,816
Identity frame:  TRZN v1
```

### `tarzan verify` — verify chunk checksums

```sh
# Verify all chunk SHA-256s
tarzan verify -f archive.tar.zst

# Verify a specific file
tarzan verify -f archive.tar.zst src/main.rs
```

---

## File Format

A tarzan archive is a valid zstd stream consisting of three sections:

```
┌─────────────────────────────────────────────────────────┐
│  Identity frame (skippable)                             │
│  Magic: 0x184D2A54  Content: "TRZN" + version byte      │
├─────────────────────────────────────────────────────────┤
│  Compressed data frames                                  │
│  One or more independent zstd frames per tar member.    │
│  Each frame corresponds to one chunk of one member.     │
│  Large members are split at --chunk-size boundaries.    │
│  Small members may be grouped into a single frame.      │
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
# archive.tar.zst: tarzan archive v1, 1847 members
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
- **Renaming `wrap`.** `wrap` reads an existing tar stream and adds the tarzan
  envelope. There is no tar verb for this, and reusing one (e.g. `create`)
  would mislead about what reads the file system.
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

† Slightly lower than monolithic `.tar.zst` due to per-chunk independent compression,
which loses cross-member redundancy. For most archives the difference is under 5%.

---

## Library usage

The `tarzan` crate exposes a library API for embedding tarzan support in other tools.

```toml
[dependencies]
tarzan = "1.0"
```

```rust
use tarzan::{Writer, Reader, WrapOptions};
use std::fs::File;

// Wrap an existing tar stream
let input = File::open("archive.tar")?;
let output = File::create("archive.tar.zst")?;
let opts = WrapOptions::default().chunk_size(4 * 1024 * 1024);
tarzan::wrap(input, output, opts)?;

// Read the TOC without decompression
let reader = Reader::open("archive.tar.zst")?;
for entry in reader.entries() {
    println!("{} ({} bytes)", entry.path(), entry.size());
}

// Extract a single file
let mut out = File::create("main.rs")?;
reader.extract_file("src/main.rs", &mut out)?;
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

```
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

MIT. See [LICENSE](LICENSE).
