// Tests for unusual but valid tar entry shapes: empty files, executables, binary
// payloads, and deeply nested paths.  All tars are built programmatically so the
// tests are self-contained and deterministic across platforms.

use std::io::Cursor;

use tarzan::format::{self, toc::TocFrame};

// ── helpers ──────────────────────────────────────────────────────────────────

fn make_tar<F: FnOnce(&mut tar::Builder<Vec<u8>>)>(f: F) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    f(&mut builder);
    builder.into_inner().expect("failed to finalise tar builder")
}

fn wrap(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    tarzan::wrap(
        Cursor::new(raw),
        &mut out,
        tarzan::WrapOptions::default(),
    )
    .expect("wrap should succeed");
    out
}

fn decode_toc(wrapped: &[u8]) -> TocFrame {
    let magic = format::SKIPPABLE_FRAME_MAGIC.to_le_bytes();
    let end = wrapped.len();
    for p in (0..=end.saturating_sub(8)).rev() {
        if wrapped[p..p + 4] != magic {
            continue;
        }
        let payload_size =
            u32::from_le_bytes(wrapped[p + 4..p + 8].try_into().unwrap()) as usize;
        if p + 8 + payload_size != end {
            continue;
        }
        let payload = &wrapped[p + 8..];
        if payload.len() >= 5
            && &payload[0..4] == b"TRZN"
            && payload[4] == format::FRAME_TYPE_TOC
        {
            return tarzan::format::toc::decode_toc_payload(payload)
                .expect("TOC decode should succeed");
        }
    }
    panic!("no TOC frame found in archive");
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
    assert_eq!(&wrapped[0..4], &magic, "archive must open with skippable magic");
    assert_eq!(&wrapped[8..12], b"TRZN");
    assert_eq!(wrapped[12], format::FRAME_TYPE_IDENTITY);
}

#[test]
fn toc_frame_is_last() {
    let raw = single_file_tar("x.txt", 0o644, b"hi");
    let wrapped = wrap(&raw);

    let toc = decode_toc(&wrapped); // panics if not found at end
    assert_eq!(toc.tarzan_version, 1);
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
    assert_eq!(m.mode, 0o755, "executable mode must survive the TOC round-trip");
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
        toc.members
            .iter()
            .any(|m| m.path.contains("deep.txt")),
        "deeply nested path must appear in TOC; got: {:?}",
        toc.members.iter().map(|m| &m.path).collect::<Vec<_>>()
    );
}

// ── multiple entries ──────────────────────────────────────────────────────────

#[test]
fn multiple_entries_all_appear_in_toc() {
    let raw = make_tar(|b| {
        for (name, content) in [("a.txt", b"aaa".as_slice()), ("b.txt", b"bb"), ("c.txt", b"c")] {
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

#[test]
fn multiple_entries_roundtrip_tar_bytes() {
    let raw = make_tar(|b| {
        for (name, content) in [("a.txt", b"aaa".as_slice()), ("b.txt", b"bb"), ("c.txt", b"c")] {
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
