// Tests for unusual but valid tar entry shapes: empty files, executables, binary
// payloads, and deeply nested paths.  All tars are built programmatically so the
// tests are self-contained and deterministic across platforms.

use std::io::Cursor;

use tarzan::{
    TarzanReader,
    format::{self, footer::FOOTER_FRAME_SIZE, toc::TocFrame},
};

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
// in-header `size` field to zero. The structural parser honours the override;
// using only the raw header size would return zero and misroute the
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

fn write_header_block_raw_path(
    out: &mut Vec<u8>,
    path_bytes: &[u8],
    size: u64,
    entry_type: tar::EntryType,
) {
    let mut h = tar::Header::new_ustar();
    h.set_size(size);
    h.set_mode(0o644);
    h.set_uid(0);
    h.set_gid(0);
    h.set_mtime(0);
    h.set_entry_type(entry_type);
    {
        let block = h.as_mut_bytes();
        let n = path_bytes.len().min(100);
        block[0..n].copy_from_slice(&path_bytes[..n]);
    }
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

/// Builds a tar containing a single regular-file entry preceded by a PAX
/// `x` header whose records are the supplied (key, value) pairs.
fn pax_prefixed_file_tar(path: &str, records: &[(&str, &str)], content: &[u8]) -> Vec<u8> {
    fn encode_record(key: &str, value: &str) -> String {
        let suffix = format!(" {key}={value}\n");
        let mut len_digits = 1;
        loop {
            let total = len_digits + suffix.len();
            let s = format!("{total}{suffix}");
            if s.len() == total {
                return s;
            }
            len_digits += 1;
        }
    }

    let mut pax_data = String::new();
    for (k, v) in records {
        pax_data.push_str(&encode_record(k, v));
    }

    let mut out = Vec::new();
    write_header_block(
        &mut out,
        &format!("PaxHeaders/{path}"),
        pax_data.len() as u64,
        tar::EntryType::XHeader,
    );
    out.extend_from_slice(pax_data.as_bytes());
    pad_to_block(&mut out);
    write_header_block(
        &mut out,
        path,
        content.len() as u64,
        tar::EntryType::Regular,
    );
    out.extend_from_slice(content);
    pad_to_block(&mut out);
    out.extend_from_slice(&[0u8; 1024]);
    out
}

fn encode_binary_pax_record(key: &[u8], value: &[u8]) -> Vec<u8> {
    let payload_len = 1 + key.len() + 1 + value.len() + 1;
    let mut len_digits = 1;
    loop {
        let total = len_digits + payload_len;
        let prefix = total.to_string();
        if prefix.len() == len_digits {
            let mut record = Vec::with_capacity(total);
            record.extend_from_slice(prefix.as_bytes());
            record.push(b' ');
            record.extend_from_slice(key);
            record.push(b'=');
            record.extend_from_slice(value);
            record.push(b'\n');
            return record;
        }
        len_digits += 1;
    }
}

fn binary_pax_prefixed_file_tar(path: &str, records: &[(&[u8], &[u8])], content: &[u8]) -> Vec<u8> {
    let mut pax_data = Vec::new();
    for (key, value) in records {
        pax_data.extend_from_slice(&encode_binary_pax_record(key, value));
    }

    let mut out = Vec::new();
    write_header_block(
        &mut out,
        &format!("PaxHeaders/{path}"),
        pax_data.len() as u64,
        tar::EntryType::XHeader,
    );
    out.extend_from_slice(&pax_data);
    pad_to_block(&mut out);
    write_header_block(
        &mut out,
        path,
        content.len() as u64,
        tar::EntryType::Regular,
    );
    out.extend_from_slice(content);
    pad_to_block(&mut out);
    out.extend_from_slice(&[0u8; 1024]);
    out
}

fn pax_global_header_tar(path: &str, content: &[u8]) -> Vec<u8> {
    fn encode_record(key: &str, value: &str) -> String {
        let suffix = format!(" {key}={value}\n");
        let mut len_digits = 1;
        loop {
            let total = len_digits + suffix.len();
            let s = format!("{total}{suffix}");
            if s.len() == total {
                return s;
            }
            len_digits += 1;
        }
    }

    let mut out = Vec::new();
    let global = encode_record("comment", "created-by-test");
    write_header_block(
        &mut out,
        "pax_global_header",
        global.len() as u64,
        tar::EntryType::XGlobalHeader,
    );
    out.extend_from_slice(global.as_bytes());
    pad_to_block(&mut out);
    write_header_block(
        &mut out,
        path,
        content.len() as u64,
        tar::EntryType::Regular,
    );
    out.extend_from_slice(content);
    pad_to_block(&mut out);
    out.extend_from_slice(&[0u8; 1024]);
    out
}

fn pax_global_records_tar(records: &[(&[u8], &[u8])], path: &str, content: &[u8]) -> Vec<u8> {
    let mut pax_data = Vec::new();
    for (key, value) in records {
        pax_data.extend_from_slice(&encode_binary_pax_record(key, value));
    }

    let mut out = Vec::new();
    write_header_block(
        &mut out,
        "pax_global_header",
        pax_data.len() as u64,
        tar::EntryType::XGlobalHeader,
    );
    out.extend_from_slice(&pax_data);
    pad_to_block(&mut out);
    write_header_block(
        &mut out,
        path,
        content.len() as u64,
        tar::EntryType::Regular,
    );
    out.extend_from_slice(content);
    pad_to_block(&mut out);
    out.extend_from_slice(&[0u8; 1024]);
    out
}

fn pax_size_mismatch_tar(path: &str) -> Vec<u8> {
    let content = b"ABCDEFGHIJ"; // 10 bytes
    let mut out = Vec::new();

    let value = "10";
    let suffix = format!(" size={value}\n");
    let mut len_digits = 1;
    let record = loop {
        let total = len_digits + suffix.len();
        let s = format!("{total}{suffix}");
        if s.len() == total {
            break s;
        }
        len_digits += 1;
    };

    write_header_block(
        &mut out,
        &format!("PaxHeaders/{path}"),
        record.len() as u64,
        tar::EntryType::XHeader,
    );
    out.extend_from_slice(record.as_bytes());
    pad_to_block(&mut out);

    // Deliberately disagree with PAX size=10.
    write_header_block(&mut out, path, 5, tar::EntryType::Regular);
    out.extend_from_slice(content);
    pad_to_block(&mut out);
    out.extend_from_slice(&[0u8; 1024]);
    out
}

#[test]
fn pax_encoded_sparse_entry_is_downgraded_to_other() {
    // PAX-encoded GNU sparse uses a regular-file entry type whose
    // on-disk payload is the sparse-encoded blob — not the logical file. The
    // TOC cannot currently represent sparse extents, so we downgrade the entry
    // to `Other` rather than letting consumers mistake these bytes for the
    // expanded logical file.
    let blob = b"real-data";
    let raw = pax_prefixed_file_tar(
        "sparse.bin",
        &[
            ("GNU.sparse.major", "0"),
            ("GNU.sparse.minor", "1"),
            ("GNU.sparse.realsize", "4105"),
            ("GNU.sparse.name", "sparse.bin"),
            ("GNU.sparse.map", "4096,9"),
        ],
        blob,
    );

    let mut wrapped = Vec::new();
    tarzan::wrap(
        Cursor::new(&raw),
        &mut wrapped,
        tarzan::WrapOptions::default(),
    )
    .expect("wrap must accept PAX-encoded sparse entries");
    let decoded = zstd::stream::decode_all(Cursor::new(&wrapped)).unwrap();
    assert_eq!(decoded, raw, "tar bytes must round-trip verbatim");

    let toc = decode_toc(&wrapped);
    let member = toc
        .members
        .iter()
        .find(|m| m.path == "sparse.bin")
        .expect("TOC must contain the sparse entry");
    assert_eq!(
        member.entry_type,
        tarzan::format::toc::EntryType::Other,
        "PAX-encoded sparse must be marked as Other so tarzan cat refuses it"
    );
    assert!(
        member.content_sha256.is_none(),
        "Other entries must not carry a content_sha256 — the on-disk blob is not the file"
    );
    assert!(
        member.content_md5.is_none(),
        "Other entries must not carry a content_md5 — the on-disk blob is not the file"
    );
}

#[test]
fn pax_mtime_mode_and_xattrs_are_captured_in_toc() {
    let raw = pax_prefixed_file_tar(
        "meta.txt",
        &[
            ("mtime", "1715000000.123456789"),
            ("atime", "1715000001.5"),
            ("ctime", "1715000002.25"),
            ("mode", "33261"), // 0100755
            ("uname", "builder"),
            ("gname", "wheel"),
            ("SCHILY.xattr.user.foo", "bar"),
        ],
        b"payload",
    );

    let toc = decode_toc(&wrap(&raw));
    let member = toc
        .members
        .iter()
        .find(|m| m.path == "meta.txt")
        .expect("meta.txt must be present");
    assert_eq!(member.mtime, 1_715_000_000);
    assert_eq!(member.mtime_ns, Some(123_456_789));
    assert_eq!(member.atime, Some(1_715_000_001));
    assert_eq!(member.atime_ns, Some(500_000_000));
    assert_eq!(member.ctime, Some(1_715_000_002));
    assert_eq!(member.ctime_ns, Some(250_000_000));
    assert_eq!(member.mode, 33_261);
    assert_eq!(member.uname.as_deref(), Some("builder"));
    assert_eq!(member.gname.as_deref(), Some("wheel"));
    let xattrs = member.xattrs.as_ref().expect("xattrs must be present");
    assert_eq!(xattrs.get("user.foo").cloned(), Some(b"bar".to_vec()));
}

#[test]
fn binary_pax_xattr_with_newlines_wraps_and_remains_seekable() {
    // macOS bsdtar writes raw SCHILY xattrs alongside its base64-encoded
    // LIBARCHIVE records. PAX records are length-delimited, so newlines and
    // NUL bytes inside a value are data, not record separators.
    let xattr = b"rsrc\n\0\x01\x02\xff";
    let content = b"ordinary file content";
    let raw = binary_pax_prefixed_file_tar(
        "resource-fork.txt",
        &[(b"SCHILY.xattr.com.apple.ResourceFork", xattr)],
        content,
    );

    let wrapped = wrap(&raw);
    let decoded = zstd::stream::decode_all(Cursor::new(&wrapped)).unwrap();
    assert_eq!(
        decoded, raw,
        "binary PAX tar bytes must round-trip verbatim"
    );

    let toc = decode_toc(&wrapped);
    let member = toc
        .members
        .iter()
        .find(|member| member.path == "resource-fork.txt")
        .expect("resource-fork.txt must be indexed");
    assert_eq!(member.tar_offset, 1024, "offset must name the real header");
    assert_eq!(
        member
            .xattrs
            .as_ref()
            .and_then(|attrs| attrs.get("com.apple.ResourceFork"))
            .map(Vec::as_slice),
        Some(xattr.as_slice())
    );

    let mut reader = TarzanReader::from_seekable(Cursor::new(wrapped)).unwrap();
    let mut extracted = Vec::new();
    reader
        .extract_member("resource-fork.txt", &mut extracted)
        .unwrap();
    assert_eq!(extracted, content);
}

#[test]
fn libarchive_xattr_is_decoded_from_base64() {
    let content = b"ordinary file content";
    let raw = binary_pax_prefixed_file_tar(
        "resource-fork.txt",
        &[(b"LIBARCHIVE.xattr.com.apple.ResourceFork", b"cnNyYwoAAQL/")],
        content,
    );
    let toc = decode_toc(&wrap(&raw));
    let member = toc
        .members
        .iter()
        .find(|member| member.path == "resource-fork.txt")
        .expect("resource-fork.txt must be indexed");
    assert_eq!(
        member
            .xattrs
            .as_ref()
            .and_then(|attrs| attrs.get("com.apple.ResourceFork"))
            .map(Vec::as_slice),
        Some(b"rsrc\n\0\x01\x02\xff".as_slice())
    );
}

#[test]
fn apple_double_companion_is_preserved_as_an_ordinary_member() {
    let apple_double = b"\0\x05\x16\x07\0\x02Mac OS X metadata";
    let content = b"ordinary file content";
    let raw = make_tar(|builder| {
        for (path, data) in [
            ("._document.txt", apple_double.as_slice()),
            ("document.txt", content.as_slice()),
        ] {
            let mut header = tar::Header::new_ustar();
            header.set_path(path).unwrap();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_uid(0);
            header.set_gid(0);
            header.set_mtime(0);
            header.set_cksum();
            builder.append(&header, Cursor::new(data)).unwrap();
        }
    });

    let wrapped = wrap(&raw);
    let decoded = zstd::stream::decode_all(Cursor::new(&wrapped)).unwrap();
    assert_eq!(decoded, raw, "AppleDouble bytes must round-trip verbatim");
    let toc = decode_toc(&wrapped);
    assert!(
        toc.members
            .iter()
            .any(|member| member.path == "._document.txt")
    );
    assert!(
        toc.members
            .iter()
            .any(|member| member.path == "document.txt")
    );

    let mut reader = TarzanReader::from_seekable(Cursor::new(wrapped)).unwrap();
    let mut extracted = Vec::new();
    reader
        .extract_member("document.txt", &mut extracted)
        .unwrap();
    assert_eq!(extracted, content);
}

#[test]
fn pax_global_header_is_not_indexed_as_member() {
    let raw = pax_global_header_tar("real.txt", b"hello");
    let wrapped = wrap(&raw);
    let decoded = zstd::stream::decode_all(Cursor::new(&wrapped)).unwrap();
    assert_eq!(decoded, raw, "global PAX bytes must still round-trip");

    let toc = decode_toc(&wrapped);
    assert!(
        !toc.members.iter().any(|m| m.path == "pax_global_header"),
        "global PAX pseudo-member must not be indexed"
    );
    assert!(toc.members.iter().any(|m| m.path == "real.txt"));
}

#[test]
fn pax_global_metadata_applies_to_following_member() {
    let raw = pax_global_records_tar(
        &[
            (b"uname", b"archive-owner"),
            (b"gname", b"archive-group"),
            (b"mtime", b"1715000000.125"),
            (b"SCHILY.xattr.user.global", b"global-value"),
        ],
        "real.txt",
        b"hello",
    );
    let toc = decode_toc(&wrap(&raw));
    let member = toc
        .members
        .iter()
        .find(|member| member.path == "real.txt")
        .expect("real.txt must be indexed");
    assert_eq!(member.uname.as_deref(), Some("archive-owner"));
    assert_eq!(member.gname.as_deref(), Some("archive-group"));
    assert_eq!(member.mtime, 1_715_000_000);
    assert_eq!(member.mtime_ns, Some(125_000_000));
    assert_eq!(
        member
            .xattrs
            .as_ref()
            .and_then(|attrs| attrs.get("user.global"))
            .map(Vec::as_slice),
        Some(b"global-value".as_slice())
    );
}

#[test]
fn trailing_pax_global_header_is_preserved() {
    let mut raw = single_file_tar("real.txt", 0o644, b"hello");
    raw.truncate(raw.len() - 1024);
    let record = encode_binary_pax_record(b"comment", b"no following member");
    write_header_block(
        &mut raw,
        "pax_global_header",
        record.len() as u64,
        tar::EntryType::XGlobalHeader,
    );
    raw.extend_from_slice(&record);
    pad_to_block(&mut raw);
    raw.extend_from_slice(&[0u8; 1024]);

    let wrapped = wrap(&raw);
    let decoded = zstd::stream::decode_all(Cursor::new(&wrapped)).unwrap();
    assert_eq!(decoded, raw);
    let toc = decode_toc(&wrapped);
    assert_eq!(toc.members.len(), 1);
    assert_eq!(toc.members[0].path, "real.txt");
}

#[test]
fn conflicting_pax_size_and_header_size_is_rejected() {
    let raw = pax_size_mismatch_tar("bad.bin");
    let mut wrapped = Vec::new();
    let err = tarzan::wrap(
        Cursor::new(&raw),
        &mut wrapped,
        tarzan::WrapOptions::default(),
    )
    .expect_err("size disagreement must be rejected");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("disagrees") || msg.contains("PAX size"),
        "unexpected error: {msg}"
    );
}

#[test]
fn uncommon_type_records_raw_type_byte() {
    let mut raw = Vec::new();
    write_header_block(&mut raw, "volhdr", 0, tar::EntryType::new(b'V'));
    raw.extend_from_slice(&[0u8; 1024]);

    let toc = decode_toc(&wrap(&raw));
    let member = toc
        .members
        .iter()
        .find(|m| m.path == "volhdr")
        .expect("member must be indexed");
    assert_eq!(member.entry_type, tarzan::format::toc::EntryType::Other);
    assert_eq!(member.raw_type_byte, Some(b'V'));
}

#[cfg(unix)]
#[test]
fn non_utf8_path_is_preserved_in_path_bytes() {
    let path = b"bad-\xff-name";
    let mut raw = Vec::new();
    write_header_block_raw_path(&mut raw, path, 1, tar::EntryType::Regular);
    raw.extend_from_slice(b"x");
    pad_to_block(&mut raw);
    raw.extend_from_slice(&[0u8; 1024]);

    let toc = decode_toc(&wrap(&raw));
    let member = toc.members.first().expect("one member expected");
    assert_eq!(member.path_bytes.as_deref(), Some(&path[..]));
    assert!(member.path.contains('\u{fffd}'));
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
