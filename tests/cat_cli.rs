use std::fs;
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
fn cat_extracts_readme_txt() {
    let temp = tempdir().expect("tempdir");
    let archive = wrap_fixture(&temp);

    let output = Command::new(tarzan_bin())
        .args(["cat", "-f"])
        .arg(&archive)
        .arg("./README.txt")
        .output()
        .expect("failed to run tarzan cat");

    assert!(
        output.status.success(),
        "tarzan cat failed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let expected = fs::read(fixture_root().join("README.txt")).expect("read fixture");
    assert_eq!(
        output.stdout, expected,
        "cat output should match fixture file bytes"
    );
}

#[test]
fn cat_extracts_src_main_rs() {
    let temp = tempdir().expect("tempdir");
    let archive = wrap_fixture(&temp);

    let output = Command::new(tarzan_bin())
        .args(["cat", "-f"])
        .arg(&archive)
        .arg("./src/main.rs")
        .output()
        .expect("failed to run tarzan cat");

    assert!(
        output.status.success(),
        "tarzan cat failed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let expected = fs::read(fixture_root().join("src/main.rs")).expect("read fixture");
    assert_eq!(output.stdout, expected);
}

#[test]
fn cat_missing_path_exits_nonzero() {
    let temp = tempdir().expect("tempdir");
    let archive = wrap_fixture(&temp);

    let status = Command::new(tarzan_bin())
        .args(["cat", "-f"])
        .arg(&archive)
        .arg("no/such/file.txt")
        .status()
        .expect("failed to run tarzan cat");

    assert!(!status.success(), "tarzan cat on missing path should fail");
}

#[test]
fn verify_exits_zero_on_fresh_archive() {
    let temp = tempdir().expect("tempdir");
    let archive = wrap_fixture(&temp);

    let output = Command::new(tarzan_bin())
        .args(["verify", "-f"])
        .arg(&archive)
        .output()
        .expect("failed to run tarzan verify");

    assert!(
        output.status.success(),
        "tarzan verify failed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn verify_single_file_exits_zero() {
    let temp = tempdir().expect("tempdir");
    let archive = wrap_fixture(&temp);

    let output = Command::new(tarzan_bin())
        .args(["verify", "-f"])
        .arg(&archive)
        .arg("./README.txt")
        .output()
        .expect("failed to run tarzan verify");

    assert!(
        output.status.success(),
        "tarzan verify single file failed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
