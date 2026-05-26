use std::cell::RefCell;
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
    pub chunk_size: usize,
    pub level: i32,
}

impl Default for WrapOptions {
    fn default() -> Self {
        Self {
            chunk_size: 4 * 1024 * 1024,
            level: 3,
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

    /// Borrows the captured bytes for the absolute range `[start, end)`.
    fn slice(&self, start: u64, end: u64) -> &[u8] {
        let lo = (start - self.base) as usize;
        let hi = (end - self.base) as usize;
        &self.buf[lo..hi]
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
    // Absolute tar offset where the most recently parsed member's region ends.
    let mut prev_entry_end: u64 = 0;
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

            let member = read_member_metadata(&entry)?;
            let header_pos = entry.raw_header_position();
            let entry_end = header_pos + 512 + member.size.div_ceil(512) * 512;
            let region_size = entry_end - prev_entry_end;
            prev_entry_end = entry_end;
            let idx = members.len();
            members.push(member);

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
                // For regular files we also fold each scratch read into a
                // streaming SHA-256 of the member's content; chunks get drained
                // from the window before we could go back and hash from there.
                let is_file = matches!(members[idx].entry_type, EntryType::File);
                let mut content_hasher = is_file.then(Sha256::new);
                let mut md5_ctx = is_file.then(md5::Context::new);
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
                    if let Some(h) = &mut content_hasher {
                        h.update(&scratch[..n]);
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
                if let Some(h) = content_hasher {
                    members[idx].content_sha256 = Some(finalize_sha256_hex(h));
                }
                if let Some(ctx) = md5_ctx {
                    members[idx].content_md5 = Some(format!("{:x}", ctx.compute()));
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
        let content_end = content_start + members[idx].size;
        let w = window.borrow();
        let content = w.slice(content_start, content_end);
        members[idx].content_sha256 = Some(sha256_hex(content));
        members[idx].content_md5 = Some(format!("{:x}", md5::compute(content)));
    }

    if !group.is_empty() && *group_size + region_size > chunk_size {
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
    *group_size += region_size;
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
    let end = start + *group_size;

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
            frame_offset += region_size;
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
    let bytes = window.slice(start, end);
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

/// Reads an entry's metadata into a partial `TocMember` (with no `chunks`).
fn read_member_metadata<R: Read>(entry: &tar::Entry<'_, R>) -> Result<TocMember> {
    let header = entry.header();
    let entry_type = to_entry_type(header.entry_type());
    let path = entry
        .path()
        .context("failed to read entry path")?
        .to_string_lossy()
        .into_owned();
    // `entry.size()` honours PAX `size=` overrides, which the ustar octal
    // size field cannot encode beyond 8 GB; `header.size()` would return the
    // (typically zero) in-header value and misroute the giant member into
    // the small-member group, leaving its data unflushed in the window.
    let size = entry.size();
    let mode = header.mode().context("failed to read entry mode")?;
    let uid = header.uid().context("failed to read entry uid")?;
    let gid = header.gid().context("failed to read entry gid")?;
    let mtime = header.mtime().context("failed to read entry mtime")? as i64;
    let tar_offset = entry.raw_header_position();
    let link_target = entry
        .link_name()
        .context("failed to read entry link name")?
        .map(|p| p.to_string_lossy().into_owned());

    Ok(TocMember {
        path,
        entry_type,
        size,
        mode,
        uid,
        gid,
        mtime,
        tar_offset,
        link_target,
        // Filled in later: from the window during small-member grouping,
        // from a streaming hasher in the large-member read loop.
        content_sha256: None,
        content_md5: None,
        chunks: Vec::new(),
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
