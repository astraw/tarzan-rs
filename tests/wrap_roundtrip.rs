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
    assert!(status.success(), "tar command failed with status {status}");
}

#[test]
fn wrap_roundtrips_tar_bytes_exactly() {
    let temp = tempdir().expect("failed to create tempdir");
    let tar_path = temp.path().join("input.tar");
    create_tar_from_fixture(&tar_path);

    let source_tar = fs::read(&tar_path).expect("failed to read source tar");
    let mut wrapped = Vec::new();
    tarzan::wrap(
        Cursor::new(&source_tar),
        &mut wrapped,
        tarzan::WrapOptions::default(),
    )
    .expect("wrap should succeed");

    let decompressed =
        zstd::stream::decode_all(Cursor::new(wrapped)).expect("zstd decode should succeed");
    assert_eq!(decompressed, source_tar);
}

#[test]
fn wrap_writes_identity_skippable_frame_prefix() {
    // Two 512-byte zero blocks = a valid empty tar archive.
    let source_tar = vec![0u8; 1024];
    let mut wrapped = Vec::new();
    tarzan::wrap(
        Cursor::new(source_tar.as_slice()),
        &mut wrapped,
        tarzan::WrapOptions::default(),
    )
    .expect("wrap should succeed");

    // Identity frame layout: magic(4) + size(4) + "TRZN"(4) + frame_type(1) + version(1)
    let expected_magic = tarzan::format::identity::SKIPPABLE_FRAME_MAGIC.to_le_bytes();
    assert!(wrapped.len() >= 14);
    assert_eq!(&wrapped[0..4], expected_magic.as_slice());
    assert_eq!(&wrapped[4..8], (6u32).to_le_bytes().as_slice());
    assert_eq!(&wrapped[8..12], b"TRZN");
    assert_eq!(wrapped[12], tarzan::format::FRAME_TYPE_IDENTITY);
    assert_eq!(wrapped[13], tarzan::format::identity::IDENTITY_VERSION_V1);

    let decompressed =
        zstd::stream::decode_all(Cursor::new(wrapped)).expect("zstd decode should succeed");
    assert_eq!(decompressed, source_tar);
}
