use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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

fn tarzan_bin() -> PathBuf {
    PathBuf::from(std::env::var("CARGO_BIN_EXE_tarzan").expect("missing tarzan test binary"))
}

#[test]
fn wrap_stdin_to_stdout_roundtrips() {
    let temp = tempdir().expect("failed to create tempdir");
    let tar_path = temp.path().join("input.tar");
    create_tar_from_fixture(&tar_path);
    let tar_bytes = fs::read(&tar_path).expect("failed to read tar bytes");

    let mut child = Command::new(tarzan_bin())
        .arg("wrap")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("failed to spawn tarzan wrap");

    let mut stdin = child.stdin.take().expect("child should have stdin");
    stdin
        .write_all(&tar_bytes)
        .expect("failed to write tar bytes to stdin");
    drop(stdin);

    let output = child
        .wait_with_output()
        .expect("failed waiting for tarzan wrap");
    assert!(output.status.success(), "tarzan wrap failed");

    let roundtrip = zstd::stream::decode_all(std::io::Cursor::new(output.stdout))
        .expect("failed to decode zstd output");
    assert_eq!(roundtrip, tar_bytes);
}

#[test]
fn wrap_input_to_output_roundtrips() {
    let temp = tempdir().expect("failed to create tempdir");
    let tar_path = temp.path().join("input.tar");
    let out_path = temp.path().join("output.tar.zst");
    create_tar_from_fixture(&tar_path);

    let status = Command::new(tarzan_bin())
        .arg("wrap")
        .arg(&tar_path)
        .arg("-f")
        .arg(&out_path)
        .status()
        .expect("failed to run tarzan wrap");
    assert!(status.success(), "tarzan wrap exited with non-zero status");

    let source_tar = fs::read(&tar_path).expect("failed to read source tar");
    let compressed = fs::read(&out_path).expect("failed to read compressed output");
    let roundtrip = zstd::stream::decode_all(std::io::Cursor::new(compressed))
        .expect("failed to decode zstd output");
    assert_eq!(roundtrip, source_tar);
}
