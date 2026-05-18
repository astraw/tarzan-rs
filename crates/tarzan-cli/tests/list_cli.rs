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
        .args(["wrap", "--input"])
        .arg(&tar_path)
        .arg("--output")
        .arg(&archive_path)
        .status()
        .expect("failed to run tarzan wrap");
    assert!(status.success(), "tarzan wrap failed");
    archive_path
}

#[test]
fn list_exits_zero_and_prints_paths() {
    let temp = tempdir().expect("failed to create tempdir");
    let archive = wrap_fixture(&temp);

    let output = Command::new(tarzan_bin())
        .arg("list")
        .arg(&archive)
        .output()
        .expect("failed to run tarzan list");

    assert!(
        output.status.success(),
        "tarzan list exited with status {}; stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    assert!(!stdout.is_empty(), "list output should not be empty");
    assert!(
        stdout.lines().any(|l| l.contains("README.txt")),
        "expected README.txt in list output; got:\n{stdout}"
    );
}

#[test]
fn list_long_format_shows_extra_columns() {
    let temp = tempdir().expect("failed to create tempdir");
    let archive = wrap_fixture(&temp);

    let output = Command::new(tarzan_bin())
        .args(["list", "-l"])
        .arg(&archive)
        .output()
        .expect("failed to run tarzan list -l");

    assert!(output.status.success(), "tarzan list -l failed");

    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    // Long format lines contain a year (mtime) and a size field.
    let readme_line = stdout
        .lines()
        .find(|l| l.contains("README.txt"))
        .expect("expected README.txt in list -l output");
    assert!(
        readme_line.contains("19") || readme_line.contains("20"),
        "expected a year in long-format line: {readme_line}"
    );
}

#[test]
fn list_paths_match_tar_tf() {
    let temp = tempdir().expect("failed to create tempdir");
    let archive = wrap_fixture(&temp);
    let tar_path = temp.path().join("input.tar");
    create_tar_from_fixture(&tar_path);

    let tar_output = Command::new("tar")
        .arg("-tf")
        .arg(&tar_path)
        .output()
        .expect("failed to run tar -tf");
    assert!(tar_output.status.success(), "tar -tf failed");
    let tar_paths: std::collections::BTreeSet<String> = String::from_utf8(tar_output.stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();

    let list_output = Command::new(tarzan_bin())
        .arg("list")
        .arg(&archive)
        .output()
        .expect("failed to run tarzan list");
    assert!(list_output.status.success(), "tarzan list failed");
    let list_paths: std::collections::BTreeSet<String> = String::from_utf8(list_output.stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();

    assert_eq!(
        list_paths, tar_paths,
        "tarzan list paths should match tar -tf paths"
    );
}

#[test]
fn list_nonexistent_archive_exits_nonzero() {
    let temp = tempdir().expect("failed to create tempdir");
    let status = Command::new(tarzan_bin())
        .arg("list")
        .arg(temp.path().join("does_not_exist.tar.zst"))
        .status()
        .expect("failed to run tarzan list");
    assert!(
        !status.success(),
        "tarzan list on missing file should fail"
    );
}

// Ensure wrapping still roundtrips correctly after adding TOC.
#[test]
fn wrap_still_roundtrips_after_toc_added() {
    let temp = tempdir().expect("failed to create tempdir");
    let tar_path = temp.path().join("input.tar");
    create_tar_from_fixture(&tar_path);
    let source_tar = fs::read(&tar_path).expect("failed to read tar");
    let archive = wrap_fixture(&temp);
    let compressed = fs::read(&archive).expect("failed to read archive");
    let roundtrip = zstd::stream::decode_all(std::io::Cursor::new(compressed))
        .expect("zstd should decode archive");
    assert_eq!(roundtrip, source_tar);
}
