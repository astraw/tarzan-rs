use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::{FRAME_TYPE_TOC, encode_skippable_frame, identity::IDENTITY_MAGIC};

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// Encodes a `TocFrame` as a tarzan skippable frame ready to append to an archive.
///
/// Payload layout: `TRZN` + `FRAME_TYPE_TOC` + `TOC_VERSION_V1` + zstd-compressed JSON.
pub fn encode_toc_frame(toc: &TocFrame, level: i32) -> Result<Vec<u8>> {
    let json = serde_json::to_vec(toc).context("failed to serialize TOC to JSON")?;
    let compressed = zstd::bulk::compress(&json, level).context("failed to compress TOC JSON")?;
    let payload = [
        IDENTITY_MAGIC.as_slice(),
        &[FRAME_TYPE_TOC, TOC_VERSION_V1],
        compressed.as_slice(),
    ]
    .concat();
    Ok(encode_skippable_frame(&payload))
}

/// Decodes a TOC-frame payload (everything after the 8-byte skippable-frame header).
///
/// Expects: `TRZN` + `FRAME_TYPE_TOC` + version byte + zstd-compressed JSON.
pub fn decode_toc_payload(payload: &[u8]) -> Result<TocFrame> {
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
    let json = zstd::stream::decode_all(std::io::Cursor::new(&payload[6..]))
        .context("failed to decompress TOC JSON")?;
    serde_json::from_slice(&json).context("failed to deserialize TOC JSON")
}
