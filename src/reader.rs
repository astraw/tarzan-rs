use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::format::{
    self,
    toc::{EntryType, TocMember},
};

/// Reads a tarzan archive without decompressing the data frames.
pub struct TarzanReader {
    path: PathBuf,
    members: Vec<TocMember>,
    archive_size: u64,
    toc_offset: u64,
    toc_frame_size: u64,
    identity_version: u8,
}

/// Result of verifying one chunk's stored SHA-256 checksum.
pub struct VerifyRecord {
    pub path: String,
    pub chunk_index: usize,
    pub status: VerifyStatus,
}

pub enum VerifyStatus {
    Ok,
    Mismatch { expected: String, actual: String },
    NoChecksum,
}

impl TarzanReader {
    /// Opens a tarzan archive: validates the leading identity frame and
    /// loads the TOC by scanning back from the end of the file.
    pub fn open(path: &Path) -> Result<Self> {
        let mut file =
            File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
        let archive_size = file
            .seek(SeekFrom::End(0))
            .context("failed to seek to end of archive")?;
        let identity_version = read_identity_frame(&mut file)
            .with_context(|| format!("invalid identity frame in {}", path.display()))?;
        let toc = find_toc(&mut file, archive_size)
            .with_context(|| format!("no tarzan TOC found in {}", path.display()))?;
        Ok(Self {
            path: path.to_owned(),
            members: toc.members,
            archive_size,
            toc_offset: toc.offset,
            toc_frame_size: toc.frame_size,
            identity_version,
        })
    }

    pub fn members(&self) -> &[TocMember] {
        &self.members
    }

    /// Total size of the archive file on disk, in bytes.
    pub fn archive_size(&self) -> u64 {
        self.archive_size
    }

    /// Byte offset of the TOC skippable frame from the start of the file.
    pub fn toc_offset(&self) -> u64 {
        self.toc_offset
    }

    /// Total size of the TOC skippable frame (8-byte header plus payload).
    pub fn toc_frame_size(&self) -> u64 {
        self.toc_frame_size
    }

    /// Version byte from the leading identity frame.
    pub fn identity_version(&self) -> u8 {
        self.identity_version
    }

    /// Extracts the file data for `target_path` to `out`.
    ///
    /// Seeks directly to the member's compressed chunks; decompresses only
    /// those chunks. A member whose data exceeds the wrap-time chunk size
    /// spans several chunks, which are decoded in sequence. Returns an error
    /// if the path is not found or the member is not a regular file.
    pub fn extract_member(&self, target_path: &str, out: &mut dyn Write) -> Result<()> {
        let (member_idx, member) = self
            .members
            .iter()
            .enumerate()
            .find(|(_, m)| m.path == target_path)
            .ok_or_else(|| anyhow::anyhow!("path not found in archive: {target_path}"))?;

        if !matches!(member.entry_type, EntryType::File) {
            bail!("{target_path} is not a regular file");
        }
        if member.chunks.is_empty() {
            bail!("member has no chunks: {target_path}");
        }

        // Chunks are contiguous in the raw tar stream. chunk_tar_start is the sum of
        // uncompressed sizes of all chunks in all preceding members.
        let chunk_tar_start: u64 = self.members[..member_idx]
            .iter()
            .flat_map(|m| m.chunks.iter())
            .map(|c| c.uncompressed_size)
            .sum();

        // Offset of the file data within the concatenation of this member's
        // chunks: skip past any extension headers and the 512-byte tar header.
        let data_offset = member.tar_offset - chunk_tar_start + 512;

        let mut file = File::open(&self.path)
            .with_context(|| format!("failed to open {}", self.path.display()))?;

        let mut skip = data_offset;
        let mut remaining = member.size;
        for chunk in &member.chunks {
            if remaining == 0 {
                break;
            }
            if skip >= chunk.uncompressed_size {
                skip -= chunk.uncompressed_size;
                continue;
            }

            file.seek(SeekFrom::Start(chunk.compressed_offset))
                .context("failed to seek to chunk")?;
            let limited = (&mut file).take(chunk.compressed_size);
            let mut decoder =
                zstd::stream::read::Decoder::new(limited).context("failed to create zstd decoder")?;

            crate::io::skip_exact(&mut decoder, skip)
                .context("failed to skip to file data in chunk")?;
            let available = chunk.uncompressed_size - skip;
            let take = available.min(remaining);
            crate::io::copy_exact(&mut decoder, out, take).context("failed to copy file data")?;
            skip = 0;
            remaining -= take;
        }

        if remaining != 0 {
            bail!("archive truncated: {target_path} is missing {remaining} bytes of data");
        }

        Ok(())
    }

    /// Verifies the SHA-256 checksum of every chunk in every member.
    pub fn verify_all(&self) -> Result<Vec<VerifyRecord>> {
        let mut file = File::open(&self.path)
            .with_context(|| format!("failed to open {}", self.path.display()))?;
        verify_members(&mut file, self.members.iter())
    }

    /// Verifies the SHA-256 checksums for the single member at `target_path`.
    pub fn verify_member(&self, target_path: &str) -> Result<Vec<VerifyRecord>> {
        let member = self
            .members
            .iter()
            .find(|m| m.path == target_path)
            .ok_or_else(|| anyhow::anyhow!("path not found in archive: {target_path}"))?;
        let mut file = File::open(&self.path)
            .with_context(|| format!("failed to open {}", self.path.display()))?;
        verify_members(&mut file, std::iter::once(member))
    }
}

fn verify_members<'a>(
    file: &mut File,
    members: impl Iterator<Item = &'a TocMember>,
) -> Result<Vec<VerifyRecord>> {
    let mut results = Vec::new();
    for member in members {
        for (chunk_index, chunk) in member.chunks.iter().enumerate() {
            let status = match &chunk.sha256 {
                None => VerifyStatus::NoChecksum,
                Some(expected) => {
                    file.seek(SeekFrom::Start(chunk.compressed_offset))
                        .with_context(|| {
                            format!("seek failed for chunk {chunk_index} of {}", member.path)
                        })?;
                    let mut limited = (&mut *file).take(chunk.compressed_size);
                    let decompressed =
                        zstd::stream::decode_all(&mut limited).with_context(|| {
                            format!(
                                "decompress failed for chunk {chunk_index} of {}",
                                member.path
                            )
                        })?;
                    let actual = sha256_hex(&decompressed);
                    if actual == *expected {
                        VerifyStatus::Ok
                    } else {
                        VerifyStatus::Mismatch {
                            expected: expected.clone(),
                            actual,
                        }
                    }
                }
            };
            results.push(VerifyRecord {
                path: member.path.clone(),
                chunk_index,
                status,
            });
        }
    }
    Ok(results)
}

fn sha256_hex(data: &[u8]) -> String {
    let hash = Sha256::digest(data);
    hash.iter().map(|b| format!("{b:02x}")).collect()
}

/// Maximum number of bytes read from the end of the file when scanning for the TOC.
///
/// Real TOCs are small (JSON + zstd), so 8 MB is a generous upper bound.
const MAX_SCAN_BYTES: u64 = 8 * 1024 * 1024;

struct TocLocation {
    members: Vec<TocMember>,
    offset: u64,
    frame_size: u64,
}

fn find_toc(file: &mut File, file_size: u64) -> Result<TocLocation> {
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
        let payload_size = u32::from_le_bytes(buf[p + 4..p + 8].try_into().unwrap()) as usize;
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
        return Ok(TocLocation {
            members: toc.members,
            offset: scan_start + p as u64,
            frame_size: 8 + payload_size as u64,
        });
    }

    bail!("no tarzan TOC frame found")
}

/// Reads and validates the leading identity frame, returning its version byte.
fn read_identity_frame(file: &mut File) -> Result<u8> {
    file.seek(SeekFrom::Start(0))
        .context("failed to seek to start of archive")?;
    let mut header = [0u8; 8];
    file.read_exact(&mut header)
        .context("failed to read identity frame header")?;
    let magic = u32::from_le_bytes(header[0..4].try_into().unwrap());
    if magic != format::SKIPPABLE_FRAME_MAGIC {
        bail!(
            "not a tarzan archive: leading frame magic is {magic:#010x}, expected {:#010x}",
            format::SKIPPABLE_FRAME_MAGIC
        );
    }
    let payload_size = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
    let mut payload = vec![0u8; payload_size];
    file.read_exact(&mut payload)
        .context("failed to read identity frame payload")?;
    format::identity::decode(&payload)
}
