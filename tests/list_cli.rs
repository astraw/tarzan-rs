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
fn list_exits_zero_and_prints_paths() {
    let temp = tempdir().expect("failed to create tempdir");
    let archive = wrap_fixture(&temp);

    let output = Command::new(tarzan_bin())
        .args(["list", "-f"])
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
        .args(["list", "-v", "-f"])
        .arg(&archive)
        .output()
        .expect("failed to run tarzan list -v");

    assert!(output.status.success(), "tarzan list -v failed");

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
        .args(["list", "-f"])
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
        .args(["list", "-f"])
        .arg(temp.path().join("does_not_exist.tar.zst"))
        .status()
        .expect("failed to run tarzan list");
    assert!(!status.success(), "tarzan list on missing file should fail");
}

#[test]
fn list_verbose_shows_owner_group_column() {
    let temp = tempdir().expect("failed to create tempdir");
    let archive = wrap_fixture(&temp);

    let output = Command::new(tarzan_bin())
        .args(["list", "-v", "-f"])
        .arg(&archive)
        .output()
        .expect("failed to run tarzan list -v");
    assert!(output.status.success());

    let stdout = String::from_utf8(output.stdout).unwrap();
    let readme_line = stdout
        .lines()
        .find(|l| l.contains("README.txt"))
        .expect("README.txt line present");
    // Owner column is `uid/gid` (numeric). Any line should match the
    // pattern `digits/digits` between the mode and size columns.
    assert!(
        readme_line.split_whitespace().any(|f| {
            f.split_once('/')
                .is_some_and(|(a, b)| a.parse::<u64>().is_ok() && b.parse::<u64>().is_ok())
        }),
        "expected uid/gid column in: {readme_line}"
    );
}

#[cfg(unix)]
#[test]
fn list_verbose_shows_symlink_target() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let src = temp.path().join("src");
    fs::create_dir(&src).unwrap();
    fs::write(src.join("target.txt"), b"hi").unwrap();
    symlink("target.txt", src.join("link.txt")).unwrap();

    let tar_path = temp.path().join("input.tar");
    let status = Command::new("tar")
        .arg("-cf")
        .arg(&tar_path)
        .arg("-C")
        .arg(&src)
        .arg(".")
        .status()
        .expect("tar");
    assert!(status.success());

    let archive_path = temp.path().join("a.tar.zst");
    let status = Command::new(tarzan_bin())
        .arg("wrap")
        .arg(&tar_path)
        .arg("-f")
        .arg(&archive_path)
        .status()
        .expect("wrap");
    assert!(status.success());

    let out = Command::new(tarzan_bin())
        .args(["list", "-v", "-f"])
        .arg(&archive_path)
        .output()
        .expect("list -v");
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    let link_line = stdout
        .lines()
        .find(|l| l.contains("link.txt"))
        .expect("link.txt should be listed");
    assert!(
        link_line.contains("-> target.txt"),
        "expected ` -> target.txt` in: {link_line}"
    );
    // Type char should be `l` for the symlink line.
    assert!(
        link_line.trim_start().starts_with('l'),
        "expected symlink line to start with `l`: {link_line}"
    );
}

#[test]
fn list_json_emits_parseable_array() {
    let temp = tempdir().expect("tempdir");
    let archive = wrap_fixture(&temp);

    let output = Command::new(tarzan_bin())
        .args(["list", "--json", "-f"])
        .arg(&archive)
        .output()
        .expect("list --json");
    assert!(output.status.success(), "list --json failed");

    let stdout = String::from_utf8(output.stdout).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let arr = parsed.as_array().expect("top-level array");
    assert!(!arr.is_empty(), "expected non-empty member array");

    let has_readme = arr.iter().any(|m| {
        m.get("path")
            .and_then(|p| p.as_str())
            .is_some_and(|s| s.ends_with("README.txt"))
    });
    assert!(has_readme, "expected a README.txt entry in JSON output");

    let first = &arr[0];
    for key in ["path", "type", "size", "mode", "uid", "gid", "mtime"] {
        assert!(
            first.get(key).is_some(),
            "JSON entry missing key `{key}`: {first}"
        );
    }
}

#[test]
fn list_filter_by_directory_prefix() {
    let temp = tempdir().expect("tempdir");
    let archive = wrap_fixture(&temp);

    let output = Command::new(tarzan_bin())
        .args(["list", "-f"])
        .arg(&archive)
        .arg("src/")
        .output()
        .expect("list with filter");
    assert!(output.status.success());

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.lines().all(|l| l.is_empty() || l.contains("src")),
        "every line should match src/ prefix:\n{stdout}"
    );
    assert!(
        stdout.lines().any(|l| l.contains("main.rs")),
        "expected src/main.rs in filtered listing"
    );
    assert!(
        !stdout.lines().any(|l| l.ends_with("README.txt")),
        "README.txt should be filtered out"
    );
}

#[test]
fn list_filter_by_glob_pattern() {
    let temp = tempdir().expect("tempdir");
    let archive = wrap_fixture(&temp);

    let output = Command::new(tarzan_bin())
        .args(["list", "-f"])
        .arg(&archive)
        .arg("*.txt")
        .output()
        .expect("list with glob");
    assert!(output.status.success());

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.lines().any(|l| l.ends_with("README.txt")),
        "README.txt should match *.txt"
    );
    assert!(
        !stdout.lines().any(|l| l.ends_with("main.rs")),
        "main.rs should NOT match *.txt"
    );
}

#[test]
fn list_json_respects_filter() {
    let temp = tempdir().expect("tempdir");
    let archive = wrap_fixture(&temp);

    let output = Command::new(tarzan_bin())
        .args(["list", "--json", "-f"])
        .arg(&archive)
        .arg("src/")
        .output()
        .expect("list --json src/");
    assert!(output.status.success());

    let stdout = String::from_utf8(output.stdout).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let arr = parsed.as_array().expect("top-level array");
    for entry in arr {
        let path = entry.get("path").unwrap().as_str().unwrap();
        assert!(
            path.contains("src"),
            "JSON entry leaked through filter: {path}"
        );
    }
}

#[test]
fn list_verbose_and_json_are_mutually_exclusive() {
    let temp = tempdir().expect("tempdir");
    let archive = wrap_fixture(&temp);

    let output = Command::new(tarzan_bin())
        .args(["list", "-v", "--json", "-f"])
        .arg(&archive)
        .output()
        .expect("list -v --json");
    assert!(
        !output.status.success(),
        "list -v --json should fail with a conflict error"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--verbose") && stderr.contains("--json"),
        "expected clap conflict message, got: {stderr}"
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
