// Tests for unusual but valid tar entry shapes: empty files, executables, binary
// payloads, and deeply nested paths.  All tars are built programmatically so the
// tests are self-contained and deterministic across platforms.

use std::io::Cursor;

use tarzan::format::{self, footer::FOOTER_FRAME_SIZE, toc::TocFrame};

// ── helpers ──────────────────────────────────────────────────────────────────

fn make_tar<F: FnOnce(&mut tar::Builder<Vec<u8>>)>(f: F) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    f(&mut builder);
    builder
        .into_inner()
        .expect("failed to finalise tar builder")
}

fn wrap(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    tarzan::wrap(Cursor::new(raw), &mut out, tarzan::WrapOptions::default())
        .expect("wrap should succeed");
    out
}

fn decode_toc(wrapped: &[u8]) -> TocFrame {
    // The footer is the last 62 bytes; it points to the TOC frame.
    let total = wrapped.len() as u64;
    assert!(total >= FOOTER_FRAME_SIZE, "archive shorter than a footer");
    let footer_start = (total - FOOTER_FRAME_SIZE) as usize;
    let footer = tarzan::format::footer::decode_footer_payload(&wrapped[footer_start + 8..])
        .expect("footer decode should succeed");
    let toc_start = footer.toc_offset as usize;
    let toc_end = toc_start + footer.toc_frame_size as usize;
    let frame = &wrapped[toc_start..toc_end];
    let payload = &frame[8..];
    assert_eq!(&payload[0..4], b"TRZN", "TOC payload missing TRZN");
    assert_eq!(payload[4], format::FRAME_TYPE_TOC, "TOC payload wrong type");
    tarzan::format::toc::decode_toc_payload(payload).expect("TOC decode should succeed")
}

fn single_file_tar(path: &str, mode: u32, content: &[u8]) -> Vec<u8> {
    make_tar(|b| {
        let mut h = tar::Header::new_gnu();
        h.set_path(path).unwrap();
        h.set_size(content.len() as u64);
        h.set_mode(mode);
        h.set_uid(0);
        h.set_gid(0);
        h.set_mtime(0);
        h.set_cksum();
        b.append(&h, Cursor::new(content)).unwrap();
    })
}

// ── frame ordering ────────────────────────────────────────────────────────────

#[test]
fn identity_frame_is_first() {
    let raw = single_file_tar("x.txt", 0o644, b"hi");
    let wrapped = wrap(&raw);

    let magic = format::identity::SKIPPABLE_FRAME_MAGIC.to_le_bytes();
    assert_eq!(
        &wrapped[0..4],
        &magic,
        "archive must open with skippable magic"
    );
    assert_eq!(&wrapped[8..12], b"TRZN");
    assert_eq!(wrapped[12], format::FRAME_TYPE_IDENTITY);
}

#[test]
fn toc_frame_precedes_footer() {
    let raw = single_file_tar("x.txt", 0o644, b"hi");
    let wrapped = wrap(&raw);

    let toc = decode_toc(&wrapped);
    assert_eq!(toc.tarzan_version, 2);
}

// ── empty file ────────────────────────────────────────────────────────────────

#[test]
fn empty_file_roundtrips_tar_bytes() {
    let raw = single_file_tar("empty.txt", 0o644, b"");
    let wrapped = wrap(&raw);
    let decoded = zstd::stream::decode_all(Cursor::new(&wrapped)).unwrap();
    assert_eq!(decoded, raw);
}

#[test]
fn empty_file_has_zero_size_in_toc() {
    let raw = single_file_tar("empty.txt", 0o644, b"");
    let toc = decode_toc(&wrap(&raw));
    let m = toc
        .members
        .iter()
        .find(|m| m.path.contains("empty.txt"))
        .expect("empty.txt must appear in TOC");
    assert_eq!(m.size, 0);
}

// ── executable mode ───────────────────────────────────────────────────────────

#[test]
fn executable_mode_roundtrips_tar_bytes() {
    let raw = single_file_tar("script.sh", 0o755, b"#!/bin/sh\necho hi\n");
    let wrapped = wrap(&raw);
    let decoded = zstd::stream::decode_all(Cursor::new(&wrapped)).unwrap();
    assert_eq!(decoded, raw);
}

#[test]
fn executable_mode_preserved_in_toc() {
    let raw = single_file_tar("script.sh", 0o755, b"#!/bin/sh\necho hi\n");
    let toc = decode_toc(&wrap(&raw));
    let m = toc
        .members
        .iter()
        .find(|m| m.path.contains("script.sh"))
        .expect("script.sh must appear in TOC");
    assert_eq!(
        m.mode, 0o755,
        "executable mode must survive the TOC round-trip"
    );
}

// ── binary content ────────────────────────────────────────────────────────────

#[test]
fn binary_content_roundtrips_exactly() {
    // All 256 byte values — definitely not valid UTF-8.
    let content: Vec<u8> = (0u8..=255).collect();
    let raw = single_file_tar("binary.bin", 0o644, &content);
    let wrapped = wrap(&raw);
    let decoded = zstd::stream::decode_all(Cursor::new(&wrapped)).unwrap();
    assert_eq!(decoded, raw);
}

#[test]
fn binary_content_size_in_toc() {
    let content: Vec<u8> = (0u8..=255).collect();
    let raw = single_file_tar("binary.bin", 0o644, &content);
    let toc = decode_toc(&wrap(&raw));
    let m = toc
        .members
        .iter()
        .find(|m| m.path.contains("binary.bin"))
        .expect("binary.bin must appear in TOC");
    assert_eq!(m.size, 256);
}

// ── deeply nested path ────────────────────────────────────────────────────────

#[test]
fn deeply_nested_path_in_toc() {
    let raw = single_file_tar("a/b/c/d/e/deep.txt", 0o644, b"deep");
    let toc = decode_toc(&wrap(&raw));
    assert!(
        toc.members.iter().any(|m| m.path.contains("deep.txt")),
        "deeply nested path must appear in TOC; got: {:?}",
        toc.members.iter().map(|m| &m.path).collect::<Vec<_>>()
    );
}

// ── multiple entries ──────────────────────────────────────────────────────────

#[test]
fn multiple_entries_all_appear_in_toc() {
    let raw = make_tar(|b| {
        for (name, content) in [
            ("a.txt", b"aaa".as_slice()),
            ("b.txt", b"bb"),
            ("c.txt", b"c"),
        ] {
            let mut h = tar::Header::new_gnu();
            h.set_path(name).unwrap();
            h.set_size(content.len() as u64);
            h.set_mode(0o644);
            h.set_uid(0);
            h.set_gid(0);
            h.set_mtime(0);
            h.set_cksum();
            b.append(&h, Cursor::new(content)).unwrap();
        }
    });
    let toc = decode_toc(&wrap(&raw));
    let paths: Vec<&str> = toc.members.iter().map(|m| m.path.as_str()).collect();
    for name in ["a.txt", "b.txt", "c.txt"] {
        assert!(
            paths.iter().any(|p| p.contains(name)),
            "{name} missing from TOC; got: {paths:?}"
        );
    }
}

// ── PAX size override ─────────────────────────────────────────────────────────
//
// When a file's size exceeds the 8 GB octal limit of the ustar header, GNU/BSD
// tars commonly emit a PAX `x` extended header with `size=<N>` and set the
// in-header `size` field to zero. The tar crate honours the override via
// `entry.size()`; using `header.size()` would return zero and misroute the
// member into the small-member group, leaving its data unflushed in the
// streaming window. We don't fabricate a multi-GB file here — header size=0
// with PAX size=<actual> is enough to exercise the same code path.

fn write_header_block(out: &mut Vec<u8>, name: &str, size: u64, entry_type: tar::EntryType) {
    let mut h = tar::Header::new_ustar();
    h.set_path(name).unwrap();
    h.set_size(size);
    h.set_mode(0o644);
    h.set_uid(0);
    h.set_gid(0);
    h.set_mtime(0);
    h.set_entry_type(entry_type);
    h.set_cksum();
    out.extend_from_slice(h.as_bytes());
}

fn pad_to_block(out: &mut Vec<u8>) {
    let rem = out.len() % 512;
    if rem != 0 {
        out.resize(out.len() + (512 - rem), 0);
    }
}

/// Builds a tar where the only entry carries `actual_size` bytes of data but
/// its in-header size field reads 0; a preceding PAX `x` header overrides it.
fn pax_size_override_tar(path: &str, content: &[u8]) -> Vec<u8> {
    // PAX record format: "<len> size=<n>\n", where <len> is the total length
    // of the line including the digits, the space, the key=value, and the
    // newline. We pick a length that's self-consistent.
    let value = content.len().to_string();
    let suffix = format!(" size={value}\n");
    // Find the smallest len whose ASCII representation, prepended to suffix,
    // matches the total length of the line.
    let mut len_digits = 1;
    let record = loop {
        let total = len_digits + suffix.len();
        let s = format!("{total}{suffix}");
        if s.len() == total {
            break s;
        }
        len_digits += 1;
    };

    let mut out = Vec::new();

    // PAX 'x' header naming the file the override applies to.
    write_header_block(
        &mut out,
        &format!("PaxHeaders/{path}"),
        record.len() as u64,
        tar::EntryType::XHeader,
    );
    out.extend_from_slice(record.as_bytes());
    pad_to_block(&mut out);

    // Main entry with header size=0; tar honours the PAX `size=` override
    // when streaming the data.
    write_header_block(&mut out, path, 0, tar::EntryType::Regular);
    out.extend_from_slice(content);
    pad_to_block(&mut out);

    // End-of-archive: two zero blocks.
    out.extend_from_slice(&[0u8; 1024]);
    out
}

#[test]
fn pax_size_override_records_actual_size_in_toc() {
    let content = b"AAAA".repeat(64); // 256 bytes; header size=0, PAX size=256.
    let raw = pax_size_override_tar("data.bin", &content);
    let toc = decode_toc(&wrap(&raw));
    let m = toc
        .members
        .iter()
        .find(|m| m.path.contains("data.bin"))
        .expect("data.bin must appear in TOC");
    assert_eq!(
        m.size,
        content.len() as u64,
        "TOC must reflect the PAX-overridden size, not the zero in-header size"
    );
}

#[test]
fn pax_size_override_roundtrips_tar_bytes() {
    let content = b"AAAA".repeat(64);
    let raw = pax_size_override_tar("data.bin", &content);
    let wrapped = wrap(&raw);
    let decoded = zstd::stream::decode_all(Cursor::new(&wrapped)).unwrap();
    assert_eq!(
        decoded, raw,
        "wrap → zstd-decode must reproduce the input tar byte-for-byte"
    );
}

#[test]
fn pax_size_override_routes_through_large_member_path() {
    // With chunk_size smaller than the member's PAX-overridden size, the
    // member must land in the large-member path (a single chunk recorded
    // with frame_offset = 0), not into a shared small-member group.
    let content = vec![0xAB; 4096];
    let raw = pax_size_override_tar("data.bin", &content);

    let mut wrapped = Vec::new();
    tarzan::wrap(
        Cursor::new(&raw),
        &mut wrapped,
        tarzan::WrapOptions::default().chunk_size(1024),
    )
    .expect("wrap should succeed");

    let toc = decode_toc(&wrapped);
    let m = toc
        .members
        .iter()
        .find(|m| m.path.contains("data.bin"))
        .expect("data.bin must appear in TOC");
    assert_eq!(m.size, content.len() as u64);
    assert!(
        m.chunks.iter().all(|c| c.frame_offset == 0),
        "large member's chunks must each start at frame_offset 0; got {:?}",
        m.chunks
    );
}

#[test]
fn malformed_small_member_truncation_returns_error() {
    let mut raw = Vec::new();
    write_header_block(&mut raw, "truncated.bin", 100, tar::EntryType::Regular);

    let mut wrapped = Vec::new();
    let result = tarzan::wrap(
        Cursor::new(&raw),
        &mut wrapped,
        tarzan::WrapOptions::default(),
    );
    assert!(
        result.is_err(),
        "truncated tar input must fail instead of producing an archive"
    );
}

#[test]
fn tar_without_end_of_archive_marker_returns_error() {
    let mut raw = Vec::new();
    write_header_block(&mut raw, "prefix-only.txt", 4, tar::EntryType::Regular);
    raw.extend_from_slice(b"data");
    pad_to_block(&mut raw);

    let mut wrapped = Vec::new();
    let result = tarzan::wrap(
        Cursor::new(&raw),
        &mut wrapped,
        tarzan::WrapOptions::default(),
    );
    assert!(
        result.is_err(),
        "tar input missing the two zero end blocks must not wrap successfully"
    );
}

#[test]
fn pax_size_that_overflows_tar_offsets_returns_error() {
    let huge_size = u64::MAX.to_string();
    let suffix = format!(" size={huge_size}\n");
    let mut len_digits = 1;
    let record = loop {
        let total = len_digits + suffix.len();
        let s = format!("{total}{suffix}");
        if s.len() == total {
            break s;
        }
        len_digits += 1;
    };

    let mut raw = Vec::new();
    write_header_block(
        &mut raw,
        "PaxHeaders/huge.bin",
        record.len() as u64,
        tar::EntryType::XHeader,
    );
    raw.extend_from_slice(record.as_bytes());
    pad_to_block(&mut raw);
    write_header_block(&mut raw, "huge.bin", 0, tar::EntryType::Regular);

    let mut wrapped = Vec::new();
    let result = tarzan::wrap(
        Cursor::new(&raw),
        &mut wrapped,
        tarzan::WrapOptions::default(),
    );
    assert!(
        result.is_err(),
        "overflowing PAX size must fail instead of wrapping offset arithmetic"
    );
}

fn gnu_sparse_tar() -> Vec<u8> {
    let data = b"real-data";
    let mut out = Vec::new();

    let mut h = tar::Header::new_gnu();
    h.set_path("sparse.bin").unwrap();
    h.set_size(data.len() as u64);
    h.set_mode(0o644);
    h.set_uid(0);
    h.set_gid(0);
    h.set_mtime(0);
    h.set_entry_type(tar::EntryType::GNUSparse);
    {
        let gnu = h.as_gnu_mut().expect("new_gnu must produce a GNU header");
        gnu.sparse[0].set_offset(4096);
        gnu.sparse[0].set_length(data.len() as u64);
        gnu.set_real_size(4096 + data.len() as u64);
    }
    h.set_cksum();
    out.extend_from_slice(h.as_bytes());
    out.extend_from_slice(data);
    pad_to_block(&mut out);
    out.extend_from_slice(&[0u8; 1024]);
    out
}

#[test]
fn gnu_sparse_entry_roundtrips_tar_bytes() {
    let raw = gnu_sparse_tar();
    let mut wrapped = Vec::new();
    tarzan::wrap(
        Cursor::new(&raw),
        &mut wrapped,
        tarzan::WrapOptions::default(),
    )
    .expect("wrap must accept GNU sparse entries");
    let decoded = zstd::stream::decode_all(Cursor::new(&wrapped)).unwrap();
    assert_eq!(
        decoded, raw,
        "decompressed tarzan archive must reproduce the original tar bytes"
    );
}

#[test]
fn multiple_entries_roundtrip_tar_bytes() {
    let raw = make_tar(|b| {
        for (name, content) in [
            ("a.txt", b"aaa".as_slice()),
            ("b.txt", b"bb"),
            ("c.txt", b"c"),
        ] {
            let mut h = tar::Header::new_gnu();
            h.set_path(name).unwrap();
            h.set_size(content.len() as u64);
            h.set_mode(0o644);
            h.set_uid(0);
            h.set_gid(0);
            h.set_mtime(0);
            h.set_cksum();
            b.append(&h, Cursor::new(content)).unwrap();
        }
    });
    let wrapped = wrap(&raw);
    let decoded = zstd::stream::decode_all(Cursor::new(&wrapped)).unwrap();
    assert_eq!(decoded, raw);
}
