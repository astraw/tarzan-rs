use std::cell::RefCell;
use std::io::{Read, Write};
use std::rc::Rc;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::format::{
    identity,
    toc::{ChunkInfo, EntryType, TocFrame, TocMember},
};
use crate::io::CountingWriter;

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
/// has consumed so far. Chunks are sliced out of this buffer and the consumed
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
pub fn wrap_with<R, W, F>(
    input: R,
    mut output: W,
    opts: WrapOptions,
    mut on_member: F,
) -> Result<()>
where
    R: Read,
    W: Write,
    F: FnMut(&TocMember),
{
    if !crate::io::is_nonzero(opts.chunk_size) {
        bail!("chunk size must be greater than zero");
    }
    let chunk_size = opts.chunk_size as u64;

    let window = Rc::new(RefCell::new(Window {
        buf: Vec::new(),
        base: 0,
    }));
    let mut archive = tar::Archive::new(CapturingReader {
        inner: input,
        window: Rc::clone(&window),
    });

    let id_frame = identity::identity_frame_v1();
    output
        .write_all(&id_frame)
        .context("failed to write identity frame")?;
    let mut pos = id_frame.len() as u64;

    let mut members: Vec<TocMember> = Vec::new();
    // Member whose final (sub-chunk-size) chunk is not yet emitted, paired with
    // the absolute tar offset at which that member's data and padding end.
    let mut pending: Option<(usize, u64)> = None;
    // Absolute tar offset where the next not-yet-emitted chunk begins.
    let mut next_chunk_start: u64 = 0;
    let mut scratch = vec![0u8; 64 * 1024];

    {
        let entries = archive.entries().context("failed to read tar entries")?;
        for entry in entries {
            let mut entry = entry.context("failed to read tar entry")?;

            // The tar reader consumed this entry's extension headers and
            // 512-byte header to reach it, so the previous member's bytes are
            // now all in the window: emit its trailing chunk.
            if let Some((prev_idx, prev_end)) = pending.take() {
                let end = prev_end.min(window.borrow().end());
                emit_chunk(
                    &mut output,
                    &mut pos,
                    &window,
                    next_chunk_start,
                    end,
                    opts.level,
                    &mut members[prev_idx].chunks,
                )?;
                next_chunk_start = end;
                window.borrow_mut().drain_to(next_chunk_start);
                on_member(&members[prev_idx]);
            }

            let member = read_member_metadata(&entry)?;
            let header_pos = entry.raw_header_position();
            let entry_end = header_pos + 512 + member.size.div_ceil(512) * 512;
            let member_idx = members.len();
            members.push(member);

            // Pull this member's data through the window ourselves, emitting a
            // chunk whenever a full chunk_size has accumulated. Letting the tar
            // reader skip the data instead would buffer the whole member.
            let mut data_left = members[member_idx].size;
            while data_left > 0 {
                let want = data_left.min(scratch.len() as u64) as usize;
                let n = entry
                    .read(&mut scratch[..want])
                    .context("failed to read entry data")?;
                if n == 0 {
                    bail!(
                        "unexpected end of input while reading {}",
                        members[member_idx].path
                    );
                }
                data_left -= n as u64;
                while window.borrow().end() - next_chunk_start >= chunk_size {
                    let end = next_chunk_start + chunk_size;
                    emit_chunk(
                        &mut output,
                        &mut pos,
                        &window,
                        next_chunk_start,
                        end,
                        opts.level,
                        &mut members[member_idx].chunks,
                    )?;
                    next_chunk_start = end;
                    window.borrow_mut().drain_to(next_chunk_start);
                }
            }

            pending = Some((member_idx, entry_end));
        }
    }

    // Drain whatever the tar reader left unread: the second end-of-archive zero
    // block and any blocking-factor padding.
    let mut reader = archive.into_inner();
    std::io::copy(&mut reader, &mut std::io::sink()).context("failed to drain trailing bytes")?;
    let total = window.borrow().end();

    if let Some((prev_idx, prev_end)) = pending.take() {
        let end = prev_end.min(total);
        emit_chunk(
            &mut output,
            &mut pos,
            &window,
            next_chunk_start,
            end,
            opts.level,
            &mut members[prev_idx].chunks,
        )?;
        next_chunk_start = end;
        window.borrow_mut().drain_to(next_chunk_start);
        on_member(&members[prev_idx]);
    }

    // Trailing chunk: end-of-archive marker and padding. It has no TOC member,
    // so its ChunkInfo is discarded.
    if total > next_chunk_start {
        let mut discard = Vec::new();
        emit_chunk(
            &mut output,
            &mut pos,
            &window,
            next_chunk_start,
            total,
            opts.level,
            &mut discard,
        )?;
    }

    let toc = TocFrame {
        tarzan_version: 1,
        members,
    };
    let toc_frame =
        crate::format::toc::encode_toc_frame(&toc, opts.level).context("failed to encode TOC")?;
    output
        .write_all(&toc_frame)
        .context("failed to write TOC frame")?;

    Ok(())
}

/// Compresses the window's `[start, end)` bytes as an independent zstd frame,
/// appends it to `output`, advances `pos`, and records its `ChunkInfo`.
fn emit_chunk<W: Write>(
    output: &mut W,
    pos: &mut u64,
    window: &Rc<RefCell<Window>>,
    start: u64,
    end: u64,
    level: i32,
    chunks: &mut Vec<ChunkInfo>,
) -> Result<()> {
    let window = window.borrow();
    let bytes = window.slice(start, end);
    if bytes.is_empty() {
        return Ok(());
    }

    let compressed_offset = *pos;
    let compressed_size = {
        let mut counting = CountingWriter::new(&mut *output);
        let mut encoder = zstd::stream::write::Encoder::new(&mut counting, level)
            .context("failed to create zstd encoder")?;
        encoder
            .write_all(bytes)
            .context("failed to compress chunk")?;
        encoder.finish().context("failed to finish zstd frame")?;
        counting.bytes_written()
    };
    *pos += compressed_size;

    chunks.push(ChunkInfo {
        compressed_offset,
        compressed_size,
        uncompressed_size: bytes.len() as u64,
        sha256: Some(sha256_hex(bytes)),
    });
    Ok(())
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
    let size = header.size().context("failed to read entry size")?;
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
        chunks: Vec::new(),
    })
}

fn sha256_hex(data: &[u8]) -> String {
    let hash = Sha256::digest(data);
    hash.iter().map(|b| format!("{b:02x}")).collect()
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
