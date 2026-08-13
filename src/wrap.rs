use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::rc::Rc;

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use sha2::{Digest, Sha256};
use tar_core::parse::{Limits, ParseEvent, ParsedEntry, Parser};
use tracing::debug;

use crate::format::{
    footer::{Footer, encode_footer_frame},
    identity,
    toc::{ChunkInfo, EntryType, TocFrame, TocMember},
};
use crate::io::{CountingWriter, HashingWriter};

/// Options controlling how an archive is written.
///
/// Fields are private; construct with [`WrapOptions::default`] and the builder
/// methods (e.g. [`WrapOptions::chunk_size`]). Keeping the fields private lets
/// new options be added in future minor releases without a breaking change.
#[derive(Debug, Clone)]
pub struct WrapOptions {
    /// Uncompressed tar bytes targeted per independently-decodable data frame.
    chunk_size: usize,
    /// zstd compression level used for data and TOC frames.
    level: i32,
    /// Whether to emit per-file `content_sha256` in the TOC.
    compute_sha256: bool,
    /// Whether to emit per-file `content_md5` in the TOC.
    compute_md5: bool,
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
/// Holds the bytes from absolute offset `base` up to whatever the structural
/// parser has requested so far. Frames are sliced out of this buffer and the
/// consumed prefix is drained, so peak memory stays bounded by the configured
/// chunk size rather than by the size of the whole archive.
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
/// Header parsing and member-data streaming both read through this adapter,
/// which lets `wrap` retain PAX/GNU extension headers and every other input
/// byte for verbatim compression.
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
    let mut reader = CapturingReader {
        inner: input,
        window: Rc::clone(&window),
    };
    let mut parser = Parser::new(Limits::default());
    parser.set_allow_empty_path(true);
    let mut global_pax = PaxRecords::new();

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
    let mut scratch = vec![0u8; 64 * 1024];
    let mut parse_start = 0u64;
    let marker_start = loop {
        match parse_next(&mut parser, &mut reader, &window, parse_start, &global_pax)? {
            ParsedItem::Global { consumed, records } => {
                update_global_pax(&mut global_pax, records);
                parse_start = parse_start
                    .checked_add(consumed as u64)
                    .ok_or_else(|| anyhow::anyhow!("global PAX offset overflows"))?;
            }
            ParsedItem::End => break parse_start,
            ParsedItem::Member(metadata) => {
                let metadata = *metadata;
                let entry_end = metadata
                    .data_start
                    .checked_add(padded_data_size(metadata.on_disk_size)?)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "tar entry {} ending at {}+padded({}) overflows",
                            metadata.member.path,
                            metadata.data_start,
                            metadata.on_disk_size
                        )
                    })?;
                let region_size =
                    entry_end
                        .checked_sub(prev_indexed_entry_end)
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "tar entry {} starts before the previous entry ended",
                                metadata.member.path
                            )
                        })?;
                prev_indexed_entry_end = entry_end;
                parse_start = entry_end;

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

                    let is_file = matches!(members[idx].entry_type, EntryType::File);
                    let mut sha256_ctx = (is_file && compute_sha256).then(Sha256::new);
                    let mut md5_ctx = (is_file && compute_md5).then(md5::Context::new);
                    let mut data_left = metadata.on_disk_size;

                    push_full_chunks(
                        &mut output,
                        &mut pos,
                        &window,
                        level,
                        &mut next_chunk_start,
                        chunk_size,
                        &mut members[idx].chunks,
                    )?;
                    while data_left > 0 {
                        let want = data_left.min(scratch.len() as u64) as usize;
                        let n = reader
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
                        push_full_chunks(
                            &mut output,
                            &mut pos,
                            &window,
                            level,
                            &mut next_chunk_start,
                            chunk_size,
                            &mut members[idx].chunks,
                        )?;
                    }
                    capture_to(&mut reader, &window, entry_end, &mut scratch)
                        .with_context(|| format!("reading padding for {}", members[idx].path))?;
                    push_full_chunks(
                        &mut output,
                        &mut pos,
                        &window,
                        level,
                        &mut next_chunk_start,
                        chunk_size,
                        &mut members[idx].chunks,
                    )?;
                    push_frame(
                        &mut output,
                        &mut pos,
                        &window,
                        next_chunk_start,
                        entry_end,
                        level,
                        &mut members[idx].chunks,
                    )?;
                    window.borrow_mut().drain_to(entry_end);
                    next_chunk_start = entry_end;

                    if let Some(hasher) = sha256_ctx {
                        members[idx].content_sha256 = Some(finalize_sha256_hex(hasher));
                    }
                    if let Some(ctx) = md5_ctx {
                        members[idx].content_md5 = Some(format!("{:x}", ctx.finalize()));
                    }
                    on_member(&members[idx]);
                } else {
                    capture_to(&mut reader, &window, entry_end, &mut scratch)
                        .with_context(|| format!("reading data for {}", members[idx].path))?;
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
            }
        }
    };

    // The parser has captured the end marker. Preserve any additional tar
    // blocking-factor padding or concatenated trailing bytes verbatim.
    std::io::copy(&mut reader, &mut std::io::sink()).context("failed to drain trailing bytes")?;
    let total = window.borrow().end();

    validate_end_of_archive(&window, marker_start, total)?;

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

type PaxRecords = BTreeMap<Vec<u8>, Vec<u8>>;

enum ParsedItem {
    Global {
        consumed: usize,
        records: Vec<(Vec<u8>, Vec<u8>)>,
    },
    Member(Box<MemberMetadata>),
    End,
}

enum ParseAttempt {
    NeedData(u64),
    Item(ParsedItem),
}

fn parse_next<R: Read>(
    parser: &mut Parser,
    reader: &mut CapturingReader<R>,
    window: &Rc<RefCell<Window>>,
    parse_start: u64,
    global_pax: &PaxRecords,
) -> Result<ParsedItem> {
    let mut scratch = [0u8; 64 * 1024];
    loop {
        let attempt = {
            let captured = window.borrow();
            let input = captured.try_slice(parse_start, captured.end())?;
            let event = parser.parse(input).context("failed to parse tar header")?;
            match event {
                ParseEvent::NeedData { min_bytes } => {
                    let target = parse_start
                        .checked_add(min_bytes as u64)
                        .ok_or_else(|| anyhow::anyhow!("tar parser input offset overflows"))?;
                    ParseAttempt::NeedData(target)
                }
                ParseEvent::GlobalExtensions { consumed, pax_data } => {
                    ParseAttempt::Item(ParsedItem::Global {
                        consumed,
                        records: parse_pax_records(pax_data)?,
                    })
                }
                ParseEvent::Entry { consumed, entry } => {
                    ParseAttempt::Item(ParsedItem::Member(Box::new(member_metadata_from_entry(
                        input,
                        parse_start,
                        consumed,
                        entry,
                        global_pax,
                        None,
                    )?)))
                }
                ParseEvent::SparseEntry {
                    consumed,
                    entry,
                    real_size,
                    ..
                } => ParseAttempt::Item(ParsedItem::Member(Box::new(member_metadata_from_entry(
                    input,
                    parse_start,
                    consumed,
                    entry,
                    global_pax,
                    Some(real_size),
                )?))),
                ParseEvent::End { .. } => ParseAttempt::Item(ParsedItem::End),
            }
        };

        match attempt {
            ParseAttempt::NeedData(target) => {
                capture_to(reader, window, target, &mut scratch)
                    .context("unexpected end of input while reading tar header")?;
            }
            ParseAttempt::Item(item) => return Ok(item),
        }
    }
}

fn capture_to<R: Read>(
    reader: &mut CapturingReader<R>,
    window: &Rc<RefCell<Window>>,
    target: u64,
    scratch: &mut [u8],
) -> Result<()> {
    while window.borrow().end() < target {
        let missing = target - window.borrow().end();
        let want = missing.min(scratch.len() as u64) as usize;
        let n = reader.read(&mut scratch[..want])?;
        if n == 0 {
            bail!("tar stream ended before byte {target}");
        }
    }
    Ok(())
}

fn push_full_chunks<W: Write>(
    output: &mut W,
    pos: &mut u64,
    window: &Rc<RefCell<Window>>,
    level: i32,
    next_chunk_start: &mut u64,
    chunk_size: u64,
    chunks: &mut Vec<ChunkInfo>,
) -> Result<()> {
    while window.borrow().end() - *next_chunk_start >= chunk_size {
        let end = *next_chunk_start + chunk_size;
        push_frame(output, pos, window, *next_chunk_start, end, level, chunks)?;
        window.borrow_mut().drain_to(end);
        *next_chunk_start = end;
    }
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
    // it. For small members the streaming loop captured the complete entry
    // before it was added to the group; those bytes live in the window until
    // the group is flushed.
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
    path: Option<Vec<u8>>,
    linkpath: Option<Vec<u8>>,
    pax_size: Option<u64>,
    uid: Option<u64>,
    gid: Option<u64>,
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
    data_start: u64,
    on_disk_size: u64,
}

fn member_metadata_from_entry(
    parser_input: &[u8],
    parse_start: u64,
    consumed: usize,
    entry: ParsedEntry<'_>,
    global_pax: &PaxRecords,
    sparse_real_size: Option<u64>,
) -> Result<MemberMetadata> {
    let mut effective_records = global_pax.clone();
    if let Some(local_pax) = entry.pax {
        update_pax_records(&mut effective_records, parse_pax_records(local_pax)?);
    }
    let pax = pax_data_from_records(&effective_records);
    let is_sparse = sparse_real_size.is_some() || pax.has_gnu_sparse_keys;

    let header_address = entry.header.as_bytes().as_ptr() as usize;
    let input_address = parser_input.as_ptr() as usize;
    let relative_header = header_address
        .checked_sub(input_address)
        .ok_or_else(|| anyhow::anyhow!("tar parser returned a header outside its input"))?;
    let tar_offset = parse_start
        .checked_add(relative_header as u64)
        .ok_or_else(|| anyhow::anyhow!("tar header offset overflows"))?;
    let data_start = parse_start
        .checked_add(consumed as u64)
        .ok_or_else(|| anyhow::anyhow!("tar data offset overflows"))?;

    let header_type = entry.entry_type;
    let entry_type = if is_sparse {
        EntryType::Other
    } else {
        to_entry_type(header_type)
    };
    let path_bytes = pax.path.as_deref().unwrap_or(entry.path.as_ref());
    let path = String::from_utf8_lossy(path_bytes).into_owned();
    let path_bytes = std::str::from_utf8(path_bytes)
        .is_err()
        .then(|| path_bytes.to_vec());

    let on_disk_size = if is_sparse {
        entry.size
    } else {
        pax.pax_size.unwrap_or(entry.size)
    };
    if matches!(
        header_type,
        tar_core::EntryType::Regular | tar_core::EntryType::Continuous
    ) && !is_sparse
        && let Some(pax_size) = pax.pax_size
    {
        let header_size = entry
            .header
            .entry_size()
            .context("failed to read in-header entry size")?;
        if header_size != 0 && header_size != pax_size {
            bail!(
                "refusing to wrap {path}: PAX size={pax_size} disagrees with in-header size={header_size}"
            );
        }
    }
    let size = sparse_real_size.unwrap_or(on_disk_size);
    let mode = pax.mode.unwrap_or(entry.mode);
    let uid = pax.uid.unwrap_or(entry.uid);
    let gid = pax.gid.unwrap_or(entry.gid);
    let (mtime, mtime_ns) = match pax.mtime {
        Some((seconds, nanos)) => (seconds, Some(nanos)),
        None => (entry.mtime as i64, None),
    };

    let effective_link = pax.linkpath.as_deref().or(entry.link_target.as_deref());
    let (link_target, link_target_bytes) = match effective_link {
        None => (None, None),
        Some(bytes) => {
            let display = String::from_utf8_lossy(bytes).into_owned();
            let raw = std::str::from_utf8(bytes).is_err().then(|| bytes.to_vec());
            (Some(display), raw)
        }
    };

    let atime = pax.atime;
    let ctime = pax.ctime;
    let uname = pax.uname.or_else(|| {
        entry
            .uname
            .as_deref()
            .map(|value| String::from_utf8_lossy(value).into_owned())
    });
    let gname = pax.gname.or_else(|| {
        entry
            .gname
            .as_deref()
            .map(|value| String::from_utf8_lossy(value).into_owned())
    });
    let xattrs = (!pax.xattrs.is_empty()).then_some(pax.xattrs);
    let raw_type_byte = (entry_type == EntryType::Other).then(|| entry.header.as_bytes()[156]);

    let member = TocMember {
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

    Ok(MemberMetadata {
        member,
        data_start,
        on_disk_size,
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

fn parse_pax_records(raw: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
    tar_core::PaxExtensions::new(raw)
        .map(|extension| {
            let extension = extension.context("malformed PAX extension")?;
            Ok((
                extension.key_bytes().to_vec(),
                extension.value_bytes().to_vec(),
            ))
        })
        .collect()
}

fn update_global_pax(global: &mut PaxRecords, records: Vec<(Vec<u8>, Vec<u8>)>) {
    update_pax_records(global, records);
}

fn update_pax_records(records: &mut PaxRecords, updates: Vec<(Vec<u8>, Vec<u8>)>) {
    for (key, value) in updates {
        if value.is_empty() {
            records.remove(&key);
        } else {
            records.insert(key, value);
        }
    }
}

fn pax_data_from_records(records: &PaxRecords) -> PaxData {
    let mut data = PaxData::default();
    let mut libarchive_xattrs = BTreeMap::new();
    let mut schily_xattrs = BTreeMap::new();
    for (key, value_bytes) in records {
        if key.starts_with(b"GNU.sparse.") {
            data.has_gnu_sparse_keys = true;
        }

        match key.as_slice() {
            b"path" => data.path = Some(value_bytes.clone()),
            b"linkpath" => data.linkpath = Some(value_bytes.clone()),
            b"size" => {
                if let Ok(s) = std::str::from_utf8(value_bytes)
                    && let Ok(n) = s.trim().parse::<u64>()
                {
                    data.pax_size = Some(n);
                }
            }
            b"uid" => {
                if let Ok(s) = std::str::from_utf8(value_bytes)
                    && let Ok(uid) = s.trim().parse::<u64>()
                {
                    data.uid = Some(uid);
                }
            }
            b"gid" => {
                if let Ok(s) = std::str::from_utf8(value_bytes)
                    && let Ok(gid) = s.trim().parse::<u64>()
                {
                    data.gid = Some(gid);
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
                    schily_xattrs.insert(
                        String::from_utf8_lossy(suffix).into_owned(),
                        value_bytes.clone(),
                    );
                } else if let Some(suffix) = key.strip_prefix(b"LIBARCHIVE.xattr.") {
                    let name = String::from_utf8_lossy(suffix).into_owned();
                    if let Ok(value) = base64::engine::general_purpose::STANDARD.decode(value_bytes)
                    {
                        libarchive_xattrs.insert(name, value);
                    } else {
                        debug!(xattr = %name, "ignoring invalid base64 LIBARCHIVE xattr");
                    }
                }
            }
        }
    }
    data.xattrs = libarchive_xattrs;
    data.xattrs.extend(schily_xattrs);
    data
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

fn to_entry_type(t: tar_core::EntryType) -> EntryType {
    match t {
        tar_core::EntryType::Regular | tar_core::EntryType::Continuous => EntryType::File,
        tar_core::EntryType::Directory => EntryType::Dir,
        tar_core::EntryType::Symlink => EntryType::Symlink,
        tar_core::EntryType::Link => EntryType::HardLink,
        tar_core::EntryType::Char => EntryType::CharDevice,
        tar_core::EntryType::Block => EntryType::BlockDevice,
        tar_core::EntryType::Fifo => EntryType::Fifo,
        _ => EntryType::Other,
    }
}
