use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::tempdir;

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/fixtures/tiny-tree")
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
