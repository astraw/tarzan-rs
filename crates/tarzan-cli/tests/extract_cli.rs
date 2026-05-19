use std::fs;
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
    assert!(status.success(), "tar command failed");
}

fn tarzan_bin() -> PathBuf {
    PathBuf::from(std::env::var("CARGO_BIN_EXE_tarzan").expect("missing tarzan test binary"))
}

fn wrap_fixture(temp: &tempfile::TempDir) -> PathBuf {
    let tar_path = temp.path().join("input.tar");
    let archive_path = temp.path().join("archive.tar.zst");
    create_tar_from_fixture(&tar_path);
    let status = Command::new(tarzan_bin())
        .arg("wrap")
        .arg(&tar_path)
        .arg("-f")
        .arg(&archive_path)
        .status()
        .expect("failed to run tarzan wrap");
    assert!(status.success(), "tarzan wrap failed");
    archive_path
}

#[test]
fn extract_recreates_fixture_files() {
    let temp = tempdir().expect("tempdir");
    let archive = wrap_fixture(&temp);
    let dest = temp.path().join("out");

    let status = Command::new(tarzan_bin())
        .args(["extract", "-f"])
        .arg(&archive)
        .arg("-C")
        .arg(&dest)
        .status()
        .expect("failed to run tarzan extract");
    assert!(status.success(), "tarzan extract failed");

    let readme = fs::read(dest.join("README.txt")).expect("README.txt should exist");
    let expected = fs::read(fixture_root().join("README.txt")).expect("read fixture");
    assert_eq!(readme, expected);

    let main_rs = fs::read(dest.join("src/main.rs")).expect("src/main.rs should exist");
    let expected = fs::read(fixture_root().join("src/main.rs")).expect("read fixture");
    assert_eq!(main_rs, expected);
}

#[test]
fn extract_alias_x_works() {
    let temp = tempdir().expect("tempdir");
    let archive = wrap_fixture(&temp);
    let dest = temp.path().join("out");

    let status = Command::new(tarzan_bin())
        .args(["x", "-f"])
        .arg(&archive)
        .arg("-C")
        .arg(&dest)
        .status()
        .expect("failed to run tarzan x");
    assert!(status.success(), "tarzan x failed");
    assert!(dest.join("README.txt").exists());
}

#[test]
fn extract_strip_components_drops_leading_dir() {
    let temp = tempdir().expect("tempdir");
    let archive = wrap_fixture(&temp);
    let dest = temp.path().join("out");

    // Fixture entries look like `./src/main.rs`; `.` normalizes away so
    // strip 1 drops the next real component (`src`, `data`, etc.).
    let status = Command::new(tarzan_bin())
        .args(["extract", "-f"])
        .arg(&archive)
        .arg("-C")
        .arg(&dest)
        .args(["--strip-components", "1"])
        .status()
        .expect("failed to run tarzan extract");
    assert!(status.success());

    assert!(
        dest.join("main.rs").exists(),
        "src/main.rs should land at dest/main.rs after strip"
    );
    assert!(
        dest.join("blob.bin").exists(),
        "data/blob.bin should land at dest/blob.bin after strip"
    );
}

#[test]
fn extract_filter_directory_prefix() {
    let temp = tempdir().expect("tempdir");
    let archive = wrap_fixture(&temp);
    let dest = temp.path().join("out");

    let status = Command::new(tarzan_bin())
        .args(["extract", "-f"])
        .arg(&archive)
        .arg("-C")
        .arg(&dest)
        .arg("src/")
        .status()
        .expect("failed to run tarzan extract");
    assert!(status.success());

    assert!(dest.join("src/main.rs").exists());
    assert!(
        !dest.join("README.txt").exists(),
        "README.txt should be filtered out"
    );
}

#[test]
fn extract_exclude_pattern() {
    let temp = tempdir().expect("tempdir");
    let archive = wrap_fixture(&temp);
    let dest = temp.path().join("out");

    let status = Command::new(tarzan_bin())
        .args(["extract", "-f"])
        .arg(&archive)
        .arg("-C")
        .arg(&dest)
        .args(["--exclude", "*.csv"])
        .status()
        .expect("failed to run tarzan extract");
    assert!(status.success());

    assert!(dest.join("README.txt").exists());
    assert!(
        !dest.join("data/numbers.csv").exists(),
        "*.csv should be excluded"
    );
}

