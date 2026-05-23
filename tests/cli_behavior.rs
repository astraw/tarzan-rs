//! CLI-level behavior tests covering gaps not exercised elsewhere:
//! `-v` timestamp rendering, `wrap -v` progress output, corruption
//! detection, `cat` on non-file entries, and `file(1)` recognition.

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::tempdir;

fn tarzan_bin() -> PathBuf {
    PathBuf::from(std::env::var("CARGO_BIN_EXE_tarzan").expect("missing tarzan test binary"))
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testdata/fixtures/tiny-tree")
        .canonicalize()
        .expect("fixture path should exist")
}

/// `tar`s a directory's contents into `tar_path`.
fn tar_directory(dir: &Path, tar_path: &Path) {
    let mut cmd = Command::new("tar");
    #[cfg(target_os = "macos")]
    cmd.env("COPYFILE_DISABLE", "1");
    let status = cmd
        .arg("-cf")
        .arg(tar_path)
        .arg("-C")
        .arg(dir)
        .arg(".")
        .status()
        .expect("failed to run tar");
    assert!(status.success(), "tar command failed");
}

/// Wraps in-memory tar bytes into an archive file via the library.
fn wrap_bytes(temp: &tempfile::TempDir, tar: &[u8]) -> PathBuf {
    let archive = temp.path().join("archive.tar.zst");
    let out = fs::File::create(&archive).expect("create archive");
    tarzan::wrap(Cursor::new(tar), out, tarzan::WrapOptions::default())
        .expect("wrap should succeed");
    archive
}

/// Wraps the tiny-tree fixture into an archive file.
fn wrap_fixture(temp: &tempfile::TempDir) -> PathBuf {
    let tar_path = temp.path().join("input.tar");
    tar_directory(&fixture_root(), &tar_path);
    wrap_bytes(temp, &fs::read(&tar_path).expect("read fixture tar"))
}

/// Builds an in-memory tar with one regular file carrying `mtime`.
fn file_tar_with_mtime(name: &str, mtime: u64) -> Vec<u8> {
    let data = b"hello\n";
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(data.len() as u64);
    header.set_mode(0o644);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(mtime);
    header.set_entry_type(tar::EntryType::Regular);
    builder
        .append_data(&mut header, name, &data[..])
        .expect("append file to tar");
    builder.into_inner().expect("finish tar")
}

#[test]
fn list_verbose_renders_local_time_then_utc() {
    // 1_700_000_000 == 2023-11-14 22:13 UTC == 2023-11-14 17:13 in New York
    // (EST, UTC-5, since DST has ended by mid-November).
    let temp = tempdir().expect("tempdir");
    let archive = wrap_bytes(&temp, &file_tar_with_mtime("stamp.txt", 1_700_000_000));

    // Default: local time, honoring $TZ — matches `tar -tvf`.
    let output = Command::new(tarzan_bin())
        .env("TZ", "America/New_York")
        .args(["list", "-v", "-f"])
        .arg(&archive)
        .output()
        .expect("failed to run tarzan list -v");
    assert!(output.status.success(), "list -v failed");
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    assert!(
        stdout.contains("2023-11-14 17:13"),
        "expected local time 17:13, got: {stdout}"
    );

    // --utc: ignores $TZ and renders in UTC — matches `tar --utc -tvf`.
    let output = Command::new(tarzan_bin())
        .env("TZ", "America/New_York")
        .args(["list", "-v", "--utc", "-f"])
        .arg(&archive)
        .output()
        .expect("failed to run tarzan list -v --utc");
    assert!(output.status.success(), "list -v --utc failed");
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    assert!(
        stdout.contains("2023-11-14 22:13"),
        "expected UTC time 22:13, got: {stdout}"
    );
}

#[test]
fn wrap_verbose_lists_members_on_stderr() {
    let temp = tempdir().expect("tempdir");
    let tar_path = temp.path().join("input.tar");
    let archive = temp.path().join("out.tar.zst");
    tar_directory(&fixture_root(), &tar_path);

    let output = Command::new(tarzan_bin())
        .args(["wrap", "-v"])
        .arg(&tar_path)
        .arg("-f")
        .arg(&archive)
        .output()
        .expect("failed to run tarzan wrap -v");
    assert!(
        output.status.success(),
        "wrap -v failed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8(output.stderr).expect("utf8");
    for expected in ["README.txt", "src/main.rs"] {
        assert!(
            stderr.contains(expected),
            "expected {expected} in wrap -v output: {stderr}"
        );
    }
}

#[test]
fn verify_detects_corrupted_data() {
    let temp = tempdir().expect("tempdir");
    let archive = wrap_fixture(&temp);

    // Flip a byte a third of the way in: past the 14-byte identity frame and
    // inside the compressed data, before the trailing TOC.
    let mut bytes = fs::read(&archive).expect("read archive");
    let target = bytes.len() / 3;
    bytes[target] ^= 0xff;
    let corrupt = temp.path().join("corrupt.tar.zst");
    fs::write(&corrupt, &bytes).expect("write corrupt archive");

    let status = Command::new(tarzan_bin())
        .args(["verify", "-f"])
        .arg(&corrupt)
        .status()
        .expect("failed to run tarzan verify");
    assert!(
        !status.success(),
        "verify should exit non-zero on a corrupted archive"
    );
}

#[test]
fn cat_rejects_a_directory() {
    let temp = tempdir().expect("tempdir");
    let archive = wrap_fixture(&temp);

    let output = Command::new(tarzan_bin())
        .args(["cat", "-f"])
        .arg(&archive)
        .arg("./src/")
        .output()
        .expect("failed to run tarzan cat");
    assert!(
        !output.status.success(),
        "cat of a directory entry should fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not a regular file"),
        "expected a 'not a regular file' error, got: {stderr}"
    );
}

#[cfg(unix)]
#[test]
fn cat_rejects_a_symlink() {
    let temp = tempdir().expect("tempdir");
    let src = temp.path().join("src");
    fs::create_dir(&src).expect("create src dir");
    fs::write(src.join("real.txt"), b"real\n").expect("write file");
    std::os::unix::fs::symlink("real.txt", src.join("link.txt")).expect("create symlink");

    let tar_path = temp.path().join("input.tar");
    tar_directory(&src, &tar_path);
    let archive = wrap_bytes(&temp, &fs::read(&tar_path).expect("read tar"));

    let output = Command::new(tarzan_bin())
        .args(["cat", "-f"])
        .arg(&archive)
        .arg("./link.txt")
        .output()
        .expect("failed to run tarzan cat");
    assert!(
        !output.status.success(),
        "cat of a symlink entry should fail"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("not a regular file"),
        "expected a 'not a regular file' error"
    );
}

#[test]
fn file_magic_identifies_tarzan_archive() {
    if Command::new("file").arg("--version").output().is_err() {
        eprintln!("skipping file_magic test: `file` is not available");
        return;
    }

    let temp = tempdir().expect("tempdir");
    let archive = wrap_fixture(&temp);
    let magic = Path::new(env!("CARGO_MANIFEST_DIR")).join("contrib/tarzan.magic");

    // Use the MAGIC env var rather than `-m` so our pattern takes sole
    // precedence. On macOS, `-m` adds to the compiled system magic which
    // detects the embedded zstd via `indirect` and wins on strength; the
    // MAGIC env var replaces the default entirely.
    let output = Command::new("file")
        .env("MAGIC", &magic)
        .arg(&archive)
        .output()
        .expect("failed to run file");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("tarzan archive v1"),
        "`file` did not recognize the archive via the magic pattern: {stdout}"
    );
}
