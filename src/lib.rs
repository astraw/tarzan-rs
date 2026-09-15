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
//! was generated iteratively using large language models — primarily Claude
//! Opus 4.7, Claude Sonnet 4.6, and Claude Fable 5.1 (Anthropic) and GPT-5.3,
//! 5.4, and 5.5 Codex (OpenAI), with a small number of early commits from Gemma
//! 4 31B (Google) — under continuous human direction and review. Every commit
//! records the contributing model in the subject line. Correctness is validated
//! through the test suite (`cargo test`), CI on Linux, macOS, and Windows
//! (including archives produced by each host's own `tar` and consumed on every
//! other host, archives from every published release, and old releases reading
//! archives from the current build), and iterative round-trip testing against
//! real archives during development.
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
//!   single-PUT uploads). `wrap` computes both by default; each can be
//!   disabled independently. Both cover only the file's content bytes — no
//!   tar headers, no padding.
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
//! `tarzan_version` mirrors the identity frame's version byte and readers
//! require the two to agree. New metadata is additive and optional: fields
//! such as `mtime_ns`, `atime`/`atime_ns`, `ctime`/`ctime_ns`, `uname`,
//! `gname`, `xattrs`, `path_bytes`, `link_target_bytes`, and `raw_type_byte`
//! appear when the source tar carried them and are absent otherwise.
//!
//! Guidance for third-party readers: prefer `path_bytes` and
//! `link_target_bytes` over the lossy `path`/`link_target` strings when
//! present; treat a chunk with `frame_offset` as a slice of a frame shared
//! with neighbouring members; never assume `content_sha256` or `content_md5`
//! is present on a member because another member carries it; and treat any
//! `type` value you do not recognise as `other`.
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
//! # Compatibility
//!
//! Four version numbers appear in an archive:
//!
//! | Where | Value | Reader behaviour |
//! |---|---|---|
//! | identity frame version byte (offset 13) | `2` | the format version; `1` is rejected as legacy, anything else as unsupported |
//! | `tarzan_version` in the TOC JSON | `2` | must equal the identity byte |
//! | TOC frame payload version byte | `1` | the JSON envelope; anything else is rejected |
//! | footer frame version byte | `1` | the footer layout; anything else is rejected |
//!
//! Within identity version 2, which every release since 0.2.0 writes:
//!
//! - **Backward compatibility is promised.** Any v2 archive opens, lists,
//!   extracts, and verifies in every later release. The TOC schema only gains
//!   optional fields; nothing is removed, renamed, or made required.
//! - **Forward compatibility is not promised but is tested.** Readers ignore
//!   unknown JSON fields at every level, so an archive from a newer release
//!   opens in an older one as long as the newer writer only added fields.
//!   The known break points are a `type` value the reader has never seen
//!   (the whole TOC fails to decode) and any change to the version bytes.
//!   CI checks that the first and the latest published releases read an
//!   archive from the current build; a change that breaks them lands
//!   knowingly.
//! - **Fixed for the life of v2:** the frame layout above, little-endian
//!   integers, zstd's per-frame XXHash64 content checksum, `content_sha256`
//!   and `content_md5` over content bytes only, and the footer XXHash64
//!   seeded with [`format::footer::ARCHIVE_HASH_SEED`]. Changing any of
//!   these means a new identity version.
//! - **Standard tools always work.** No dictionaries and no experimental
//!   frame features are used, so any RFC 8878 decoder plus any tar recovers
//!   the original stream from any tarzan archive, forever.
//! - **Not promised: identical bytes across releases.** The zstd library
//!   version changes the compressed bytes. Two releases wrapping the same tar
//!   with the same options produce different archives with identical
//!   metadata and identical decoded output. Within one release and one set
//!   of options, output is deterministic.
//!
//! Limits: the compressed TOC must fit a skippable frame (under 4 GiB), which
//! is a format limit; this reader additionally refuses a TOC that
//! decompresses to more than [`format::toc::MAX_TOC_DECOMPRESSED_BYTES`]
//! (1 GiB, roughly four million members), which is a reader safety limit
//! and not a property of the archive.
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
//! [`WrapOptions`] controls chunk size, zstd compression level, and checksum
//! emission policy:
//!
//! ```no_run
//! # use std::fs::File;
//! # use tarzan::WrapOptions;
//! # let (input, output) = (File::open("a.tar")?, File::create("a.tar.zst")?);
//! tarzan::wrap(input, output, WrapOptions::default()
//!     .level(9))?;  // zstd compression level
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
//! ## Extracting to a directory
//!
//! [`TarzanReader::extract_to_dir`] materialises members on the filesystem
//! under the contract *content is guaranteed, metadata is attempted, and the
//! difference is reported*: a regular file either lands byte-exact or is
//! declined, and every attribute the host could not apply (an xattr
//! namespace it lacks, a symlink it cannot create, a name the filesystem
//! folds onto another) is recorded in the returned [`FidelityReport`] while
//! extraction continues. [`ExtractOptions::strict`] turns a non-empty report
//! into a [`StrictFidelityError`] after the tree has been extracted as far as
//! possible.
//!
//! ```no_run
//! # use std::path::Path;
//! # use tarzan::{ExtractOptions, TarzanReader};
//! let mut reader = TarzanReader::open(Path::new("archive.tar.zst"))?;
//! let report = reader.extract_to_dir(Path::new("out"), &ExtractOptions::default(), |_| {})?;
//! if !report.is_clean() {
//!     eprintln!("{}", report.summary());
//! }
//! # Ok::<(), anyhow::Error>(())
//! ```
//!
//! # Dependencies
//!
//! Compression is provided by the [`zstd`](https://docs.rs/zstd) crate, which
//! links the zstd C library statically. There are no Cargo features.

mod extract;
pub mod filter;
pub mod format;
mod io;
mod reader;
mod wrap;
mod zstd_impl;

pub use crate::extract::{ExtractOptions, FidelityReport, Loss, LossKind, StrictFidelityError};
pub use crate::filter::PathFilter;
pub use crate::reader::{TarzanReader, VerifyRecord, VerifyStatus};
pub use crate::wrap::{WrapOptions, wrap, wrap_with};
