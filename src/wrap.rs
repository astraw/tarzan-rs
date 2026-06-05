use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::rc::Rc;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use tracing::debug;

use crate::format::{
    footer::{Footer, encode_footer_frame},
    identity,
    toc::{ChunkInfo, EntryType, TocFrame, TocMember},
};
use crate::io::{CountingWriter, HashingWriter};

#[derive(Debug, Clone)]
pub struct WrapOptions {
    /// Uncompressed tar bytes targeted per independently-decodable data frame.
    pub chunk_size: usize,
    /// zstd compression level used for data and TOC frames.
    pub level: i32,
    /// Whether to emit per-file `content_sha256` in the TOC.
    pub compute_sha256: bool,
    /// Whether to emit per-file `content_md5` in the TOC.
    pub compute_md5: bool,
}

impl Default for WrapOptions {
    fn default() -> Self {
        Self {
            chunk_size: 4 * 1024 * 1024,
            level: 3,
            compute_sha256: true,
            compute_md5: true,
        }
    }
}

impl WrapOptions {
    pub fn chunk_size(mut self, chunk_size: usize) -> Self {
        self.chunk_size = chunk_size;
        self
    }

    pub fn level(mut self, level: i32) -> Self {
        self.level = level;
        self
    }

    pub fn compute_sha256(mut self, compute_sha256: bool) -> Self {
        self.compute_sha256 = compute_sha256;
        self
    }

    pub fn compute_md5(mut self, compute_md5: bool) -> Self {
        self.compute_md5 = compute_md5;
        self
    }
}

/// A sliding window of raw tar bytes captured from the input stream.
///
/// Holds the bytes from absolute offset `base` up to whatever the tar reader
/// has consumed so far. Frames are sliced out of this buffer and the consumed
/// prefix is drained, so peak memory stays bounded by the configured chunk
/// size rather than by the size of the whole archive.
struct Window {
    buf: Vec<u8>,
    base: u64,
}

impl Window {
    /// Absolute offset one past the last captured byte.
    fn end(&self) -> u64 {
        self.base + self.buf.len() as u64
    }

    /// Borrows the captured bytes for an absolute range derived from
    /// untrusted tar metadata.
    fn try_slice(&self, start: u64, end: u64) -> Result<&[u8]> {
        if start > end {
            bail!("invalid tar byte range: {start}..{end}");
        }
        if start < self.base || end > self.end() {
            bail!(
                "tar stream ended before byte range {start}..{end} was captured \
                 (captured {}..{})",
                self.base,
                self.end()
            );
        }
        let lo = (start - self.base) as usize;
        let hi = (end - self.base) as usize;
        Ok(&self.buf[lo..hi])
    }

    /// Discards captured bytes before absolute offset `offset`.
    fn drain_to(&mut self, offset: u64) {
        let n = (offset - self.base) as usize;
        self.buf.drain(..n);
        self.base = offset;

        let target = self.buf.len().max(8 * 1024 * 1024);
        if self.buf.capacity() > target.saturating_mul(2) {
            self.buf.shrink_to(target);
        }
    }
}

/// A `Read` adapter that copies every byte it serves into a shared [`Window`].
///
/// The tar reader reads its headers and member data through this adapter,
/// which lets `wrap` recover the exact raw tar bytes — including PAX/GNU
/// extension headers, which the `tar` crate consumes without exposing — and
/// compress them verbatim.
struct CapturingReader<R> {
    inner: R,
    window: Rc<RefCell<Window>>,
}

impl<R: Read> Read for CapturingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.window.borrow_mut().buf.extend_from_slice(&buf[..n]);
        Ok(n)
    }
}

/// A member parsed but not yet emitted: its trailing padding is only captured
/// once the *next* entry's header has been read.
enum Pending {
    /// A small member, awaiting placement into the current group.
    Small { idx: usize, region_size: u64 },
    /// A large member whose final (sub-chunk-size) frame is not yet emitted.
    Large { idx: usize, entry_end: u64 },
}

/// Wraps an existing tar stream into a tarzan archive.
///
/// The input is processed as a stream: member data is compressed and written
/// incrementally, so peak memory is bounded by `opts.chunk_size` rather than
/// by the size of the input.
pub fn wrap<R: Read, W: Write>(input: R, output: W, opts: WrapOptions) -> Result<()> {
    wrap_with(input, output, opts, |_| {})
}

/// Like [`wrap`], but invokes `on_member` with each member's TOC entry as soon
/// as that member has been fully compressed. Useful for progress reporting.
///
/// Members smaller than `opts.chunk_size` are grouped together into a shared
/// zstd frame, so a member is reported once the group it belongs to has been
/// flushed.
pub fn wrap_with<R, W, F>(input: R, output: W, opts: WrapOptions, mut on_member: F) -> Result<()>
where
    R: Read,
    W: Write,
    F: FnMut(&TocMember),
{
    if !crate::io::is_nonzero(opts.chunk_size) {
        bail!("chunk size must be greater than zero");
    }
    let chunk_size = opts.chunk_size as u64;
    let level = opts.level;
    let compute_sha256 = opts.compute_sha256;
    let compute_md5 = opts.compute_md5;

    let window = Rc::new(RefCell::new(Window {
        buf: Vec::new(),
        base: 0,
    }));
    let mut archive = tar::Archive::new(CapturingReader {
        inner: input,
        window: Rc::clone(&window),
    });

    // Everything from the identity frame through the TOC frame is hashed; the
    // footer (which carries the hash) is written outside the hashed region.
    let mut output = HashingWriter::new(output);

    let id_frame = identity::identity_frame();
    output
        .write_all(&id_frame)
        .context("failed to write identity frame")?;
    let mut pos = id_frame.len() as u64;

    let mut members: Vec<TocMember> = Vec::new();
    // Members accumulated for the current shared frame, with each member's
    // region size; the group covers `[next_chunk_start, next_chunk_start + group_size)`.
    let mut group: Vec<(usize, u64)> = Vec::new();
    let mut group_size: u64 = 0;
    // Absolute tar offset where the next not-yet-emitted frame begins; kept
    // equal to the window's base.
    let mut next_chunk_start: u64 = 0;
    // Absolute tar offset where the most recently indexed member's region ends.
    // Non-indexed entries (e.g. PAX global headers) are folded into the next
    // indexed member's region so extraction offsets remain consistent.
    let mut prev_indexed_entry_end: u64 = 0;
    // Absolute tar offset where the most recently parsed tar entry ends.
    let mut last_entry_end: u64 = 0;
    let mut pending: Option<Pending> = None;
    let mut scratch = vec![0u8; 64 * 1024];

    {
        let entries = archive.entries().context("failed to read tar entries")?;
        for entry in entries {
            let mut entry = entry.context("failed to read tar entry")?;

            // The previous member's region is now fully captured (the tar
            // reader consumed this entry's extension headers and 512-byte
            // header to reach it).
            match pending.take() {
                Some(Pending::Small { idx, region_size }) => {
                    add_to_group(
                        &mut output,
                        &mut pos,
                        &window,
                        level,
                        &mut members,
                        &mut group,
                        &mut group_size,
                        &mut next_chunk_start,
                        &mut on_member,
                        chunk_size,
                        compute_sha256,
                        compute_md5,
                        idx,
                        region_size,
                    )?;
                }
                Some(Pending::Large { idx, entry_end }) => {
                    let end = entry_end.min(window.borrow().end());
                    push_frame(
                        &mut output,
                        &mut pos,
                        &window,
                        next_chunk_start,
                        end,
                        level,
                        &mut members[idx].chunks,
                    )?;
                    window.borrow_mut().drain_to(end);
                    next_chunk_start = end;
                    on_member(&members[idx]);
                }
                None => {}
            }

            let metadata = read_member_metadata(&mut entry)?;
            // PAX local/global extension headers can override entry size. If
            // both PAX size and in-header size are present (non-zero) on a
            // non-sparse entry, they must agree; disagreement is hostile.
            if matches!(
                entry.header().entry_type(),
                tar::EntryType::Regular | tar::EntryType::Continuous
            ) && !metadata.is_sparse
                && let Some(pax_size) = metadata.pax_size
            {
                let header_size = entry
                    .header()
                    .entry_size()
                    .context("failed to read in-header entry size")?;
                if header_size != 0 && header_size != pax_size {
                    bail!(
                        "refusing to wrap {}: PAX size={} disagrees with in-header size={}",
                        metadata.member.path,
                        pax_size,
                        header_size
                    );
                }
            }
            // For GNU sparse the tar reader consumes the main header *and*
            // any continuation headers before yielding the entry, so the
            // data starts wherever the window currently ends. The on-disk
            // data length is the raw size field (`entry_size`), not the
            // expanded logical size that `entry.size()` reports. For every
            // other entry type the two are identical and the window's end is
            // exactly `header_pos + 512`.
            let on_disk_size = if metadata.is_sparse {
                entry
                    .header()
                    .entry_size()
                    .context("failed to read sparse entry on-disk size")?
            } else {
                metadata.member.size
            };
            let data_start = window.borrow().end();
            let entry_end = data_start
                .checked_add(padded_data_size(on_disk_size)?)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "tar entry {} ending at {data_start}+padded({on_disk_size}) overflows",
                        metadata.member.path
                    )
                })?;
            let region_size = entry_end
                .checked_sub(prev_indexed_entry_end)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "tar entry {} starts before the previous entry ended",
                        metadata.member.path
                    )
                })?;
            last_entry_end = entry_end;

            // Keep global PAX headers in the compressed byte stream but omit
            // them from TOC member indexing.
            if metadata.skip_toc {
                continue;
            }

            prev_indexed_entry_end = entry_end;
            let idx = members.len();
            members.push(metadata.member);

            {
                let w = window.borrow();
                debug!(
                    members_len = members.len(),
                    window_len = w.buf.len(),
                    window_capacity = w.buf.capacity(),
                    region_size,
                    pos,
                    "wrap loop state"
                );
            }

            if region_size >= chunk_size {
                // Large member: it gets its own frames, split at chunk_size.
                // Flush any pending group so the large member starts a frame.
                flush_group(
                    &mut output,
                    &mut pos,
                    &window,
                    level,
                    &mut members,
                    &mut group,
                    &mut group_size,
                    &mut next_chunk_start,
                    &mut on_member,
                )?;

                // Pull the data through the window ourselves, emitting a frame
                // whenever a full chunk_size has accumulated. Letting the tar
                // reader skip the data instead would buffer the whole member.
                // For regular files we also fold each scratch read into
                // streaming checksum contexts of the member's content; chunks
                // get drained from the window before we could go back and hash
                // from there.
                let is_file = matches!(members[idx].entry_type, EntryType::File);
                let mut sha256_ctx = (is_file && compute_sha256).then(Sha256::new);
                let mut md5_ctx = (is_file && compute_md5).then(md5::Context::new);
                let mut data_left = members[idx].size;
                while data_left > 0 {
                    let want = data_left.min(scratch.len() as u64) as usize;
                    let n = entry
                        .read(&mut scratch[..want])
                        .context("failed to read entry data")?;
                    if n == 0 {
                        bail!(
                            "unexpected end of input while reading {}",
                            members[idx].path
                        );
                    }
                    if let Some(ctx) = &mut sha256_ctx {
                        ctx.update(&scratch[..n]);
                    }
                    if let Some(ctx) = &mut md5_ctx {
                        ctx.consume(&scratch[..n]);
                    }
                    data_left -= n as u64;
                    while window.borrow().end() - next_chunk_start >= chunk_size {
                        let end = next_chunk_start + chunk_size;
                        push_frame(
                            &mut output,
                            &mut pos,
                            &window,
                            next_chunk_start,
                            end,
                            level,
                            &mut members[idx].chunks,
                        )?;
                        window.borrow_mut().drain_to(end);
                        next_chunk_start = end;
                    }
                }
                if let Some(h) = sha256_ctx {
                    members[idx].content_sha256 = Some(finalize_sha256_hex(h));
                }
                if let Some(ctx) = md5_ctx {
                    members[idx].content_md5 = Some(format!("{:x}", ctx.finalize()));
                }

                pending = Some(Pending::Large { idx, entry_end });
            } else {
                // Small member: grouped into a shared frame. Its data is left
                // for the tar reader to skip, which still captures it.
                pending = Some(Pending::Small { idx, region_size });
            }
        }
    }

    // Drain whatever the tar reader left unread: the second end-of-archive zero
    // block and any blocking-factor padding.
    let mut reader = archive.into_inner();
    std::io::copy(&mut reader, &mut std::io::sink()).context("failed to drain trailing bytes")?;
    let total = window.borrow().end();

    match pending.take() {
        Some(Pending::Small { idx, region_size }) => {
            add_to_group(
                &mut output,
                &mut pos,
                &window,
                level,
                &mut members,
                &mut group,
                &mut group_size,
                &mut next_chunk_start,
                &mut on_member,
                chunk_size,
                compute_sha256,
                compute_md5,
                idx,
                region_size,
            )?;
        }
        Some(Pending::Large { idx, entry_end }) => {
            let end = entry_end.min(total);
            push_frame(
                &mut output,
                &mut pos,
                &window,
                next_chunk_start,
                end,
                level,
                &mut members[idx].chunks,
            )?;
            window.borrow_mut().drain_to(end);
            next_chunk_start = end;
            on_member(&members[idx]);
        }
        None => {}
    }

    validate_end_of_archive(&window, last_entry_end, total)?;

    flush_group(
        &mut output,
        &mut pos,
        &window,
        level,
        &mut members,
        &mut group,
        &mut group_size,
        &mut next_chunk_start,
        &mut on_member,
    )?;

    // Trailing frame: end-of-archive marker and padding. It has no TOC member,
    // so its ChunkInfo is discarded.
    if total > next_chunk_start {
        let mut discard = Vec::new();
        push_frame(
            &mut output,
            &mut pos,
            &window,
            next_chunk_start,
            total,
            level,
            &mut discard,
        )?;
    }

    let toc = TocFrame {
        tarzan_version: 2,
        members,
    };
    let toc_offset = pos;
    // Stream the TOC straight to `output` rather than buffering the whole
    // compressed frame in memory. For large archives the uncompressed JSON
    // alone can be many GB.
    let toc_frame_size = crate::format::toc::write_toc_frame(&mut output, &toc, opts.level)
        .context("failed to write TOC frame")?;

    // The footer sits outside the hashed region and carries the hash of
    // everything before it (identity + data frames + TOC).
    let (mut inner, archive_xxhash64) = output.finish();
    let footer = encode_footer_frame(&Footer {
        toc_offset,
        toc_frame_size,
        archive_xxhash64,
    });
    inner
        .write_all(&footer)
        .context("failed to write footer frame")?;
    inner.flush().context("failed to flush wrapped archive")?;

    Ok(())
}

/// Adds a small member to the current group, flushing the group as needed to
/// keep it within `chunk_size`.
#[allow(clippy::too_many_arguments)]
fn add_to_group<W, F>(
    output: &mut W,
    pos: &mut u64,
    window: &Rc<RefCell<Window>>,
    level: i32,
    members: &mut [TocMember],
    group: &mut Vec<(usize, u64)>,
    group_size: &mut u64,
    next_chunk_start: &mut u64,
    on_member: &mut F,
    chunk_size: u64,
    compute_sha256: bool,
    compute_md5: bool,
    idx: usize,
    region_size: u64,
) -> Result<()>
where
    W: Write,
    F: FnMut(&TocMember),
{
    // Hash the member's content from the window before any flush could drain
    // it. For small members the tar reader captured the content bytes when it
    // skipped past them to reach the next entry; those bytes live in the
    // window until the group they belong to is flushed.
    if matches!(members[idx].entry_type, EntryType::File) {
        let content_start = members[idx].tar_offset + 512;
        let content_end = content_start
            .checked_add(members[idx].size)
            .ok_or_else(|| {
                anyhow::anyhow!("member {} content range overflows", members[idx].path)
            })?;
        let w = window.borrow();
        let content = w.try_slice(content_start, content_end)?;
        if compute_sha256 {
            members[idx].content_sha256 = Some(sha256_hex(content));
        }
        if compute_md5 {
            members[idx].content_md5 = Some(format!("{:x}", md5::compute(content)));
        }
    }

    let would_exceed_chunk = match group_size.checked_add(region_size) {
        Some(size) => size > chunk_size,
        None => true,
    };
    if !group.is_empty() && would_exceed_chunk {
        flush_group(
            output,
            pos,
            window,
            level,
            members,
            group,
            group_size,
            next_chunk_start,
            on_member,
        )?;
    }
    group.push((idx, region_size));
    *group_size = group_size
        .checked_add(region_size)
        .ok_or_else(|| anyhow::anyhow!("grouped tar byte range overflows"))?;
    if *group_size >= chunk_size {
        flush_group(
            output,
            pos,
            window,
            level,
            members,
            group,
            group_size,
            next_chunk_start,
            on_member,
        )?;
    }
    Ok(())
}

/// Compresses the grouped members as one shared zstd frame and records a
/// `ChunkInfo` for each, then drains the window and reports the members.
#[allow(clippy::too_many_arguments)]
fn flush_group<W, F>(
    output: &mut W,
    pos: &mut u64,
    window: &Rc<RefCell<Window>>,
    level: i32,
    members: &mut [TocMember],
    group: &mut Vec<(usize, u64)>,
    group_size: &mut u64,
    next_chunk_start: &mut u64,
    on_member: &mut F,
) -> Result<()>
where
    W: Write,
    F: FnMut(&TocMember),
{
    if group.is_empty() {
        return Ok(());
    }
    let start = *next_chunk_start;
    let end = start
        .checked_add(*group_size)
        .ok_or_else(|| anyhow::anyhow!("grouped tar byte range overflows"))?;

    if let Some((compressed_offset, compressed_size)) =
        compress_frame(output, pos, window, start, end, level)?
    {
        let mut frame_offset = 0u64;
        for (idx, region_size) in group.iter() {
            members[*idx].chunks.push(ChunkInfo {
                compressed_offset,
                compressed_size,
                uncompressed_size: *region_size,
                frame_offset,
            });
            frame_offset = frame_offset
                .checked_add(*region_size)
                .ok_or_else(|| anyhow::anyhow!("grouped frame offset overflows"))?;
        }
    }

    window.borrow_mut().drain_to(end);
    *next_chunk_start = end;
    for (idx, _) in group.iter() {
        on_member(&members[*idx]);
    }
    group.clear();
    *group_size = 0;
    {
        let w = window.borrow();
        debug!(
            members_len = members.len(),
            window_len = w.buf.len(),
            window_capacity = w.buf.capacity(),
            pos = *pos,
            "flush_group state"
        );
    }
    Ok(())
}

/// Compresses the window's `[start, end)` bytes as a standalone zstd frame and
/// records a single `ChunkInfo` for it (`frame_offset` is zero).
fn push_frame<W: Write>(
    output: &mut W,
    pos: &mut u64,
    window: &Rc<RefCell<Window>>,
    start: u64,
    end: u64,
    level: i32,
    chunks: &mut Vec<ChunkInfo>,
) -> Result<()> {
    if let Some((compressed_offset, compressed_size)) =
        compress_frame(output, pos, window, start, end, level)?
    {
        chunks.push(ChunkInfo {
            compressed_offset,
            compressed_size,
            uncompressed_size: end - start,
            frame_offset: 0,
        });
    }
    Ok(())
}

/// Compresses the window's `[start, end)` bytes as an independent zstd frame,
/// appends it to `output`, and advances `pos`.
///
/// The encoder is configured to embed a 4-byte XXHash64 content checksum in
/// every frame; the standard zstd decoder verifies it automatically on the
/// way out, so a corrupted chunk fails at decompress time without any
/// further work on our side.
///
/// Returns the frame's compressed offset and size — or `None` if the range
/// is empty.
fn compress_frame<W: Write>(
    output: &mut W,
    pos: &mut u64,
    window: &Rc<RefCell<Window>>,
    start: u64,
    end: u64,
    level: i32,
) -> Result<Option<(u64, u64)>> {
    let window = window.borrow();
    let bytes = window.try_slice(start, end)?;
    if bytes.is_empty() {
        return Ok(None);
    }

    let compressed_offset = *pos;
    let compressed_size = {
        let mut counting = CountingWriter::new(&mut *output);
        let mut encoder = crate::zstd_impl::Encoder::new(&mut counting, level)
            .context("failed to create zstd encoder")?;
        encoder
            .include_checksum(true)
            .context("failed to enable zstd content checksum")?;
        encoder
            .write_all(bytes)
            .context("failed to compress chunk")?;
        encoder.finish().context("failed to finish zstd frame")?;
        counting.bytes_written()
    };
    *pos += compressed_size;

    Ok(Some((compressed_offset, compressed_size)))
}

fn validate_end_of_archive(
    window: &Rc<RefCell<Window>>,
    marker_start: u64,
    total: u64,
) -> Result<()> {
    let marker_end = marker_start
        .checked_add(1024)
        .ok_or_else(|| anyhow::anyhow!("tar end-of-archive marker offset overflows"))?;
    if total < marker_end {
        bail!(
            "tar stream ended before the two-block end-of-archive marker \
             (need at least {marker_end} bytes, got {total})"
        );
    }

    let window = window.borrow();
    let marker = window.try_slice(marker_start, marker_end)?;
    if !marker.iter().all(|byte| *byte == 0) {
        bail!("tar stream is missing the two-block end-of-archive marker");
    }

    Ok(())
}

fn padded_data_size(size: u64) -> Result<u64> {
    size.checked_add(511)
        .map(|size| size / 512 * 512)
        .ok_or_else(|| anyhow::anyhow!("tar entry size {size} overflows"))
}

#[derive(Default)]
struct PaxData {
    has_gnu_sparse_keys: bool,
    pax_size: Option<u64>,
    mtime: Option<(i64, u32)>,
    atime: Option<(i64, u32)>,
    ctime: Option<(i64, u32)>,
    mode: Option<u32>,
    uname: Option<String>,
    gname: Option<String>,
    xattrs: BTreeMap<String, Vec<u8>>,
}

struct MemberMetadata {
    member: TocMember,
    is_sparse: bool,
    skip_toc: bool,
    pax_size: Option<u64>,
}

/// Reads an entry's metadata into a partial `TocMember` (with no `chunks`).
///
/// A regular-file entry whose merged PAX header carries any `GNU.sparse.*`
/// key is a PAX-encoded sparse file (GNU tar formats 0.0, 0.1, 1.0). The
/// `tar` crate does not interpret these as sparse, so the data on disk is
/// the sparse-encoded blob, not the logical file content. Recording such
/// an entry as `EntryType::File` would let consumers extract or hash the
/// encoded blob and believe they had the original file. We downgrade it
/// to `EntryType::Other` so the small-/large-member content-hashing path
/// is skipped and tarzan's random-access tools refuse to treat it as a
/// regular file. The raw tar bytes still round-trip verbatim, so
/// `zstd -d | tar x` recovers the original file correctly.
fn read_member_metadata<R: Read>(entry: &mut tar::Entry<'_, R>) -> Result<MemberMetadata> {
    let pax = read_pax_data(entry)?;
    let header_type = entry.header().entry_type();
    let is_pax_sparse = matches!(header_type, tar::EntryType::Regular) && pax.has_gnu_sparse_keys;
    let skip_toc = header_type.is_pax_global_extensions();
    let entry_type = if is_pax_sparse {
        EntryType::Other
    } else {
        to_entry_type(header_type)
    };
    let header = entry.header();
    let path_bytes_full = entry.path_bytes();
    let path_bytes = path_bytes_full.as_ref();
    let path = String::from_utf8_lossy(path_bytes).into_owned();
    let path_bytes = std::str::from_utf8(path_bytes)
        .is_err()
        .then(|| path_bytes.to_vec());
    // `entry.size()` honours PAX `size=` overrides, which the ustar octal
    // size field cannot encode beyond 8 GB; `header.size()` would return the
    // (typically zero) in-header value and misroute the giant member into
    // the small-member group, leaving its data unflushed in the window.
    //
    // For GNU sparse entries `entry.size()` is the *expanded* (logical) size
    // — what the file looks like after the holes are filled in. The raw
    // on-disk data length (sum of the sparse extent lengths) is the
    // `entry_size` field, and is what we use for tar-layout arithmetic in
    // the wrap loop. We still record the logical size here so listings and
    // extraction tooling show what users expect.
    let size = entry.size();
    let mut mode = header.mode().context("failed to read entry mode")?;
    if let Some(pax_mode) = pax.mode {
        mode = pax_mode;
    }
    let uid = header.uid().context("failed to read entry uid")?;
    let gid = header.gid().context("failed to read entry gid")?;
    let mut mtime = header.mtime().context("failed to read entry mtime")? as i64;
    let mut mtime_ns = None;
    if let Some((sec, nsec)) = pax.mtime {
        mtime = sec;
        mtime_ns = Some(nsec);
    }
    let tar_offset = entry.raw_header_position();
    let (link_target, link_target_bytes) = match entry.link_name_bytes().map(|c| c.into_owned()) {
        None => (None, None),
        Some(bytes) => {
            let display = String::from_utf8_lossy(&bytes).into_owned();
            let raw = std::str::from_utf8(&bytes).is_err().then_some(bytes);
            (Some(display), raw)
        }
    };

    let pax_size = pax.pax_size;
    let atime = pax.atime;
    let ctime = pax.ctime;
    let uname = pax.uname.clone();
    let gname = pax.gname.clone();
    let xattrs = (!pax.xattrs.is_empty()).then_some(pax.xattrs.clone());
    let raw_type_byte = (entry_type == EntryType::Other).then(|| header.as_bytes()[156]);

    let mut member = TocMember {
        path,
        path_bytes,
        entry_type,
        raw_type_byte,
        size,
        mode,
        uid,
        gid,
        mtime,
        mtime_ns,
        atime: atime.map(|(sec, _)| sec),
        atime_ns: atime.map(|(_, nsec)| nsec),
        ctime: ctime.map(|(sec, _)| sec),
        ctime_ns: ctime.map(|(_, nsec)| nsec),
        uname,
        gname,
        xattrs,
        tar_offset,
        link_target,
        link_target_bytes,
        // Filled in later: from the window during small-member grouping,
        // from a streaming hasher in the large-member read loop.
        content_sha256: None,
        content_md5: None,
        chunks: Vec::new(),
    };
    if skip_toc {
        // A global PAX header is metadata, not a file-like member.
        member.content_sha256 = None;
        member.content_md5 = None;
    }

    Ok(MemberMetadata {
        member,
        is_sparse: entry.header().entry_type().is_gnu_sparse(),
        skip_toc,
        pax_size,
    })
}

fn sha256_hex(data: &[u8]) -> String {
    let hash = Sha256::digest(data);
    hash.iter().map(|b| format!("{b:02x}")).collect()
}

fn finalize_sha256_hex(hasher: Sha256) -> String {
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Returns true if any of the entry's merged PAX keys begin with `GNU.sparse.`,
/// indicating a PAX-encoded sparse file (formats 0.0, 0.1, 1.0).
fn read_pax_data<R: Read>(entry: &mut tar::Entry<'_, R>) -> Result<PaxData> {
    let mut data = PaxData::default();
    let Some(exts) = entry
        .pax_extensions()
        .context("failed to read entry PAX extensions")?
    else {
        return Ok(data);
    };
    for ext in exts {
        let ext = ext.context("malformed PAX extension")?;
        let key = ext.key_bytes();
        let value_bytes = ext.value_bytes();
        if key.starts_with(b"GNU.sparse.") {
            data.has_gnu_sparse_keys = true;
        }

        match key {
            b"size" => {
                if let Ok(s) = std::str::from_utf8(value_bytes)
                    && let Ok(n) = s.trim().parse::<u64>()
                {
                    data.pax_size = Some(n);
                }
            }
            b"mtime" => {
                if let Ok(s) = std::str::from_utf8(value_bytes)
                    && let Some(ts) = parse_pax_timestamp(s)
                {
                    data.mtime = Some(ts);
                }
            }
            b"atime" => {
                if let Ok(s) = std::str::from_utf8(value_bytes)
                    && let Some(ts) = parse_pax_timestamp(s)
                {
                    data.atime = Some(ts);
                }
            }
            b"ctime" => {
                if let Ok(s) = std::str::from_utf8(value_bytes)
                    && let Some(ts) = parse_pax_timestamp(s)
                {
                    data.ctime = Some(ts);
                }
            }
            b"mode" => {
                if let Ok(s) = std::str::from_utf8(value_bytes)
                    && let Ok(mode) = s.parse::<u32>()
                {
                    data.mode = Some(mode);
                }
            }
            b"uname" => {
                if let Ok(s) = std::str::from_utf8(value_bytes) {
                    data.uname = Some(s.to_owned());
                }
            }
            b"gname" => {
                if let Ok(s) = std::str::from_utf8(value_bytes) {
                    data.gname = Some(s.to_owned());
                }
            }
            _ => {
                if let Some(suffix) = key.strip_prefix(b"SCHILY.xattr.") {
                    data.xattrs.insert(
                        String::from_utf8_lossy(suffix).into_owned(),
                        value_bytes.to_vec(),
                    );
                } else if let Some(suffix) = key.strip_prefix(b"LIBARCHIVE.xattr.") {
                    data.xattrs.insert(
                        String::from_utf8_lossy(suffix).into_owned(),
                        value_bytes.to_vec(),
                    );
                }
            }
        }
    }
    Ok(data)
}

fn parse_pax_timestamp(raw: &str) -> Option<(i64, u32)> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }

    let (negative, body) = if let Some(rest) = s.strip_prefix('-') {
        (true, rest)
    } else if let Some(rest) = s.strip_prefix('+') {
        (false, rest)
    } else {
        (false, s)
    };

    let (whole_str, frac_str) = match body.split_once('.') {
        Some((w, f)) => (w, f),
        None => (body, ""),
    };

    let whole: i64 = if whole_str.is_empty() {
        0
    } else {
        whole_str.parse().ok()?
    };

    if !frac_str.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut ns = 0u32;
    for (i, b) in frac_str.bytes().enumerate() {
        if i >= 9 {
            break;
        }
        ns = ns.checked_mul(10)?.checked_add((b - b'0') as u32)?;
    }
    for _ in frac_str.len().min(9)..9 {
        ns = ns.checked_mul(10)?;
    }

    if !negative {
        return Some((whole, ns));
    }

    if ns == 0 {
        Some((whole.checked_neg()?, 0))
    } else {
        Some((whole.checked_neg()?.checked_sub(1)?, 1_000_000_000 - ns))
    }
}

fn to_entry_type(t: tar::EntryType) -> EntryType {
    match t {
        tar::EntryType::Regular | tar::EntryType::Continuous => EntryType::File,
        tar::EntryType::Directory => EntryType::Dir,
        tar::EntryType::Symlink => EntryType::Symlink,
        tar::EntryType::Link => EntryType::HardLink,
        tar::EntryType::Char => EntryType::CharDevice,
        tar::EntryType::Block => EntryType::BlockDevice,
        tar::EntryType::Fifo => EntryType::Fifo,
        _ => EntryType::Other,
    }
}
