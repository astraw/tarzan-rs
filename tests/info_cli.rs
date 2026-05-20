use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::tempdir;

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testdata/fixtures/tiny-tree")
        .canonicalize()
        .expect("fixture path should exist")
}

fn tarzan_bin() -> PathBuf {
    PathBuf::from(std::env::var("CARGO_BIN_EXE_tarzan").expect("missing tarzan test binary"))
}

fn create_archive(temp: &tempfile::TempDir) -> PathBuf {
    let tar_path = temp.path().join("input.tar");
    let archive = temp.path().join("archive.tar.zst");

    let status = Command::new("tar")
        .arg("-cf")
        .arg(&tar_path)
        .arg("-C")
        .arg(fixture_root())
        .arg(".")
        .status()
        .expect("failed to run tar");
    assert!(status.success(), "tar failed");

    let status = Command::new(tarzan_bin())
        .arg("wrap")
        .arg(&tar_path)
        .arg("-f")
        .arg(&archive)
        .status()
        .expect("failed to run tarzan wrap");
    assert!(status.success(), "tarzan wrap failed");
    archive
}

#[test]
fn info_prints_expected_headers() {
    let temp = tempdir().expect("tempdir");
    let archive = create_archive(&temp);

    let output = Command::new(tarzan_bin())
        .args(["info", "-f"])
        .arg(&archive)
        .output()
        .expect("failed to run tarzan info");
    assert!(
        output.status.success(),
        "tarzan info failed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("utf-8");
    for expected in [
        "Format:",
        "File:",
        "Size:",
        "Uncompressed:",
        "Members:",
        "Chunks:",
        "Identity frame:",
        "TOC frame:",
    ] {
        assert!(
            stdout.contains(expected),
            "expected `{expected}` in info output:\n{stdout}"
        );
    }
}

#[test]
fn info_member_count_matches_list() {
    let temp = tempdir().expect("tempdir");
    let archive = create_archive(&temp);

    let info_out = Command::new(tarzan_bin())
        .args(["info", "-f"])
        .arg(&archive)
        .output()
        .expect("info");
    assert!(info_out.status.success());
    let info_stdout = String::from_utf8(info_out.stdout).unwrap();

    let members_line = info_stdout
        .lines()
        .find(|l| l.starts_with("Members:"))
        .expect("Members line present");
    let info_count: usize = members_line
        .split_whitespace()
        .last()
        .unwrap()
        .parse()
        .expect("members count parses as integer");

    let list_out = Command::new(tarzan_bin())
        .args(["list", "-f"])
        .arg(&archive)
        .output()
        .expect("list");
    let list_stdout = String::from_utf8(list_out.stdout).unwrap();
    let list_count = list_stdout.lines().count();

    assert_eq!(
        info_count, list_count,
        "info Members count should equal list line count"
    );
}

#[test]
fn info_json_emits_object_with_expected_keys() {
    let temp = tempdir().expect("tempdir");
    let archive = create_archive(&temp);

    let output = Command::new(tarzan_bin())
        .args(["info", "--json", "-f"])
        .arg(&archive)
        .output()
        .expect("info --json");
    assert!(output.status.success(), "info --json failed");

    let stdout = String::from_utf8(output.stdout).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let obj = parsed.as_object().expect("top-level object");

    for key in [
        "format_version",
        "file",
        "size_bytes",
        "uncompressed_bytes",
        "data_frame_bytes",
        "members",
        "chunks",
        "toc_offset",
        "toc_frame_bytes",
    ] {
        assert!(obj.contains_key(key), "JSON missing key `{key}`: {stdout}");
    }

    // member count in JSON should match `tarzan list` line count.
    let json_members = obj.get("members").unwrap().as_u64().unwrap();
    let list_out = Command::new(tarzan_bin())
        .args(["list", "-f"])
        .arg(&archive)
        .output()
        .expect("list");
    let list_count = String::from_utf8(list_out.stdout).unwrap().lines().count() as u64;
    assert_eq!(json_members, list_count, "members count mismatch");
}

#[test]
fn info_rejects_non_tarzan_file() {
    let temp = tempdir().expect("tempdir");
    let junk = temp.path().join("not-an-archive.bin");
    std::fs::write(&junk, b"this is not a tarzan archive").unwrap();

    let output = Command::new(tarzan_bin())
        .args(["info", "-f"])
        .arg(&junk)
        .output()
        .expect("info");
    assert!(
        !output.status.success(),
        "info should fail on a non-tarzan file"
    );
}
