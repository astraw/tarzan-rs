use std::collections::BTreeMap;
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

fn ensure_tools() {
    for tool in ["tar", "zstd", "sh"] {
        let status = Command::new("sh")
            .arg("-c")
            .arg(format!("command -v {tool} >/dev/null"))
            .status()
            .expect("failed to check command availability");
        assert!(status.success(), "required tool is not available: {tool}");
    }
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

fn list_files(root: &Path, current: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
    let entries = fs::read_dir(current).expect("failed to read directory");
    for entry in entries {
        let entry = entry.expect("failed to read directory entry");
        let path = entry.path();
        let metadata = entry.metadata().expect("failed to read metadata");
        if metadata.is_dir() {
            list_files(root, &path, out);
            continue;
        }
        if metadata.is_file() {
            let relative = path
                .strip_prefix(root)
                .expect("path should be under root")
                .to_path_buf();
            out.insert(relative, fs::read(path).expect("failed to read file bytes"));
        }
    }
}

fn file_map(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    list_files(root, root, &mut out);
    out
}

#[test]
fn wrapped_archives_extract_with_standard_tools() {
    ensure_tools();

    let temp = tempdir().expect("failed to create tempdir");
    let tar_path = temp.path().join("source.tar");
    let archive_path = temp.path().join("archive.tar.zst");
    let out_pipe = temp.path().join("out-pipe");
    let out_tar = temp.path().join("out-tar-zstd");
    fs::create_dir_all(&out_pipe).expect("failed to create output dir");
    fs::create_dir_all(&out_tar).expect("failed to create output dir");

    create_tar_from_fixture(&tar_path);
    let input = fs::File::open(&tar_path).expect("failed to open source tar");
    let output = fs::File::create(&archive_path).expect("failed to create archive");
    tarzan::wrap(input, output, tarzan::WrapOptions::default()).expect("wrap should succeed");

    let pipe_status = Command::new("sh")
        .arg("-c")
        .arg("zstd -d -q -c \"$1\" | tar -x -C \"$2\"")
        .arg("sh")
        .arg(&archive_path)
        .arg(&out_pipe)
        .status()
        .expect("failed to run zstd|tar pipeline");
    assert!(
        pipe_status.success(),
        "zstd -d | tar x failed with status {pipe_status}"
    );

    let tar_status = Command::new("tar")
        .arg("--zstd")
        .arg("-xf")
        .arg(&archive_path)
        .arg("-C")
        .arg(&out_tar)
        .status()
        .expect("failed to run tar --zstd -xf");
    assert!(
        tar_status.success(),
        "tar --zstd -xf failed with status {tar_status}"
    );

    let expected = file_map(&fixture_root());
    let extracted_pipe = file_map(&out_pipe);
    let extracted_tar = file_map(&out_tar);

    assert_eq!(extracted_pipe, expected);
    assert_eq!(extracted_tar, expected);
}
