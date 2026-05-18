use std::io::{Cursor, Read, Write};

use anyhow::{Context, Result, bail};

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

pub fn wrap<R: Read, W: Write>(mut input: R, mut output: W, opts: WrapOptions) -> Result<()> {
    if !crate::io::is_nonzero(opts.chunk_size) {
        bail!("chunk size must be greater than zero");
    }

    let mut raw_tar = Vec::new();
    input
        .read_to_end(&mut raw_tar)
        .context("failed to read input tar stream")?;

    let toc_members_partial = parse_tar_entries(&raw_tar)?;

    let id_frame = identity::identity_frame_v1();
    output
        .write_all(&id_frame)
        .context("failed to write identity frame")?;
    let mut pos = id_frame.len() as u64;

    let compressed_offset = pos;
    let compressed_size = {
        let mut counting = CountingWriter::new(&mut output);
        let mut encoder = zstd::stream::write::Encoder::new(&mut counting, opts.level)
            .context("failed to create zstd encoder")?;
        encoder
            .write_all(&raw_tar)
            .context("failed to compress tar data")?;
        encoder.finish().context("failed to finish zstd frame")?;
        counting.bytes_written()
    };
    pos += compressed_size;
    let _ = pos;

    let chunk = ChunkInfo {
        compressed_offset,
        compressed_size,
        uncompressed_size: raw_tar.len() as u64,
    };

    let members = toc_members_partial
        .into_iter()
        .map(|m| TocMember {
            chunks: vec![chunk.clone()],
            ..m
        })
        .collect();

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

/// Parses tar entries from `raw_tar` and returns partial `TocMember`s (no `chunks` yet).
fn parse_tar_entries(raw_tar: &[u8]) -> Result<Vec<TocMember>> {
    let mut archive = tar::Archive::new(Cursor::new(raw_tar));
    let mut members = Vec::new();

    for entry in archive.entries().context("failed to read tar entries")? {
        let entry = entry.context("failed to read tar entry")?;
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

        members.push(TocMember {
            path,
            entry_type,
            size,
            mode,
            uid,
            gid,
            mtime,
            tar_offset,
            link_target,
            chunks: Vec::new(), // filled in by caller
        });
    }

    Ok(members)
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
