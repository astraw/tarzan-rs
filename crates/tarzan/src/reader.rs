use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::format::{self, toc::TocMember};

/// Reads a tarzan archive without decompressing the data frames.
pub struct TarzanReader {
    members: Vec<TocMember>,
}

impl TarzanReader {
    /// Opens a tarzan archive and loads its TOC by scanning from the end of the file.
    pub fn open(path: &Path) -> Result<Self> {
        let mut file =
            File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
        let file_size = file
            .seek(SeekFrom::End(0))
            .context("failed to seek to end of archive")?;
        let members = find_toc(&mut file, file_size)
            .with_context(|| format!("no tarzan TOC found in {}", path.display()))?;
        Ok(Self { members })
    }

    pub fn members(&self) -> &[TocMember] {
        &self.members
    }
}

/// Maximum number of bytes read from the end of the file when scanning for the TOC.
///
/// Real TOCs are small (JSON + zstd), so 8 MB is a generous upper bound.
const MAX_SCAN_BYTES: u64 = 8 * 1024 * 1024;

fn find_toc(file: &mut File, file_size: u64) -> Result<Vec<TocMember>> {
    if file_size < 8 {
        bail!("file too small to be a tarzan archive");
    }
    let scan_size = MAX_SCAN_BYTES.min(file_size) as usize;
    let scan_start = file_size - scan_size as u64;

    file.seek(SeekFrom::Start(scan_start))
        .context("failed to seek for TOC scan")?;
    let mut buf = vec![0u8; scan_size];
    file.read_exact(&mut buf)
        .context("failed to read tail of archive")?;

    let magic = format::SKIPPABLE_FRAME_MAGIC.to_le_bytes();

    // Walk backwards through the buffer looking for a skippable frame that ends at EOF.
    for p in (0..=buf.len().saturating_sub(8)).rev() {
        if buf[p..p + 4] != magic {
            continue;
        }
        let payload_size =
            u32::from_le_bytes(buf[p + 4..p + 8].try_into().unwrap()) as usize;
        if p + 8 + payload_size != buf.len() {
            continue; // frame doesn't end exactly at EOF
        }
        let payload = &buf[p + 8..];
        if payload.len() < 6 || &payload[0..4] != b"TRZN" {
            continue;
        }
        if payload[4] != format::FRAME_TYPE_TOC {
            continue;
        }
        let toc = crate::format::toc::decode_toc_payload(payload)
            .context("failed to decode TOC frame")?;
        return Ok(toc.members);
    }

    bail!("no tarzan TOC frame found")
}
