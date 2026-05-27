// Tests for TarzanReader::extract_member and verify_all/verify_member.

use std::io::Cursor;

use tarzan::{ExtractOptions, TarzanReader, VerifyStatus, WrapOptions};
use tempfile::tempdir;

fn make_tar<F: FnOnce(&mut tar::Builder<Vec<u8>>)>(f: F) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    f(&mut builder);
    builder
        .into_inner()
        .expect("failed to finalise tar builder")
}

fn wrap_to_file(raw: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("archive.tar.zst");
    let mut f = std::fs::File::create(&path).unwrap();
    tarzan::wrap(Cursor::new(raw), &mut f, WrapOptions::default()).expect("wrap");
    (dir, path)
}

fn single_file_tar(path: &str, content: &[u8]) -> Vec<u8> {
    make_tar(|b| {
        let mut h = tar::Header::new_gnu();
        h.set_path(path).unwrap();
        h.set_size(content.len() as u64);
        h.set_mode(0o644);
        h.set_uid(0);
        h.set_gid(0);
        h.set_mtime(0);
        h.set_cksum();
        b.append(&h, Cursor::new(content)).unwrap();
    })
}

#[test]
fn extract_member_returns_correct_bytes() {
    let content = b"hello from tarzan cat!";
    let raw = single_file_tar("hello.txt", content);
    let (_dir, path) = wrap_to_file(&raw);

    let mut reader = TarzanReader::open(&path).expect("open");
    let mut out = Vec::new();
    reader
        .extract_member("hello.txt", &mut out)
        .expect("extract");
    assert_eq!(out, content);
}

#[test]
fn extract_member_empty_file_yields_empty_bytes() {
    let raw = single_file_tar("empty.txt", b"");
    let (_dir, path) = wrap_to_file(&raw);

    let mut reader = TarzanReader::open(&path).expect("open");
    let mut out = Vec::new();
    reader
        .extract_member("empty.txt", &mut out)
        .expect("extract");
    assert!(out.is_empty());
}

#[test]
fn extract_member_binary_content_exact() {
    let content: Vec<u8> = (0u8..=255).collect();
    let raw = single_file_tar("binary.bin", &content);
    let (_dir, path) = wrap_to_file(&raw);

    let mut reader = TarzanReader::open(&path).expect("open");
    let mut out = Vec::new();
    reader
        .extract_member("binary.bin", &mut out)
        .expect("extract");
    assert_eq!(out, content);
}

#[test]
fn extract_member_second_entry_correct() {
    let raw = make_tar(|b| {
        for (name, content) in [("a.txt", b"aaaa".as_slice()), ("b.txt", b"bbbbbb")] {
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
    let (_dir, path) = wrap_to_file(&raw);

    let mut reader = TarzanReader::open(&path).expect("open");

    let mut out_a = Vec::new();
    reader
        .extract_member("a.txt", &mut out_a)
        .expect("extract a");
    assert_eq!(out_a, b"aaaa");

    let mut out_b = Vec::new();
    reader
        .extract_member("b.txt", &mut out_b)
        .expect("extract b");
    assert_eq!(out_b, b"bbbbbb");
}

#[test]
fn extract_member_checks_zstd_frame_trailer() {
    let raw = single_file_tar("hello.txt", b"hello from tarzan cat!");
    let (dir, path) = wrap_to_file(&raw);

    let reader = TarzanReader::open(&path).expect("open");
    let chunk = reader
        .members()
        .iter()
        .find(|m| m.path == "hello.txt")
        .expect("member present")
        .chunks[0]
        .clone();

    let mut archive = std::fs::read(&path).expect("read archive");
    let checksum_byte = (chunk.compressed_offset + chunk.compressed_size - 1) as usize;
    archive[checksum_byte] ^= 0xff;

    let corrupted_path = dir.path().join("corrupted.tar.zst");
    std::fs::write(&corrupted_path, archive).expect("write corrupted archive");

    let mut reader = TarzanReader::open(&corrupted_path).expect("open corrupted archive");
    let mut out = Vec::new();
    let result = reader.extract_member("hello.txt", &mut out);
    assert!(
        result.is_err(),
        "extract_member should reject a corrupted zstd frame trailer"
    );
}

#[test]
fn extract_member_missing_path_errors() {
    let raw = single_file_tar("exists.txt", b"data");
    let (_dir, path) = wrap_to_file(&raw);

    let mut reader = TarzanReader::open(&path).expect("open");
    let mut out = Vec::new();
    let result = reader.extract_member("does_not_exist.txt", &mut out);
    assert!(result.is_err(), "expected error for missing path");
}

#[test]
fn verify_all_passes_for_freshly_wrapped_archive() {
    let raw = make_tar(|b| {
        for (name, content) in [("a.txt", b"aaa".as_slice()), ("b.txt", b"bbb")] {
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
    let (_dir, path) = wrap_to_file(&raw);

    let mut reader = TarzanReader::open(&path).expect("open");
    let results = reader.verify_all().expect("verify");

    assert!(!results.is_empty(), "expected at least one verify record");
    for r in &results {
        assert!(
            matches!(r.status, VerifyStatus::Ok),
            "expected Ok for {}; got mismatch or no-checksum",
            r.path
        );
    }
}

#[test]
fn verify_member_passes_for_specific_file() {
    let raw = single_file_tar("check.txt", b"verify me");
    let (_dir, path) = wrap_to_file(&raw);

    let mut reader = TarzanReader::open(&path).expect("open");
    let results = reader.verify_member("check.txt").expect("verify");

    assert!(!results.is_empty());
    assert!(matches!(results[0].status, VerifyStatus::Ok));
}

#[test]
fn content_sha256_matches_sha256sum() {
    use sha2::{Digest, Sha256};
    let content = b"this is the file body";
    let raw = single_file_tar("hello.txt", content);
    let (_dir, path) = wrap_to_file(&raw);

    let reader = TarzanReader::open(&path).expect("open");
    let m = reader
        .members()
        .iter()
        .find(|m| m.path == "hello.txt")
        .expect("member present");

    let recorded = m
        .content_sha256
        .as_ref()
        .expect("regular files must record content_sha256");
    let expected: String = Sha256::digest(content)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(*recorded, expected);
}

#[test]
fn content_sha256_matches_for_large_member_spanning_chunks() {
    // chunk_size=4 KiB and a 16 KiB body forces the wrap into the large path
    // with multiple chunks; the streaming hasher must aggregate them correctly.
    use sha2::{Digest, Sha256};
    let content: Vec<u8> = (0..16 * 1024).map(|i| (i % 251) as u8).collect();
    let raw = single_file_tar("big.bin", &content);

    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("archive.tar.zst");
    let f = std::fs::File::create(&path).unwrap();
    tarzan::wrap(
        Cursor::new(&raw),
        f,
        tarzan::WrapOptions::default().chunk_size(4 * 1024),
    )
    .expect("wrap");

    let reader = TarzanReader::open(&path).expect("open");
    let m = reader
        .members()
        .iter()
        .find(|m| m.path == "big.bin")
        .unwrap();
    assert!(m.chunks.len() > 1, "large member should span >1 chunk");

    let recorded = m.content_sha256.as_ref().expect("hash must be present");
    let expected: String = Sha256::digest(&content)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(*recorded, expected);
}

#[test]
fn content_md5_matches_for_small_member() {
    let content = b"this is the file body";
    let raw = single_file_tar("hello.txt", content);
    let (_dir, path) = wrap_to_file(&raw);

    let reader = TarzanReader::open(&path).expect("open");
    let m = reader
        .members()
        .iter()
        .find(|m| m.path == "hello.txt")
        .expect("member present");

    let recorded = m
        .content_md5
        .as_ref()
        .expect("regular files must record content_md5");
    let expected = format!("{:x}", md5::compute(content));
    assert_eq!(*recorded, expected);
}

#[test]
fn content_md5_matches_for_large_member_spanning_chunks() {
    // chunk_size=4 KiB and a 16 KiB body forces the wrap into the large-member
    // streaming path; the md5::Context must accumulate all chunks correctly.
    let content: Vec<u8> = (0..16 * 1024).map(|i| (i % 251) as u8).collect();
    let raw = single_file_tar("big.bin", &content);

    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("archive.tar.zst");
    let f = std::fs::File::create(&path).unwrap();
    tarzan::wrap(
        Cursor::new(&raw),
        f,
        tarzan::WrapOptions::default().chunk_size(4 * 1024),
    )
    .expect("wrap");

    let reader = TarzanReader::open(&path).expect("open");
    let m = reader
        .members()
        .iter()
        .find(|m| m.path == "big.bin")
        .unwrap();
    assert!(m.chunks.len() > 1, "large member should span >1 chunk");

    let recorded = m.content_md5.as_ref().expect("hash must be present");
    let expected = format!("{:x}", md5::compute(&content));
    assert_eq!(*recorded, expected);
}

// ── --skip-bad-chunks: corruption survival ───────────────────────────────────

/// Wraps three small files with a chunk size that forces each member into its
/// own zstd frame, returns the archive path and the corruption target's chunk
/// info. Each file's region (512-byte tar header + 512-byte padded content)
/// is 1024 bytes; chunk_size = 1500 makes the group flush after every
/// member, so a single zstd frame holds exactly one member.
fn wrap_three_isolated_files(
    dir: &std::path::Path,
) -> (std::path::PathBuf, tarzan::format::toc::ChunkInfo) {
    let raw = make_tar(|b| {
        for (i, name) in ["a.txt", "b.txt", "c.txt"].iter().enumerate() {
            // Distinct content per file so we can't get a false pass by
            // accidentally reading another file's bytes.
            let content: Vec<u8> = (0..100u8).map(|x| x.wrapping_add(i as u8 * 17)).collect();
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
    let archive_path = dir.join("archive.tar.zst");
    let f = std::fs::File::create(&archive_path).unwrap();
    tarzan::wrap(
        Cursor::new(&raw),
        f,
        WrapOptions::default().chunk_size(1500),
    )
    .expect("wrap");

    let reader = TarzanReader::open(&archive_path).expect("open");
    let members: Vec<_> = reader.members().to_vec();
    let target = members.iter().find(|m| m.path == "b.txt").unwrap();
    assert_eq!(
        target.chunks.len(),
        1,
        "b.txt should be in exactly one chunk"
    );
    let a_off = members[0].chunks[0].compressed_offset;
    let b_off = target.chunks[0].compressed_offset;
    let c_off = members[2].chunks[0].compressed_offset;
    assert!(
        a_off != b_off && b_off != c_off,
        "test precondition: each of a/b/c.txt must be in its own frame; \
         got offsets a={a_off} b={b_off} c={c_off}"
    );

    (archive_path, target.chunks[0].clone())
}

fn clobber_frame(archive_path: &std::path::Path, chunk: &tarzan::format::toc::ChunkInfo) {
    let mut bytes = std::fs::read(archive_path).unwrap();
    let start = chunk.compressed_offset as usize;
    let end = start + chunk.compressed_size as usize;
    for b in &mut bytes[start..end] {
        *b = 0;
    }
    std::fs::write(archive_path, &bytes).unwrap();
}

#[test]
fn extract_without_skip_bad_chunks_fails_on_corrupted_frame() {
    let dir = tempdir().expect("tempdir");
    let (archive, b_chunk) = wrap_three_isolated_files(dir.path());
    clobber_frame(&archive, &b_chunk);

    let out = dir.path().join("out");
    let mut reader = TarzanReader::open(&archive).expect("open after corruption");
    let result = reader.extract_to_dir(&out, &ExtractOptions::default(), |_| {});
    assert!(
        result.is_err(),
        "extract should fail when a chunk is unreadable and skip_bad_chunks is off"
    );
}

#[test]
fn extract_with_skip_bad_chunks_recovers_good_files() {
    let dir = tempdir().expect("tempdir");
    let (archive, b_chunk) = wrap_three_isolated_files(dir.path());
    clobber_frame(&archive, &b_chunk);

    let out = dir.path().join("out");
    let mut reader = TarzanReader::open(&archive).expect("open after corruption");
    let opts = ExtractOptions {
        skip_bad_chunks: true,
        ..ExtractOptions::default()
    };
    reader
        .extract_to_dir(&out, &opts, |_| {})
        .expect("extract should succeed with --skip-bad-chunks");

    assert!(out.join("a.txt").exists(), "a.txt should be extracted");
    assert!(
        !out.join("b.txt").exists(),
        "b.txt should be removed after its chunk failed"
    );
    assert!(out.join("c.txt").exists(), "c.txt should be extracted");

    // The good files should contain the bytes we wrote during wrap.
    let a = std::fs::read(out.join("a.txt")).unwrap();
    let c = std::fs::read(out.join("c.txt")).unwrap();
    let expected_a: Vec<u8> = (0..100u8).collect();
    let expected_c: Vec<u8> = (0..100u8).map(|x| x.wrapping_add(34)).collect();
    assert_eq!(a, expected_a);
    assert_eq!(c, expected_c);
}
