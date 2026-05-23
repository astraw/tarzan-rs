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

/// Tunables for opening a tarzan archive.
///
/// The defaults are appropriate for typical archives. Override
/// [`initial_scan_bytes`](Self::initial_scan_bytes) when opening very large
/// archives — the TOC scales with member count, and starting with a larger
/// initial read avoids a few doubling rounds on huge archives. Override
/// [`max_scan_bytes`](Self::max_scan_bytes) to relax or tighten the safety
/// ceiling on the scan window.
#[derive(Debug, Clone, Copy)]
pub struct ReaderOptions {
    initial_scan_bytes: u64,
    max_scan_bytes: u64,
}

impl Default for ReaderOptions {
    fn default() -> Self {
        Self {
            initial_scan_bytes: INITIAL_SCAN_BYTES,
            max_scan_bytes: MAX_SCAN_BYTES,
        }
    }
}

impl ReaderOptions {
    /// Initial size of the read window used when scanning back from EOF
    /// for the TOC frame. Must be at least 8 (one skippable-frame header).
    /// Values below 8 are silently raised to 8.
    pub fn initial_scan_bytes(mut self, bytes: u64) -> Self {
        self.initial_scan_bytes = bytes.max(8);
        self
    }

    /// Hard upper bound on the scan window. If the TOC frame is larger than
    /// this, the open fails with "no tarzan TOC frame found". The default
    /// is 1 GiB.
    pub fn max_scan_bytes(mut self, bytes: u64) -> Self {
        self.max_scan_bytes = bytes.max(8);
        self
    }
}

impl TarzanReader {
    /// Opens a tarzan archive file: validates the leading identity frame and
    /// loads the TOC by scanning back from the end of the file.
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_with_options(path, ReaderOptions::default())
    }

    /// Like [`open`](Self::open), but lets the caller tune the TOC scan
    /// window — useful when opening very large archives whose TOC may
    /// exceed the default initial read.
    pub fn open_with_options(path: &Path, opts: ReaderOptions) -> Result<Self> {
        let file =
            File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
        Self::from_seekable_with_options(file, opts)
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
    pub fn from_seekable<S: Read + Seek + 'static>(source: S) -> Result<Self> {
        Self::from_seekable_with_options(source, ReaderOptions::default())
    }

    /// Like [`from_seekable`](Self::from_seekable), but accepts a
    /// [`ReaderOptions`] for tuning the TOC scan window.
    pub fn from_seekable_with_options<S: Read + Seek + 'static>(
        mut source: S,
        opts: ReaderOptions,
    ) -> Result<Self> {
        let archive_size = source
            .seek(SeekFrom::End(0))
            .context("failed to seek to end of archive")?;
        let identity_version =
            read_identity_frame(&mut source).context("invalid identity frame")?;
        let toc = find_toc(&mut source, archive_size, &opts).context("no tarzan TOC found")?;
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
            let mut decoder = zstd::stream::read::Decoder::new(limited)
                .context("failed to create zstd decoder")?;

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

/// Initial window read from the end of the file when scanning for the TOC.
///
/// Most TOCs fit easily in this; oversized ones trigger the adaptive grow
/// below. Keeping the initial read modest avoids pulling tens of MB off
/// remote/cold storage just to open a small archive.
pub(crate) const INITIAL_SCAN_BYTES: u64 = 64 * 1024;

/// Hard upper bound on the scan window. A TOC frame larger than this is
/// treated as missing; the zstd skippable-frame length field is a `u32`,
/// so 4 GiB is the absolute ceiling and 1 GiB is a comfortable safety net.
pub(crate) const MAX_SCAN_BYTES: u64 = 1024 * 1024 * 1024;

struct TocLocation {
    members: Vec<TocMember>,
    offset: u64,
    frame_size: u64,
}

/// Scans backward from EOF for the TOC skippable frame, growing the read
/// window until the frame fits or the cap is reached.
fn find_toc<R: Read + Seek>(
    file: &mut R,
    file_size: u64,
    opts: &ReaderOptions,
) -> Result<TocLocation> {
    if file_size < 8 {
        bail!("file too small to be a tarzan archive");
    }

    let magic = format::SKIPPABLE_FRAME_MAGIC.to_le_bytes();
    let cap = opts.max_scan_bytes.min(file_size);
    let mut scan_size = opts.initial_scan_bytes.min(cap).max(8);

    loop {
        let scan_start = file_size - scan_size;
        file.seek(SeekFrom::Start(scan_start))
            .context("failed to seek for TOC scan")?;
        let mut buf = vec![0u8; scan_size as usize];
        file.read_exact(&mut buf)
            .context("failed to read tail of archive")?;

        let buf_len = buf.len() as u64;
        // Walk backwards through the buffer looking for a skippable frame that ends at EOF.
        for p in (0..=buf.len().saturating_sub(8)).rev() {
            if buf[p..p + 4] != magic {
                continue;
            }
            // Compare in u64 — payload_size is a u32 (up to ~4 GiB), so on
            // 32-bit targets the obvious `p + 8 + payload_size` could wrap.
            let payload_size = u32::from_le_bytes(buf[p + 4..p + 8].try_into().unwrap());
            if (p as u64) + 8 + (payload_size as u64) != buf_len {
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

        // Not found in this window. The TOC frame may be larger than our
        // current read; grow and retry until we hit the cap.
        if scan_size >= cap {
            break;
        }
        scan_size = scan_size.saturating_mul(2).min(cap);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::identity::identity_frame_v1;
    use crate::format::toc::{ChunkInfo, EntryType, TocFrame, TocMember, encode_toc_frame};
    use std::io::Cursor;

    /// Builds a synthetic tarzan archive in memory whose TOC frame, after
    /// zstd-compression, is at least `min_compressed_toc_bytes`.
    ///
    /// Each member is given a long pseudorandom path built from many concatenated
    /// SHA-256 hashes, which carries enough entropy that zstd compresses it at
    /// roughly 50%. The member count is therefore chosen so the projected
    /// compressed size comfortably exceeds the target, and the TOC is encoded
    /// exactly once (the previous batch-and-retry loop was O(n²)).
    fn synth_archive_with_large_toc(min_compressed_toc_bytes: usize) -> (Vec<u8>, usize) {
        // Path length per member, in characters. 8 × 64 chars = 512-byte path
        // contributes ~256 bytes compressed. The per-member lower bound is
        // deliberately conservative so the projected total comfortably exceeds
        // the target — empirically the actual rate is ~315 bytes/member.
        const HASHES_PER_PATH: u64 = 8;
        const PER_MEMBER_COMPRESSED_LOWER_BOUND: usize = 280;

        let target_members = (min_compressed_toc_bytes / PER_MEMBER_COMPRESSED_LOWER_BOUND).max(8);

        let mut members: Vec<TocMember> = Vec::with_capacity(target_members);
        for i in 0..target_members as u64 {
            let mut path = String::with_capacity((HASHES_PER_PATH * 64) as usize);
            for j in 0..HASHES_PER_PATH {
                path.push_str(&sha256_hex(&[i.to_le_bytes(), j.to_le_bytes()].concat()));
            }
            let sha = sha256_hex(&[b"chunk", i.to_le_bytes().as_slice()].concat());
            members.push(TocMember {
                path,
                entry_type: EntryType::File,
                size: 100,
                mode: 0o644,
                uid: 1000,
                gid: 1000,
                mtime: 0,
                tar_offset: i * 1024,
                link_target: None,
                chunks: vec![ChunkInfo {
                    compressed_offset: i * 1024,
                    compressed_size: 100,
                    uncompressed_size: 100,
                    frame_offset: 0,
                    sha256: Some(sha),
                }],
            });
        }

        let toc = TocFrame {
            tarzan_version: 1,
            members,
        };
        let toc_bytes = encode_toc_frame(&toc, 3).expect("encode toc");
        assert!(
            toc_bytes.len() >= min_compressed_toc_bytes,
            "synthetic TOC ({} bytes) is smaller than target ({}); bump HASHES_PER_PATH \
             or PER_MEMBER_COMPRESSED_ESTIMATE",
            toc_bytes.len(),
            min_compressed_toc_bytes
        );

        let identity = identity_frame_v1();
        // 0xFF filler can't accidentally match the skippable-frame magic (0x184D2A54).
        let filler = vec![0xFFu8; 1024];
        let mut archive = Vec::with_capacity(identity.len() + filler.len() + toc_bytes.len());
        archive.extend_from_slice(&identity);
        archive.extend_from_slice(&filler);
        archive.extend_from_slice(&toc_bytes);
        (archive, toc.members.len())
    }

    #[test]
    fn reader_finds_toc_frame_larger_than_default_initial_window() {
        // Forces at least one window-doubling round with default options.
        let target = (INITIAL_SCAN_BYTES as usize) * 4;
        let (archive, member_count) = synth_archive_with_large_toc(target);

        let reader = TarzanReader::from_seekable(Cursor::new(archive)).expect("open archive");
        assert_eq!(reader.members().len(), member_count);
    }

    #[test]
    fn reader_finds_toc_frame_larger_than_eight_megabytes() {
        // Regression for the original report: a TOC larger than the old hard
        // 8 MB scan cap. Confirms the adaptive grow reaches multi-MB frames.
        let (archive, member_count) = synth_archive_with_large_toc(8 * 1024 * 1024 + 1);

        let reader = TarzanReader::from_seekable(Cursor::new(archive)).expect("open archive");
        assert_eq!(reader.members().len(), member_count);
    }

    #[test]
    fn reader_options_initial_scan_bytes_skips_growth_for_huge_toc() {
        // Pre-sized initial window finds the TOC on the first read.
        let (archive, member_count) = synth_archive_with_large_toc(2 * 1024 * 1024);
        let opts = ReaderOptions::default().initial_scan_bytes(16 * 1024 * 1024);
        let reader = TarzanReader::from_seekable_with_options(Cursor::new(archive), opts)
            .expect("open archive");
        assert_eq!(reader.members().len(), member_count);
    }

    #[test]
    fn reader_options_max_scan_bytes_clamps_search() {
        // A 256 KB cap can't reach a multi-MB TOC, so open should fail
        // cleanly with the expected error.
        let (archive, _) = synth_archive_with_large_toc(2 * 1024 * 1024);
        let opts = ReaderOptions::default().max_scan_bytes(256 * 1024);
        let result = TarzanReader::from_seekable_with_options(Cursor::new(archive), opts);
        let err = match result {
            Ok(_) => panic!("open should fail when TOC exceeds max_scan_bytes"),
            Err(e) => e,
        };
        let msg = format!("{err:#}");
        assert!(
            msg.contains("no tarzan TOC frame found"),
            "unexpected error: {msg}"
        );
    }
}
