use std::io::Write;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::{
    FRAME_TYPE_TOC,
    identity::{IDENTITY_MAGIC, SKIPPABLE_FRAME_MAGIC},
};

pub const TOC_VERSION_V1: u8 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TocFrame {
    pub tarzan_version: u8,
    pub members: Vec<TocMember>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TocMember {
    pub path: String,
    #[serde(rename = "type")]
    pub entry_type: EntryType,
    pub size: u64,
    pub mode: u32,
    pub uid: u64,
    pub gid: u64,
    pub mtime: i64,
    pub tar_offset: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link_target: Option<String>,
    /// SHA-256 of the member's file content (no headers, no padding), as
    /// 64-character lowercase hex. Populated for regular files; `None` for
    /// directories, symlinks, hard links, and device nodes. Format and value
    /// match `sha256sum`'s output, so users can verify against on-disk files
    /// without invoking tarzan.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_sha256: Option<String>,
    pub chunks: Vec<ChunkInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EntryType {
    File,
    Dir,
    Symlink,
    HardLink,
    CharDevice,
    BlockDevice,
    Fifo,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkInfo {
    pub compressed_offset: u64,
    pub compressed_size: u64,
    pub uncompressed_size: u64,
    /// Offset of this member's bytes within the frame's decompressed output.
    /// Zero unless the member shares a frame with other (small) members; see
    /// the grouping notes in the format documentation.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub frame_offset: u64,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// Encodes a `TocFrame` as a tarzan skippable frame ready to append to an archive.
///
/// Payload layout: `TRZN` + `FRAME_TYPE_TOC` + `TOC_VERSION_V1` + zstd-compressed JSON.
///
/// Equivalent to [`write_toc_frame`] but collects the bytes into a `Vec` — prefer
/// `write_toc_frame` in the wrap path so we never materialise the whole frame in
/// memory.
pub fn encode_toc_frame(toc: &TocFrame, level: i32) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    write_toc_frame(&mut out, toc, level)?;
    Ok(out)
}

/// Writes the TOC frame directly to `out` and returns the number of bytes
/// written.
///
/// Avoids the two largest allocations that `encode_toc_frame` historically held
/// simultaneously: the uncompressed JSON `Vec<u8>` and the assembled-frame
/// `Vec<u8>`. We still buffer the compressed payload (the skippable-frame
/// header has to carry its length, so we cannot start writing it until we know
/// the size); for typical TOCs that buffer is ~10× smaller than the JSON.
pub fn write_toc_frame<W: Write>(out: &mut W, toc: &TocFrame, level: i32) -> Result<u64> {
    // Stream serde_json straight through the zstd encoder so the uncompressed
    // JSON never exists as a single allocation.
    let mut compressed: Vec<u8> = Vec::new();
    {
        let mut encoder = zstd::stream::write::Encoder::new(&mut compressed, level)
            .context("failed to create zstd encoder for TOC")?;
        serde_json::to_writer(&mut encoder, toc).context("failed to serialize TOC to JSON")?;
        encoder.finish().context("failed to finish TOC zstd frame")?;
    }

    // Skippable-frame payload: TRZN + frame type + version + compressed JSON.
    let payload_len = IDENTITY_MAGIC.len() + 2 + compressed.len();
    if payload_len > u32::MAX as usize {
        bail!(
            "compressed TOC payload ({payload_len} bytes) exceeds the {} byte \
             skippable-frame limit; archive has too many members",
            u32::MAX
        );
    }

    out.write_all(&SKIPPABLE_FRAME_MAGIC.to_le_bytes())
        .context("failed to write TOC frame magic")?;
    out.write_all(&(payload_len as u32).to_le_bytes())
        .context("failed to write TOC frame length")?;
    out.write_all(&IDENTITY_MAGIC)
        .context("failed to write TOC payload identifier")?;
    out.write_all(&[FRAME_TYPE_TOC, TOC_VERSION_V1])
        .context("failed to write TOC frame header bytes")?;
    out.write_all(&compressed)
        .context("failed to write compressed TOC payload")?;

    Ok(8u64 + payload_len as u64)
}

/// Hard cap on the decompressed size of the TOC's JSON payload. A producer
/// can legally write a zstd skippable frame up to ~4 GiB, which could
/// decompress to many times that — refusing here defends against accidental
/// zip-bomb-shaped TOCs and against malicious input. 1 GiB of JSON
/// corresponds to roughly 4 million members at typical entry sizes; if you
/// genuinely have a larger archive, raise the cap deliberately.
pub const MAX_TOC_DECOMPRESSED_BYTES: u64 = 1024 * 1024 * 1024;

/// Decodes a TOC-frame payload (everything after the 8-byte skippable-frame header).
///
/// Expects: `TRZN` + `FRAME_TYPE_TOC` + version byte + zstd-compressed JSON.
pub fn decode_toc_payload(payload: &[u8]) -> Result<TocFrame> {
    use std::io::Read;
    if payload.len() < 6 {
        bail!(
            "TOC payload too short: {} bytes (expected ≥6)",
            payload.len()
        );
    }
    if payload[0..4] != IDENTITY_MAGIC {
        bail!("TOC payload does not begin with TRZN");
    }
    if payload[4] != FRAME_TYPE_TOC {
        bail!("unexpected frame type in TOC payload: {:#04x}", payload[4]);
    }
    let version = payload[5];
    if version != TOC_VERSION_V1 {
        bail!("unsupported TOC version: {version}");
    }
    // Take 1 extra byte past the limit so we can distinguish "exactly at
    // limit" from "over limit" with a single read.
    let mut decoder = zstd::stream::read::Decoder::new(std::io::Cursor::new(&payload[6..]))
        .context("failed to create zstd decoder for TOC")?;
    let mut json = Vec::new();
    decoder
        .by_ref()
        .take(MAX_TOC_DECOMPRESSED_BYTES + 1)
        .read_to_end(&mut json)
        .context("failed to decompress TOC JSON")?;
    if json.len() as u64 > MAX_TOC_DECOMPRESSED_BYTES {
        bail!("decompressed TOC exceeds the {MAX_TOC_DECOMPRESSED_BYTES}-byte safety cap");
    }
    serde_json::from_slice(&json).context("failed to deserialize TOC JSON")
}
