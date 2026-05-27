use std::fs::File;
use std::hash::Hasher;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use twox_hash::XxHash64;

use crate::format::{
    self,
    footer::{ARCHIVE_HASH_SEED, FOOTER_FRAME_SIZE, Footer, decode_footer_payload},
    identity::{IDENTITY_VERSION_V1_LEGACY, IDENTITY_VERSION_V2},
    toc::{EntryType, TocMember, decode_toc_payload},
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
    archive_xxhash64: u64,
}

/// Result of verifying one member's stored SHA-256 content checksum.
pub struct VerifyRecord {
    pub path: String,
    pub status: VerifyStatus,
}

pub enum VerifyStatus {
    Ok,
    Mismatch { expected: String, actual: String },
    NoChecksum,
}

impl TarzanReader {
    /// Opens a tarzan archive file: validates the leading identity frame, reads
    /// the trailing footer to find the TOC, and loads the TOC.
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
    /// Two reads happen up front: the leading identity frame (14 bytes) and the
    /// trailing footer (62 bytes); the footer carries the TOC offset, so the
    /// TOC is then fetched with a single seek. Member data is read lazily by
    /// [`extract_member`](Self::extract_member) and the `verify` methods.
    pub fn from_seekable<S: Read + Seek + 'static>(mut source: S) -> Result<Self> {
        let archive_size = source
            .seek(SeekFrom::End(0))
            .context("failed to seek to end of archive")?;
        let identity_version =
            read_identity_frame(&mut source).context("invalid identity frame")?;
        match identity_version {
            IDENTITY_VERSION_V2 => {}
            IDENTITY_VERSION_V1_LEGACY => bail!(
                "Legacy v1 format. Please decode files using zstd -d and \
                re-wrap them."
            ),
            other => bail!(
                "unsupported tarzan format version {other}; this build understands v{IDENTITY_VERSION_V2}"
            ),
        }

        let footer = read_footer(&mut source, archive_size).context("failed to read footer")?;
        let toc_frame_size = footer.toc_frame_size;
        let toc_offset = footer.toc_offset;

        // Sanity: the footer's TOC pointer must land inside the archive prefix
        // (between the identity frame and the start of the footer itself).
        let prefix_end = archive_size - FOOTER_FRAME_SIZE;
        if toc_offset >= prefix_end || toc_offset + toc_frame_size != prefix_end {
            bail!(
                "footer points to TOC at {toc_offset}+{toc_frame_size}, \
                 which doesn't match the archive layout (prefix ends at {prefix_end})"
            );
        }

        let members = read_toc(&mut source, toc_offset, toc_frame_size)
            .context("failed to read TOC frame")?;

        // Bound every chunk byte range to the archive's data region and
        // reject overlap between distinct frames. Distinct members can share
        // a frame (same offset+size), so dedupe before the overlap walk.
        validate_chunk_layout(&members, toc_offset)
            .context("TOC contains invalid chunk offsets")?;

        Ok(Self {
            source: Box::new(source),
            members,
            archive_size,
            toc_offset,
            toc_frame_size,
            identity_version,
            archive_xxhash64: footer.archive_xxhash64,
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

    /// Version byte from the leading identity frame. Always 2 for archives
    /// produced by this build.
    pub fn identity_version(&self) -> u8 {
        self.identity_version
    }

    /// XXHash64 of bytes 0..(archive_size - 38) — i.e. the identity frame,
    /// every data frame, and the TOC frame, but not the footer that carries
    /// this hash. Seeded with [`ARCHIVE_HASH_SEED`]. Fast end-to-end
    /// integrity check; not a cryptographic hash.
    pub fn archive_xxhash64(&self) -> u64 {
        self.archive_xxhash64
    }

    /// Re-reads the archive prefix (everything before the footer), hashes it,
    /// and compares against the hash stored in the footer.
    ///
    /// `Ok(())` means the archive is intact end-to-end; `Err` carries the
    /// expected and observed hashes. This is the fast integrity check —
    /// one sequential read plus an XXHash64, with no zstd decompression.
    pub fn verify_archive_hash(&mut self) -> Result<()> {
        let prefix_end = self
            .archive_size
            .checked_sub(FOOTER_FRAME_SIZE)
            .context("archive too small to contain a footer")?;
        self.source
            .seek(SeekFrom::Start(0))
            .context("failed to seek to start of archive")?;
        let mut hasher = XxHash64::with_seed(ARCHIVE_HASH_SEED);
        let mut remaining = prefix_end;
        let mut buf = vec![0u8; 1024 * 1024];
        while remaining > 0 {
            let want = remaining.min(buf.len() as u64) as usize;
            self.source
                .read_exact(&mut buf[..want])
                .context("failed to read archive prefix")?;
            hasher.write(&buf[..want]);
            remaining -= want as u64;
        }
        let actual = hasher.finish();
        if actual != self.archive_xxhash64 {
            bail!(
                "whole-archive hash mismatch: expected {:016x}, computed {:016x}",
                self.archive_xxhash64,
                actual
            );
        }
        Ok(())
    }

    /// Extracts the file data for `target_path` to `out`.
    ///
    /// Seeks directly to the member's compressed chunks; decompresses only
    /// those chunks. A member whose data exceeds the wrap-time chunk size
    /// spans several chunks, which are decoded in sequence. Returns an error
    /// if the path is not found or the member is not a regular file.
    pub fn extract_member(&mut self, target_path: &str, out: &mut dyn Write) -> Result<()> {
        let member_idx = self
            .members
            .iter()
            .position(|m| m.path == target_path)
            .ok_or_else(|| anyhow::anyhow!("path not found in archive: {target_path}"))?;
        extract_by_index(&mut self.source, &self.members, member_idx, out)
    }

    /// Verifies every regular-file member's content against the SHA-256
    /// recorded in the TOC, decompressing each chunk as needed. Zstd's own
    /// per-frame checksum is verified automatically along the way.
    pub fn verify_all(&mut self) -> Result<Vec<VerifyRecord>> {
        let mut records = Vec::with_capacity(self.members.len());
        let source = &mut self.source;
        let members = &self.members;
        for idx in 0..members.len() {
            if !matches!(members[idx].entry_type, EntryType::File) {
                continue;
            }
            records.push(verify_member_at(source, members, idx)?);
        }
        Ok(records)
    }

    /// Verifies a single member's content against its TOC SHA-256.
    pub fn verify_member(&mut self, target_path: &str) -> Result<Vec<VerifyRecord>> {
        let idx = self
            .members
            .iter()
            .position(|m| m.path == target_path)
            .ok_or_else(|| anyhow::anyhow!("path not found in archive: {target_path}"))?;
        if !matches!(self.members[idx].entry_type, EntryType::File) {
            bail!("{target_path} is not a regular file");
        }
        let record = verify_member_at(&mut self.source, &self.members, idx)?;
        Ok(vec![record])
    }
}

/// Decompresses member `idx`'s chunks and writes its file contents to `out`.
/// Returns an error if the member isn't a regular file.
fn extract_by_index(
    source: &mut Box<dyn ReadSeek>,
    members: &[TocMember],
    idx: usize,
    out: &mut dyn Write,
) -> Result<()> {
    let member = &members[idx];
    if !matches!(member.entry_type, EntryType::File) {
        bail!("{} is not a regular file", member.path);
    }
    if member.chunks.is_empty() {
        bail!("member has no chunks: {}", member.path);
    }

    // Chunks are contiguous in the raw tar stream. chunk_tar_start is the sum of
    // uncompressed sizes of all chunks in all preceding members.
    let chunk_tar_start: u64 = members[..idx]
        .iter()
        .flat_map(|m| m.chunks.iter())
        .map(|c| c.uncompressed_size)
        .sum();

    // Offset of the file data within the concatenation of this member's
    // chunks: skip past any extension headers and the 512-byte tar header.
    let data_offset = member.tar_offset - chunk_tar_start + 512;

    let mut skip = data_offset;
    let mut remaining = member.size;
    for chunk in &member.chunks {
        if remaining == 0 {
            break;
        }

        source
            .seek(SeekFrom::Start(chunk.compressed_offset))
            .context("failed to seek to chunk")?;
        let limited = (&mut *source).take(chunk.compressed_size);
        let mut decoder =
            crate::zstd_impl::Decoder::new(limited).context("failed to create zstd decoder")?;

        if skip >= chunk.uncompressed_size {
            std::io::copy(&mut decoder, &mut std::io::sink())
                .context("failed to verify skipped zstd chunk")?;
            skip -= chunk.uncompressed_size;
            continue;
        }

        // `frame_offset` skips past other members sharing this frame; `skip`
        // then skips this member's own extension headers and tar header.
        crate::io::skip_exact(&mut decoder, chunk.frame_offset + skip)
            .context("failed to skip to file data in chunk")?;
        let available = chunk.uncompressed_size - skip;
        let take = available.min(remaining);
        crate::io::copy_exact(&mut decoder, out, take).context("failed to copy file data")?;
        std::io::copy(&mut decoder, &mut std::io::sink()).context("failed to finish zstd frame")?;
        skip = 0;
        remaining -= take;
    }

    if remaining != 0 {
        bail!(
            "archive truncated: {} is missing {remaining} bytes of data",
            member.path
        );
    }

    Ok(())
}

/// `Write` impl that only hashes (discards bytes), used to compute the SHA-256
/// of a member's content while extracting without keeping it in memory.
struct Sha256Sink {
    hasher: Sha256,
}

impl Sha256Sink {
    fn new() -> Self {
        Self {
            hasher: Sha256::new(),
        }
    }
    fn finalize_hex(self) -> String {
        self.hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
}

impl Write for Sha256Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.hasher.update(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn verify_member_at(
    source: &mut Box<dyn ReadSeek>,
    members: &[TocMember],
    idx: usize,
) -> Result<VerifyRecord> {
    let path = members[idx].path.clone();
    let expected = match &members[idx].content_sha256 {
        None => {
            return Ok(VerifyRecord {
                path,
                status: VerifyStatus::NoChecksum,
            });
        }
        Some(h) => h.clone(),
    };
    let mut sink = Sha256Sink::new();
    extract_by_index(source, members, idx, &mut sink)?;
    let actual = sink.finalize_hex();
    let status = if actual == expected {
        VerifyStatus::Ok
    } else {
        VerifyStatus::Mismatch { expected, actual }
    };
    Ok(VerifyRecord { path, status })
}

/// Verifies every chunk byte range lies inside the data region
/// `[14, toc_offset)` and that distinct frames do not overlap. Multiple
/// chunks at the same `(compressed_offset, compressed_size)` are fine —
/// that's how small members share a frame.
fn validate_chunk_layout(members: &[TocMember], toc_offset: u64) -> Result<()> {
    let mut ranges: Vec<(u64, u64)> = members
        .iter()
        .flat_map(|m| m.chunks.iter())
        .map(|c| (c.compressed_offset, c.compressed_size))
        .collect();
    ranges.sort_unstable();
    ranges.dedup();

    let data_start: u64 = 14; // size of the identity frame

    let mut prev_end: u64 = data_start;
    for (start, size) in &ranges {
        let end = start
            .checked_add(*size)
            .ok_or_else(|| anyhow::anyhow!("chunk at offset {start} size {size} overflows u64"))?;
        if *start < data_start {
            bail!("chunk offset {start} is inside the identity frame ({data_start} bytes)");
        }
        if end > toc_offset {
            bail!("chunk at {start}+{size} extends into the TOC region (starts at {toc_offset})");
        }
        if *start < prev_end {
            bail!(
                "chunk frames overlap: previous frame ends at {prev_end}, next starts at {start}"
            );
        }
        prev_end = end;
    }
    Ok(())
}

/// Reads the trailing footer frame: a fixed-size skippable frame at the very
/// end of the file. Validates magic, length, TRZN marker, and frame type, then
/// returns the decoded payload.
fn read_footer<R: Read + Seek>(file: &mut R, file_size: u64) -> Result<Footer> {
    if file_size < FOOTER_FRAME_SIZE {
        bail!(
            "file too small to contain a tarzan footer: {} bytes (need ≥ {FOOTER_FRAME_SIZE})",
            file_size
        );
    }
    file.seek(SeekFrom::Start(file_size - FOOTER_FRAME_SIZE))
        .context("failed to seek to footer")?;
    let mut buf = vec![0u8; FOOTER_FRAME_SIZE as usize];
    file.read_exact(&mut buf)
        .context("failed to read footer bytes")?;

    let magic = u32::from_le_bytes(buf[0..4].try_into().unwrap());
    if magic != format::SKIPPABLE_FRAME_MAGIC {
        bail!(
            "not a tarzan archive (or footer corrupted): trailing frame magic is {magic:#010x}, \
             expected {:#010x}",
            format::SKIPPABLE_FRAME_MAGIC
        );
    }
    let payload_size = u32::from_le_bytes(buf[4..8].try_into().unwrap()) as u64;
    if payload_size + 8 != FOOTER_FRAME_SIZE {
        bail!(
            "footer length field is {payload_size}, expected {} bytes",
            FOOTER_FRAME_SIZE - 8
        );
    }
    decode_footer_payload(&buf[8..])
}

/// Reads and decodes the TOC frame at the given offset.
fn read_toc<R: Read + Seek>(file: &mut R, offset: u64, frame_size: u64) -> Result<Vec<TocMember>> {
    // On 32-bit targets `usize` tops out at ~4 GiB; zstd skippable frames can
    // legally go right up to that limit. Refuse rather than silently truncate.
    let frame_size_usize: usize = frame_size
        .try_into()
        .map_err(|_| anyhow::anyhow!("TOC frame size {frame_size} exceeds addressable memory"))?;
    file.seek(SeekFrom::Start(offset))
        .context("failed to seek to TOC")?;
    let mut buf = vec![0u8; frame_size_usize];
    file.read_exact(&mut buf)
        .context("failed to read TOC bytes")?;

    let magic = u32::from_le_bytes(buf[0..4].try_into().unwrap());
    if magic != format::SKIPPABLE_FRAME_MAGIC {
        bail!("TOC frame magic mismatch: got {magic:#010x}");
    }
    let payload_size = u32::from_le_bytes(buf[4..8].try_into().unwrap()) as u64;
    if payload_size + 8 != frame_size {
        bail!(
            "TOC frame length mismatch: header says {payload_size} bytes, frame is {} bytes",
            frame_size - 8
        );
    }
    let payload = &buf[8..];
    if payload.len() < 6 || &payload[0..4] != b"TRZN" {
        bail!("TOC frame is not a TRZN payload");
    }
    if payload[4] != format::FRAME_TYPE_TOC {
        bail!(
            "frame at TOC offset is not a TOC frame (type {:#04x})",
            payload[4]
        );
    }
    let toc = decode_toc_payload(payload).context("failed to decode TOC payload")?;
    Ok(toc.members)
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
    use crate::format::footer::encode_footer_frame;
    use crate::format::identity::identity_frame;
    use crate::format::toc::{ChunkInfo, EntryType, TocFrame, TocMember, encode_toc_frame};
    use std::io::Cursor;

    fn small_toc_bytes() -> Vec<u8> {
        // A TOC with one minimal File member whose single chunk sits right
        // after the identity frame. The chunk validator on open requires
        // every chunk to fit inside [14, toc_offset); we use
        // compressed_offset=14 and a zero-byte size so the chunk sits at
        // the very start of the data region.
        let toc = TocFrame {
            tarzan_version: 2,
            members: vec![TocMember {
                path: "x.txt".into(),
                entry_type: EntryType::File,
                size: 0,
                mode: 0o644,
                uid: 0,
                gid: 0,
                mtime: 0,
                tar_offset: 0,
                link_target: None,
                content_sha256: None,
                content_md5: None,
                chunks: vec![ChunkInfo {
                    compressed_offset: 14,
                    compressed_size: 0,
                    uncompressed_size: 0,
                    frame_offset: 0,
                }],
            }],
        };
        encode_toc_frame(&toc, 3).expect("encode toc")
    }

    /// Builds a minimal in-memory v2 archive: identity, optional opaque
    /// data-frame stand-in, TOC, footer. The hash in the footer matches
    /// everything before it, so verify_archive_hash() succeeds.
    fn synth_v2_archive(data_filler: &[u8], toc_bytes: &[u8]) -> Vec<u8> {
        let identity = identity_frame();

        let mut hasher = XxHash64::with_seed(ARCHIVE_HASH_SEED);
        hasher.write(&identity);
        hasher.write(data_filler);
        hasher.write(toc_bytes);
        let archive_xxhash64 = hasher.finish();

        let toc_offset = (identity.len() + data_filler.len()) as u64;
        let toc_frame_size = toc_bytes.len() as u64;
        let footer = encode_footer_frame(&Footer {
            toc_offset,
            toc_frame_size,
            archive_xxhash64,
        });

        let mut archive =
            Vec::with_capacity(identity.len() + data_filler.len() + toc_bytes.len() + footer.len());
        archive.extend_from_slice(&identity);
        archive.extend_from_slice(data_filler);
        archive.extend_from_slice(toc_bytes);
        archive.extend_from_slice(&footer);
        archive
    }

    #[test]
    fn reader_opens_minimal_v2_archive_via_footer() {
        let toc = small_toc_bytes();
        let archive = synth_v2_archive(&[0xFFu8; 16], &toc);
        let reader = TarzanReader::from_seekable(Cursor::new(archive)).expect("open");
        assert_eq!(reader.members().len(), 1);
        assert_eq!(reader.identity_version(), IDENTITY_VERSION_V2);
    }

    #[test]
    fn reader_opens_when_toc_is_far_from_eof() {
        // No scan needed — the footer points directly to the TOC, so a many-MB
        // filler between TOC and EOF doesn't matter to open performance.
        let toc = small_toc_bytes();
        let filler = vec![0xCCu8; 16 * 1024 * 1024];
        let archive = synth_v2_archive(&filler, &toc);
        let reader = TarzanReader::from_seekable(Cursor::new(archive)).expect("open");
        assert_eq!(reader.members().len(), 1);
    }

    #[test]
    fn verify_archive_hash_succeeds_on_unmodified_archive() {
        let toc = small_toc_bytes();
        let archive = synth_v2_archive(&[0xFFu8; 1024], &toc);
        let mut reader = TarzanReader::from_seekable(Cursor::new(archive)).expect("open");
        reader.verify_archive_hash().expect("hash should match");
    }

    #[test]
    fn verify_archive_hash_detects_corruption_in_prefix() {
        let toc = small_toc_bytes();
        let mut archive = synth_v2_archive(&[0xFFu8; 1024], &toc);
        // Flip a byte well inside the data filler.
        let pos = archive.len() / 2;
        archive[pos] ^= 0xFF;
        let mut reader = TarzanReader::from_seekable(Cursor::new(archive)).expect("open");
        let err = match reader.verify_archive_hash() {
            Ok(()) => panic!("verify_archive_hash should fail on corrupted archive"),
            Err(e) => e,
        };
        assert!(format!("{err:#}").contains("whole-archive hash mismatch"));
    }

    /// Builds a TOC with caller-supplied members, then a full archive whose
    /// hash and footer offsets reflect it. Lets layout-edge tests construct
    /// archives the wrap path would never produce.
    fn archive_from_members(members: Vec<TocMember>, filler_after_identity: &[u8]) -> Vec<u8> {
        let toc = TocFrame {
            tarzan_version: 2,
            members,
        };
        let toc_bytes = encode_toc_frame(&toc, 3).expect("encode toc");
        synth_v2_archive(filler_after_identity, &toc_bytes)
    }

    #[test]
    fn open_rejects_chunk_overlap_between_distinct_frames() {
        // Two members, each claiming its own frame, but their byte ranges
        // overlap. The on-open layout validator must catch this.
        let m = |path: &str, off: u64, size: u64| TocMember {
            path: path.into(),
            entry_type: EntryType::File,
            size: 0,
            mode: 0o644,
            uid: 0,
            gid: 0,
            mtime: 0,
            tar_offset: 0,
            link_target: None,
            content_sha256: None,
            content_md5: None,
            chunks: vec![ChunkInfo {
                compressed_offset: off,
                compressed_size: size,
                uncompressed_size: 0,
                frame_offset: 0,
            }],
        };
        // Frame A: [14, 114). Frame B: [100, 200). They overlap at [100, 114).
        let archive =
            archive_from_members(vec![m("a", 14, 100), m("b", 100, 100)], &[0xFFu8; 1024]);
        let err = match TarzanReader::from_seekable(Cursor::new(archive)) {
            Ok(_) => panic!("open should fail when chunk frames overlap"),
            Err(e) => e,
        };
        let msg = format!("{err:#}");
        assert!(
            msg.contains("overlap") || msg.contains("Overlap"),
            "expected overlap error, got: {msg}"
        );
    }

    #[test]
    fn open_rejects_chunk_pointing_into_toc_region() {
        let m = |off: u64, size: u64| TocMember {
            path: "x".into(),
            entry_type: EntryType::File,
            size: 0,
            mode: 0o644,
            uid: 0,
            gid: 0,
            mtime: 0,
            tar_offset: 0,
            link_target: None,
            content_sha256: None,
            content_md5: None,
            chunks: vec![ChunkInfo {
                compressed_offset: off,
                compressed_size: size,
                uncompressed_size: 0,
                frame_offset: 0,
            }],
        };
        // identity (14) + filler (16) = TOC starts at offset 30; a chunk at
        // [14, 1014) would overrun the data region into the TOC frame.
        let archive = archive_from_members(vec![m(14, 1000)], &[0xFFu8; 16]);
        let err = match TarzanReader::from_seekable(Cursor::new(archive)) {
            Ok(_) => panic!("open should fail when chunk overruns into TOC"),
            Err(e) => e,
        };
        let msg = format!("{err:#}");
        assert!(
            msg.contains("TOC region") || msg.contains("extends"),
            "expected TOC-overrun error, got: {msg}"
        );
    }

    #[test]
    fn open_rejects_chunk_pointing_into_identity_frame() {
        let m = |off: u64| TocMember {
            path: "x".into(),
            entry_type: EntryType::File,
            size: 0,
            mode: 0o644,
            uid: 0,
            gid: 0,
            mtime: 0,
            tar_offset: 0,
            link_target: None,
            content_sha256: None,
            content_md5: None,
            chunks: vec![ChunkInfo {
                compressed_offset: off,
                compressed_size: 0,
                uncompressed_size: 0,
                frame_offset: 0,
            }],
        };
        let archive = archive_from_members(vec![m(8)], &[0xFFu8; 64]);
        let err = match TarzanReader::from_seekable(Cursor::new(archive)) {
            Ok(_) => panic!("open should fail when chunk points inside identity frame"),
            Err(e) => e,
        };
        let msg = format!("{err:#}");
        assert!(
            msg.contains("identity frame"),
            "expected identity-frame error, got: {msg}"
        );
    }

    #[test]
    fn opening_v1_archive_emits_retracted_error() {
        // A v1 identity frame is the only thing the reader needs to see to
        // make the verdict; the rest of the file is irrelevant.
        let mut archive = vec![0u8; 14 + 100];
        archive[0..4].copy_from_slice(&format::SKIPPABLE_FRAME_MAGIC.to_le_bytes());
        archive[4..8].copy_from_slice(&6u32.to_le_bytes());
        archive[8..12].copy_from_slice(b"TRZN");
        archive[12] = format::FRAME_TYPE_IDENTITY;
        archive[13] = IDENTITY_VERSION_V1_LEGACY;
        let err = match TarzanReader::from_seekable(Cursor::new(archive)) {
            Ok(_) => panic!("should fail"),
            Err(e) => e,
        };
        let msg = format!("{err:#}");
        assert!(msg.contains("Legacy v1 format."), "{msg}");
        assert!(msg.contains("zstd -d"), "{msg}");
    }
}
