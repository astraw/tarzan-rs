// Tests for TarzanReader::extract_member and verify_all/verify_member.

use std::io::Cursor;

use tarzan::{TarzanReader, VerifyStatus, WrapOptions};
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
