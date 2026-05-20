use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::tempdir;

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testdata/fixtures/tiny-tree")
        .canonicalize()
        .expect("fixture path should exist")
}

fn create_tar_from_fixture(output_tar: &Path) {
    let fixture = fixture_root();
    let status = Command::new("tar")
        .arg("-cf")
        .arg(output_tar)
        .arg("-C")
        .arg(&fixture)
        .arg(".")
        .status()
        .expect("failed to run tar command");
    assert!(status.success(), "tar command failed");
}

fn wrap_fixture() -> (tempfile::TempDir, Vec<u8>) {
    let temp = tempdir().expect("failed to create tempdir");
    let tar_path = temp.path().join("input.tar");
    create_tar_from_fixture(&tar_path);
    let input = fs::File::open(&tar_path).expect("failed to open tar");
    let mut wrapped = Vec::new();
    tarzan::wrap(input, &mut wrapped, tarzan::WrapOptions::default()).expect("wrap should succeed");
    (temp, wrapped)
}

#[test]
fn wrapped_archive_ends_with_toc_skippable_frame() {
    let (_temp, wrapped) = wrap_fixture();

    let magic = tarzan::format::SKIPPABLE_FRAME_MAGIC.to_le_bytes();
    assert!(
        wrapped.len() >= 8,
        "archive too short: {} bytes",
        wrapped.len()
    );
    // The last 4+4+N bytes must be a valid skippable frame.
    // Walk backwards from the end to find it.
    let end = wrapped.len();
    let mut found = false;
    for p in (0..=end.saturating_sub(8)).rev() {
        if wrapped[p..p + 4] != magic {
            continue;
        }
        let payload_size = u32::from_le_bytes(wrapped[p + 4..p + 8].try_into().unwrap()) as usize;
        if p + 8 + payload_size != end {
            continue;
        }
        let payload = &wrapped[p + 8..];
        if payload.len() >= 5 && &payload[0..4] == b"TRZN" {
            assert_eq!(
                payload[4],
                tarzan::format::FRAME_TYPE_TOC,
                "last skippable frame should be a TOC frame"
            );
            found = true;
            break;
        }
    }
    assert!(found, "did not find a tarzan TOC frame at end of archive");
}

#[test]
fn toc_contains_expected_entries() {
    let (_temp, wrapped) = wrap_fixture();

    // Find and decode the TOC frame.
    let magic = tarzan::format::SKIPPABLE_FRAME_MAGIC.to_le_bytes();
    let end = wrapped.len();
    let mut toc_frame: Option<tarzan::format::toc::TocFrame> = None;
    for p in (0..=end.saturating_sub(8)).rev() {
        if wrapped[p..p + 4] != magic {
            continue;
        }
        let payload_size = u32::from_le_bytes(wrapped[p + 4..p + 8].try_into().unwrap()) as usize;
        if p + 8 + payload_size != end {
            continue;
        }
        let payload = &wrapped[p + 8..];
        if payload.len() >= 5
            && &payload[0..4] == b"TRZN"
            && payload[4] == tarzan::format::FRAME_TYPE_TOC
        {
            toc_frame = Some(
                tarzan::format::toc::decode_toc_payload(payload)
                    .expect("TOC decode should succeed"),
            );
            break;
        }
    }
    let toc = toc_frame.expect("TOC frame must be present");
    assert_eq!(toc.tarzan_version, 1);
    assert!(
        !toc.members.is_empty(),
        "TOC should have at least one member"
    );

    // Every member must have exactly one chunk (the single big frame used in MVP).
    for member in &toc.members {
        assert_eq!(member.chunks.len(), 1, "MVP produces one chunk per member");
        let chunk = &member.chunks[0];
        assert!(
            chunk.compressed_size > 0,
            "chunk must have non-zero compressed size"
        );
        assert!(
            chunk.uncompressed_size > 0 || member.entry_type == tarzan::format::toc::EntryType::Dir,
            "non-directory member must have non-zero uncompressed size"
        );
    }
}

#[test]
fn standard_tools_still_decode_archive_with_toc() {
    let (_temp, wrapped) = wrap_fixture();
    let decoded = zstd::stream::decode_all(Cursor::new(&wrapped))
        .expect("zstd should decode archive with TOC");
    // Result should be a valid tar (starts with a 512-byte block).
    assert!(decoded.len() >= 512, "decoded archive too short");
}

#[test]
fn reader_open_returns_expected_members() {
    let temp = tempdir().expect("failed to create tempdir");
    let tar_path = temp.path().join("input.tar");
    let archive_path = temp.path().join("archive.tar.zst");
    create_tar_from_fixture(&tar_path);
    let input = fs::File::open(&tar_path).expect("failed to open tar");
    let output = fs::File::create(&archive_path).expect("failed to create archive");
    tarzan::wrap(input, output, tarzan::WrapOptions::default()).expect("wrap should succeed");

    let reader = tarzan::TarzanReader::open(&archive_path).expect("reader should open");
    assert!(!reader.members().is_empty(), "reader should find members");
    let paths: Vec<&str> = reader.members().iter().map(|m| m.path.as_str()).collect();
    // tiny-tree fixture has README.txt at the top level
    assert!(
        paths.iter().any(|p| p.contains("README.txt")),
        "expected README.txt in members; got: {paths:?}"
    );
}
