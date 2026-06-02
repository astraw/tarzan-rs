//! Random-access, seekable `.tar.zst` archives with an embedded
//! table-of-contents index.
//!
//! A tarzan archive is a valid zstd stream that divides the compressed data
//! into independently decodable chunks and appends a table of contents (TOC)
//! as a zstd skippable frame. The TOC stores filenames, permissions,
//! ownership, sizes, and per-chunk byte offsets, so contents can be listed
//! without decompression and individual files extracted by seeking directly
//! to their chunks.
//!
//! A command-line tool (`tarzan`) is also available — see the
//! [tarzan-rs repository](https://github.com/astraw/tarzan-rs).
//!
//! # AI-assisted development
//!
//! This crate was developed with substantial AI assistance. The implementation
//! was generated iteratively using Claude language models (Anthropic) — primarily
//! Claude Opus 4.7 and Claude Sonnet 4.6, with a small number of early commits
//! from Gemma 4 31B — under continuous human direction and review. Every commit
//! records the contributing model in the subject line. Correctness is validated
//! through the test suite (`cargo test`), CI on Linux, macOS, and Windows, and
//! iterative round-trip testing against real archives during development.
//!
//! # File format
//!
//! A tarzan archive is a valid zstd stream with four sections:
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────┐
//! │  Identity frame (skippable, 14 bytes)                   │
//! │  Magic: 0x184D2A54  Content: "TRZN" + type + version    │
//! ├─────────────────────────────────────────────────────────┤
//! │  Compressed data frames                                 │
//! │  Independent zstd frames sized around --chunk-size,     │
//! │  each carrying a 4-byte XXHash64 content checksum that  │
//! │  the standard zstd decoder verifies on decompression.   │
//! │  Large members split across several frames; small       │
//! │  members packed together to share a frame.              │
//! ├─────────────────────────────────────────────────────────┤
//! │  TOC frame (skippable)                                  │
//! │  Magic: 0x184D2A54  Content: zstd-compressed JSON TOC   │
//! ├─────────────────────────────────────────────────────────┤
//! │  Footer frame (skippable, 38 bytes)                     │
//! │  Magic: 0x184D2A54  Content: "TRZN" + type + version    │
//! │  + TOC offset (u64) + TOC size (u64) + XXHash64 (8 B)   │
//! │  Hash covers bytes 0..(file_size - 38), seeded with     │
//! │  the constant `ARCHIVE_HASH_SEED`.                      │
//! └─────────────────────────────────────────────────────────┘
//! ```
//!
//! The skippable frame magic `0x184D2A54` is shared by all four sections;
//! they are distinguished by a frame-type byte in the payload
//! (`0x01` identity, `0x02` TOC, `0x03` footer). The zstd spec defines any
//! value in `0x184D2A50`–`0x184D2A5F` as a skippable frame; tarzan-aware
//! readers identify tarzan frames via the `TRZN` ASCII identifier at offset 8,
//! not by the magic number alone.
//!
//! zstd frames are little-endian on disk, so `0x184D2A54` is written as the
//! byte sequence `54 2A 4D 18` — the first byte of every tarzan archive is
//! ASCII `T`.  A hex dump confirms the identity frame:
//!
//! ```text
//! $ xxd -l 14 archive.tar.zst
//! 00000000: 542a 4d18 0600 0000 5452 5a4e 0102       T*M.....TRZN..
//!           └── 0x184D2A54 ──┘           └TRZN┘
//! ```
//!
//! The version byte at offset 13 is `0x02` for the current format.
//!
//! Opening an archive reads two regions: the 14-byte identity frame at the
//! start and the 38-byte footer at the end. The footer carries the TOC's
//! byte offset and size, so the TOC is then fetched with a single seek — no
//! scanning, regardless of TOC size.
//!
//! ## Integrity layers
//!
//! - **Per data frame** — zstd's built-in XXHash64 content checksum is
//!   enabled on every chunk, so a corrupted compressed byte fails at
//!   decompress time with no extra work on the reader's side.
//! - **Per member** — each regular-file entry's TOC record carries an
//!   optional `content_sha256` (SHA-256, same format as `sha256sum`) and an
//!   optional `content_md5` (MD5, same format as `md5sum`, for interoperability
//!   with systems that expose MD5 checksums such as S3 ETags for
//!   single-PUT uploads). Both cover only the file's content bytes —
//!   no tar headers, no padding.
//! - **Whole archive** — the footer carries an XXHash64 over the entire
//!   archive prefix. `tarzan verify --quick` re-hashes the file in one
//!   sequential pass and compares; cheap end-to-end bit-rot detection
//!   that requires no decompression.
//!
//! ## TOC schema
//!
//! The TOC is a zstd-compressed JSON object:
//!
//! ```json
//! {
//!   "tarzan_version": 2,
//!   "members": [
//!     {
//!       "path": "src/main.rs",
//!       "type": "file",
//!       "size": 4301,
//!       "mode": 420,
//!       "uid": 1000,
//!       "gid": 1000,
//!       "mtime": 1730643742,
//!       "tar_offset": 1024,
//!       "content_sha256": "a948904f2f0f479b8f936f2b38bf5e9e2c5a7b5b5e7e3f7b6e5d4c3b2a19080f",
//!       "content_md5": "1b374e3a9f0c8d7b6e5a4f3c2d1b0a9e",
//!       "chunks": [
//!         {
//!           "compressed_offset": 1024,
//!           "compressed_size": 1891,
//!           "uncompressed_size": 4301
//!         }
//!       ]
//!     }
//!   ]
//! }
//! ```
//!
//! Writers keep `tarzan_version` at 2. New metadata is additive and optional:
//! fields such as `mtime_ns`, `atime`/`atime_ns`, `ctime`/`ctime_ns`,
//! `uname`, `gname`, `xattrs`, `path_bytes`, `link_target_bytes`, and
//! `raw_type_byte` may appear when present in the source tar metadata.
//!
//! Each chunk locates one member's bytes inside a compressed frame.  A member
//! larger than the chunk size spans several chunks; small members are packed
//! together to share a frame, and the optional `frame_offset` field (omitted
//! when zero) gives the member's byte offset within that frame's decompressed
//! data.
//!
//! ## zstd compatibility
//!
//! Every tarzan archive is a valid zstd stream.  Standard decoders skip the
//! identity, TOC, and footer skippable frames and decompress the data frames
//! normally:
//!
//! ```sh
//! zstd -d archive.tar.zst | tar x
//! tar --zstd -xf archive.tar.zst
//! ```
//!
//! The decompressed tar stream is bit-for-bit identical to the original.
//! What is lost is the index: listing or extracting via standard tools
//! requires a full sequential pass.
//!
//! # Usage
//!
//! ## Creating an archive
//!
//! [`wrap`] reads a raw tar stream and writes a tarzan-formatted `.tar.zst`:
//!
//! ```no_run
//! use std::fs::File;
//! use tarzan::WrapOptions;
//!
//! let input = File::open("archive.tar")?;
//! let output = File::create("archive.tar.zst")?;
//! tarzan::wrap(input, output, WrapOptions::default())?;
//! # Ok::<(), anyhow::Error>(())
//! ```
//!
//! [`WrapOptions`] controls chunk size and zstd compression level:
//!
//! ```no_run
//! # use std::fs::File;
//! # use tarzan::WrapOptions;
//! # let (input, output) = (File::open("a.tar")?, File::create("a.tar.zst")?);
//! tarzan::wrap(input, output, WrapOptions::default()
//!     .chunk_size(1024 * 1024)  // 1 MB chunks
//!     .level(9))?;
//! # Ok::<(), anyhow::Error>(())
//! ```
//!
//! ## Reading an archive
//!
//! [`TarzanReader`] opens an archive and gives access to the TOC without
//! decompressing any data frames:
//!
//! ```no_run
//! use std::path::Path;
//! use tarzan::TarzanReader;
//!
//! let reader = TarzanReader::open(Path::new("archive.tar.zst"))?;
//! for member in reader.members() {
//!     println!("{} ({} bytes)", member.path, member.size);
//! }
//! # Ok::<(), anyhow::Error>(())
//! ```
//!
//! ## Extracting a single member
//!
//! [`TarzanReader::extract_member`] seeks directly to the member's chunks and
//! decompresses only those frames:
//!
//! ```no_run
//! # use std::path::Path;
//! # use tarzan::TarzanReader;
//! let mut reader = TarzanReader::open(Path::new("archive.tar.zst"))?;
//! let mut out = std::fs::File::create("main.rs")?;
//! reader.extract_member("src/main.rs", &mut out)?;
//! # Ok::<(), anyhow::Error>(())
//! ```
//!
//! # Cargo features
//!
//! | Feature | Default | Description |
//! |---|---|---|
//! | `zstd-sys` | ✓ | Links the zstd C library via `zstd-sys`. Best performance. |
//! | `pure-rust` | | Pure-Rust zstd via `zstd-pure-rs`. No C toolchain needed. |
//!
//! Exactly one feature must be active. For cross-compilation without a C toolchain:
//!
//! ```toml
//! tarzan = { version = "0.2", default-features = false, features = ["pure-rust"] }
//! ```

mod extract;
pub mod filter;
pub mod format;
mod io;
mod reader;
mod wrap;
mod zstd_impl;

pub use crate::extract::ExtractOptions;
pub use crate::filter::PathFilter;
pub use crate::reader::{TarzanReader, VerifyRecord, VerifyStatus};
pub use crate::wrap::{WrapOptions, wrap, wrap_with};
