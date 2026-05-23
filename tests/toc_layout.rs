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
    let mut cmd = Command::new("tar");
    #[cfg(target_os = "macos")]
    cmd.env("COPYFILE_DISABLE", "1");
    let status = cmd
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
fn wrapped_archive_ends_with_footer_then_toc_before() {
    let (_temp, wrapped) = wrap_fixture();

    let footer_size = tarzan::format::footer::FOOTER_FRAME_SIZE as usize;
    assert!(
        wrapped.len() >= footer_size,
        "archive too short for a footer: {} bytes",
        wrapped.len()
    );

    let footer_start = wrapped.len() - footer_size;
    let footer_magic =
        u32::from_le_bytes(wrapped[footer_start..footer_start + 4].try_into().unwrap());
    assert_eq!(footer_magic, tarzan::format::SKIPPABLE_FRAME_MAGIC);
    assert_eq!(&wrapped[footer_start + 8..footer_start + 12], b"TRZN");
    assert_eq!(
        wrapped[footer_start + 12],
        tarzan::format::FRAME_TYPE_FOOTER,
        "last skippable frame should be the footer frame"
    );

    let footer = tarzan::format::footer::decode_footer_payload(&wrapped[footer_start + 8..])
        .expect("footer decode should succeed");
    let toc_start = footer.toc_offset as usize;
    let toc_end = toc_start + footer.toc_frame_size as usize;
    assert_eq!(toc_end, footer_start, "TOC must butt up against the footer");
    assert_eq!(&wrapped[toc_start + 8..toc_start + 12], b"TRZN");
    assert_eq!(
        wrapped[toc_start + 12],
        tarzan::format::FRAME_TYPE_TOC,
        "frame the footer points to must be a TOC frame"
    );
}

#[test]
fn toc_contains_expected_entries() {
    let (_temp, wrapped) = wrap_fixture();

    let footer_size = tarzan::format::footer::FOOTER_FRAME_SIZE as usize;
    let footer_start = wrapped.len() - footer_size;
    let footer = tarzan::format::footer::decode_footer_payload(&wrapped[footer_start + 8..])
        .expect("footer decode should succeed");

    let toc_start = footer.toc_offset as usize;
    let toc_end = toc_start + footer.toc_frame_size as usize;
    let toc = tarzan::format::toc::decode_toc_payload(&wrapped[toc_start + 8..toc_end])
        .expect("TOC decode should succeed");

    assert_eq!(toc.tarzan_version, 2);
    assert!(
        !toc.members.is_empty(),
        "TOC should have at least one member"
    );

    // Every fixture member is far smaller than the default chunk size, so each
    // fits in a single chunk. (Members larger than chunk_size span several.)
    for member in &toc.members {
        assert_eq!(
            member.chunks.len(),
            1,
            "small members should fit in one chunk"
        );
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
