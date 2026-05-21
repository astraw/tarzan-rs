use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::format::{
    self,
    toc::{EntryType, TocMember},
};

/// A seekable byte source a [`TarzanReader`] can read an archive from.
///
/// Blanket-implemented for every `Read + Seek` type — a `File`, an
/// in-memory `Cursor`, or a custom reader backed by HTTP range requests.
trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}

/// Reads a tarzan archive without decompressing the data frames.
///
/// Methods that touch the underlying byte source (`extract_member`, the
/// `verify` methods) take `&mut self`, since reading a chunk seeks the
/// source. Pure TOC accessors take `&self`.
pub struct TarzanReader {
    source: Box<dyn ReadSeek>,
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
    /// Opens a tarzan archive file: validates the leading identity frame and
    /// loads the TOC by scanning back from the end of the file.
    pub fn open(path: &Path) -> Result<Self> {
        let file =
            File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
        Self::from_seekable(file)
            .with_context(|| format!("reading tarzan archive {}", path.display()))
    }

    /// Opens a tarzan archive from any seekable byte source — a file, an
    /// in-memory [`Cursor`](std::io::Cursor), or a custom reader backed by
    /// HTTP range requests.
    ///
    /// The identity frame and TOC are read up front (a seek to the end of the
    /// source and back); member data is read lazily by `extract_member` and
    /// the `verify` methods. Each member's chunk byte ranges
    /// (`compressed_offset` / `compressed_size`) are then available via
    /// [`members`](Self::members), so a caller can fetch them in parallel.
    pub fn from_seekable<S: Read + Seek + 'static>(mut source: S) -> Result<Self> {
        let archive_size = source
            .seek(SeekFrom::End(0))
            .context("failed to seek to end of archive")?;
        let identity_version =
            read_identity_frame(&mut source).context("invalid identity frame")?;
        let toc = find_toc(&mut source, archive_size).context("no tarzan TOC found")?;
        Ok(Self {
            source: Box::new(source),
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
    pub fn extract_member(&mut self, target_path: &str, out: &mut dyn Write) -> Result<()> {
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

        let source = &mut self.source;

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

            source
                .seek(SeekFrom::Start(chunk.compressed_offset))
                .context("failed to seek to chunk")?;
            let limited = (&mut *source).take(chunk.compressed_size);
            let mut decoder =
                zstd::stream::read::Decoder::new(limited).context("failed to create zstd decoder")?;

            // `frame_offset` skips past other members sharing this frame; `skip`
            // then skips this member's own extension headers and tar header.
            crate::io::skip_exact(&mut decoder, chunk.frame_offset + skip)
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
    pub fn verify_all(&mut self) -> Result<Vec<VerifyRecord>> {
        verify_members(&mut self.source, self.members.iter())
    }

    /// Verifies the SHA-256 checksums for the single member at `target_path`.
    pub fn verify_member(&mut self, target_path: &str) -> Result<Vec<VerifyRecord>> {
        let member = self
            .members
            .iter()
            .find(|m| m.path == target_path)
            .ok_or_else(|| anyhow::anyhow!("path not found in archive: {target_path}"))?;
        verify_members(&mut self.source, std::iter::once(member))
    }
}

fn verify_members<'a, R: Read + Seek>(
    file: &mut R,
    members: impl Iterator<Item = &'a TocMember>,
) -> Result<Vec<VerifyRecord>> {
    let mut results = Vec::new();
    // Members can share a frame (small-member grouping), so decode each
    // distinct frame only once, keyed by its compressed offset.
    let mut frame_hashes: HashMap<u64, String> = HashMap::new();
    for member in members {
        for (chunk_index, chunk) in member.chunks.iter().enumerate() {
            let status = match &chunk.sha256 {
                None => VerifyStatus::NoChecksum,
                Some(expected) => {
                    let actual = match frame_hashes.get(&chunk.compressed_offset) {
                        Some(hash) => hash.clone(),
                        None => {
                            file.seek(SeekFrom::Start(chunk.compressed_offset))
                                .with_context(|| {
                                    format!(
                                        "seek failed for chunk {chunk_index} of {}",
                                        member.path
                                    )
                                })?;
                            let mut limited = (&mut *file).take(chunk.compressed_size);
                            let decompressed = zstd::stream::decode_all(&mut limited)
                                .with_context(|| {
                                    format!(
                                        "decompress failed for chunk {chunk_index} of {}",
                                        member.path
                                    )
                                })?;
                            let hash = sha256_hex(&decompressed);
                            frame_hashes.insert(chunk.compressed_offset, hash.clone());
                            hash
                        }
                    };
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

fn find_toc<R: Read + Seek>(file: &mut R, file_size: u64) -> Result<TocLocation> {
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
fn read_identity_frame<R: Read + Seek>(file: &mut R) -> Result<u8> {
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
