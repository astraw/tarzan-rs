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

#[cfg(unix)]
#[test]
fn extract_restores_file_mtime() {
    use std::os::unix::fs::MetadataExt;

    // Build a tree whose file mtimes are set to a known timestamp, archive
    // it, extract it, and confirm the round-tripped mtimes match.
    let temp = tempdir().expect("tempdir");
    let src = temp.path().join("src");
    fs::create_dir_all(src.join("inner")).unwrap();
    fs::write(src.join("inner/file.txt"), b"hi").unwrap();

    let stamped: i64 = 1_700_000_000; // 2023-11-14 22:13:20 UTC, far enough back to detect drift
    let ft = filetime::FileTime::from_unix_time(stamped, 0);
    filetime::set_file_mtime(src.join("inner/file.txt"), ft).unwrap();
    filetime::set_file_mtime(src.join("inner"), ft).unwrap();

    let tar_path = temp.path().join("input.tar");
    let mut cmd = Command::new("tar");
    #[cfg(target_os = "macos")]
    cmd.env("COPYFILE_DISABLE", "1");
    let status = cmd
        .arg("-cf")
        .arg(&tar_path)
        .arg("-C")
        .arg(&src)
        .arg(".")
        .status()
        .expect("tar");
    assert!(status.success());

    let archive = temp.path().join("a.tar.zst");
    let status = Command::new(tarzan_bin())
        .arg("wrap")
        .arg(&tar_path)
        .arg("-f")
        .arg(&archive)
        .status()
        .expect("wrap");
    assert!(status.success());

    let dest = temp.path().join("out");
    let status = Command::new(tarzan_bin())
        .args(["extract", "-f"])
        .arg(&archive)
        .arg("-C")
        .arg(&dest)
        .status()
        .expect("extract");
    assert!(status.success());

    let file_mtime = fs::metadata(dest.join("inner/file.txt")).unwrap().mtime();
    assert_eq!(
        file_mtime, stamped,
        "file mtime should be restored to the stamped value"
    );

    let dir_mtime = fs::metadata(dest.join("inner")).unwrap().mtime();
    assert_eq!(
        dir_mtime, stamped,
        "directory mtime should be restored to the stamped value (applied after children written)"
    );
}

#[cfg(unix)]
#[test]
fn extract_restores_hard_links() {
    use std::os::unix::fs::MetadataExt;

    let temp = tempdir().expect("tempdir");
    let src = temp.path().join("src");
    fs::create_dir(&src).unwrap();
    fs::write(src.join("original.txt"), b"shared content").unwrap();
    fs::hard_link(src.join("original.txt"), src.join("link.txt")).unwrap();

    let tar_path = temp.path().join("input.tar");
    let mut cmd = Command::new("tar");
    #[cfg(target_os = "macos")]
    cmd.env("COPYFILE_DISABLE", "1");
    let status = cmd
        .arg("-cf")
        .arg(&tar_path)
        .arg("-C")
        .arg(&src)
        .arg(".")
        .status()
        .expect("tar");
    assert!(status.success());

    let archive = temp.path().join("a.tar.zst");
    let status = Command::new(tarzan_bin())
        .arg("wrap")
        .arg(&tar_path)
        .arg("-f")
        .arg(&archive)
        .status()
        .expect("wrap");
    assert!(status.success());

    let dest = temp.path().join("out");
    let status = Command::new(tarzan_bin())
        .args(["extract", "-f"])
        .arg(&archive)
        .arg("-C")
        .arg(&dest)
        .status()
        .expect("extract");
    assert!(status.success());

    let orig = fs::metadata(dest.join("original.txt")).expect("original.txt");
    let link = fs::metadata(dest.join("link.txt")).expect("link.txt");
    assert_eq!(
        orig.ino(),
        link.ino(),
        "hard-linked entries should share an inode after extraction"
    );
    assert_eq!(
        fs::read(dest.join("link.txt")).unwrap(),
        b"shared content",
        "hard link should expose the shared content"
    );
}

#[cfg(unix)]
#[test]
fn extract_no_mtime_keeps_current_time() {
    use std::os::unix::fs::MetadataExt;
    use std::time::{SystemTime, UNIX_EPOCH};

    let temp = tempdir().expect("tempdir");
    let src = temp.path().join("src");
    fs::create_dir(&src).unwrap();
    fs::write(src.join("file.txt"), b"hi").unwrap();

    let stamped: i64 = 1_500_000_000; // 2017-07-14, clearly in the past
    let ft = filetime::FileTime::from_unix_time(stamped, 0);
    filetime::set_file_mtime(src.join("file.txt"), ft).unwrap();

    let tar_path = temp.path().join("input.tar");
    let mut cmd = Command::new("tar");
    #[cfg(target_os = "macos")]
    cmd.env("COPYFILE_DISABLE", "1");
    assert!(
        cmd.arg("-cf")
            .arg(&tar_path)
            .arg("-C")
            .arg(&src)
            .arg(".")
            .status()
            .expect("tar")
            .success()
    );

    let archive = temp.path().join("a.tar.zst");
    assert!(
        Command::new(tarzan_bin())
            .arg("wrap")
            .arg(&tar_path)
            .arg("-f")
            .arg(&archive)
            .status()
            .expect("wrap")
            .success()
    );

    let dest = temp.path().join("out");
    assert!(
        Command::new(tarzan_bin())
            .args(["extract", "-f"])
            .arg(&archive)
            .arg("-C")
            .arg(&dest)
            .arg("--no-mtime")
            .status()
            .expect("extract")
            .success()
    );

    let mtime = fs::metadata(dest.join("file.txt")).unwrap().mtime();
    assert_ne!(
        mtime, stamped,
        "--no-mtime should not restore the recorded timestamp"
    );
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    assert!(
        (now - mtime).abs() < 120,
        "with --no-mtime the file should carry a fresh timestamp (got {mtime}, now {now})"
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
