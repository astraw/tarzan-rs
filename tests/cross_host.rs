//! Consume archives produced by `ci/produce.py` on other hosts.
//!
//! Ignored by default; CI runs it with `--ignored` after downloading every
//! producer's artifact into a subdirectory of `CROSS_HOST_DIR`. Locally:
//!
//! ```sh
//! python3 ci/produce.py /tmp/xh/local target/release/tarzan
//! CROSS_HOST_DIR=/tmp/xh cargo test --test cross_host -- --ignored
//! ```
//!
//! For every archive: the zstd stream decodes to the producer's tar
//! byte-for-byte, both verify modes pass, every manifest path is listed,
//! extraction succeeds, every regular file's SHA-256 matches the producer's
//! manifest, and whatever could not be restored is confined to kinds the
//! design predicts for this producer/consumer pair. A summary row per archive
//! is appended to `$GITHUB_STEP_SUMMARY` when set.

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;
use sha2::{Digest, Sha256};
use tarzan::{ExtractOptions, LossKind, TarzanReader};

fn tarzan_bin() -> PathBuf {
    PathBuf::from(std::env::var("CARGO_BIN_EXE_tarzan").expect("missing tarzan test binary"))
}

fn producer_dirs(root: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(root)
        .unwrap_or_else(|e| panic!("CROSS_HOST_DIR {}: {e}", root.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.join("manifest.json").is_file())
        .collect();
    // Also accept CROSS_HOST_DIR pointing directly at one producer dir.
    if dirs.is_empty() && root.join("manifest.json").is_file() {
        dirs.push(root.to_path_buf());
    }
    dirs.sort();
    dirs
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn strip_dot(p: &str) -> &str {
    p.trim_start_matches("./").trim_end_matches('/')
}

/// Kinds of loss the design predicts for an archive from `producer` when
/// consumed here. Anything outside this set fails the test.
fn expected_loss_kinds(producer: &str) -> Vec<LossKind> {
    let mut kinds = vec![
        // Host-specific attribute namespaces never travel (com.apple.*,
        // security.*, system.posix_acl_*), and some CI mounts refuse user.*.
        LossKind::Xattr,
    ];
    if !cfg!(unix) {
        kinds.push(LossKind::Symlink);
        kinds.push(LossKind::HardLink);
    }
    if producer == "Darwin" {
        // AppleDouble companions (`._x`) are ordinary members to tarzan and
        // never collide; nothing extra expected.
    }
    kinds
}

fn append_summary(line: &str) {
    if let Ok(path) = std::env::var("GITHUB_STEP_SUMMARY") {
        use std::io::Write;
        if let Ok(mut f) = fs::OpenOptions::new().append(true).create(true).open(path) {
            let _ = writeln!(f, "{line}");
        }
    }
}

#[test]
#[ignore = "needs CROSS_HOST_DIR from ci/produce.py"]
fn archives_from_every_producer_consume_here() {
    let root = PathBuf::from(
        std::env::var_os("CROSS_HOST_DIR").expect("set CROSS_HOST_DIR to the producer outputs"),
    );
    let dirs = producer_dirs(&root);
    assert!(
        !dirs.is_empty(),
        "no producer directories under {}",
        root.display()
    );

    append_summary(&format!(
        "\n### cross-host consume on {} {}\n\n| producer | archive | members | written | declined | metadata lost |\n|---|---|---|---|---|---|",
        std::env::consts::OS,
        std::env::consts::ARCH
    ));

    let mut failures = Vec::new();
    for dir in &dirs {
        let manifest: Value =
            serde_json::from_slice(&fs::read(dir.join("manifest.json")).unwrap()).unwrap();
        let producer = manifest["producer"]["system"]
            .as_str()
            .unwrap_or("?")
            .to_owned();
        let files = manifest["files"].as_array().cloned().unwrap_or_default();
        let symlinks = manifest["symlinks"].as_array().cloned().unwrap_or_default();
        let allowed = expected_loss_kinds(&producer);

        for archive in manifest["archives"].as_array().cloned().unwrap_or_default() {
            let label = format!("{producer}/{}", archive["name"].as_str().unwrap_or("?"));
            let tar_path = dir.join(archive["tar"].as_str().unwrap());
            let zst_path = dir.join(archive["tarzan"].as_str().unwrap());
            let mut problems = Vec::new();

            // 1. Byte round-trip survived production and artifact transit.
            let decoded = zstd::stream::decode_all(Cursor::new(fs::read(&zst_path).unwrap()))
                .unwrap_or_else(|e| panic!("{label}: zstd decode: {e}"));
            if decoded != fs::read(&tar_path).unwrap() {
                problems.push("zstd payload differs from producer's tar".to_owned());
            }

            // 2. Both verify modes.
            for args in [&["verify"][..], &["verify", "--quick"][..]] {
                let out = Command::new(tarzan_bin())
                    .args(args)
                    .arg("-f")
                    .arg(&zst_path)
                    .output()
                    .unwrap();
                if !out.status.success() {
                    problems.push(format!(
                        "{args:?} failed: {}",
                        String::from_utf8_lossy(&out.stderr)
                    ));
                }
            }

            // 3. Listing covers the manifest.
            let mut reader = TarzanReader::open(&zst_path).unwrap();
            let listed: Vec<String> = reader
                .members()
                .iter()
                .map(|m| strip_dot(&m.path).to_owned())
                .collect();
            for f in &files {
                let p = strip_dot(f["path"].as_str().unwrap());
                if !listed.iter().any(|l| l == p) {
                    problems.push(format!("manifest file {p} not listed"));
                }
            }

            // 4. Extract and compare content.
            let dest = tempfile::tempdir().unwrap();
            let report = reader
                .extract_to_dir(dest.path(), &ExtractOptions::default(), |_| {})
                .unwrap_or_else(|e| panic!("{label}: extract aborted: {e:#}"));
            for f in &files {
                let rel = strip_dot(f["path"].as_str().unwrap());
                let want = f["sha256"].as_str().unwrap();
                match fs::read(dest.path().join(rel)) {
                    Ok(bytes) if sha256_hex(&bytes) == want => {}
                    Ok(_) => problems.push(format!("content mismatch: {rel}")),
                    Err(e) => problems.push(format!("missing regular file {rel}: {e}")),
                }
            }
            for s in &symlinks {
                let rel = strip_dot(s["path"].as_str().unwrap());
                let target = s["target"].as_str().unwrap();
                match fs::read_link(dest.path().join(rel)) {
                    Ok(t) => {
                        if t != Path::new(target) {
                            problems
                                .push(format!("symlink {rel} -> {} (want {target})", t.display()));
                        }
                    }
                    Err(_) => {
                        let declined = report
                            .declined
                            .iter()
                            .any(|l| strip_dot(&l.path) == rel && l.kind == LossKind::Symlink);
                        if !declined {
                            problems.push(format!("symlink {rel} missing and not declined"));
                        }
                    }
                }
            }

            // 5. Degradations are the predicted ones, and never touch a
            //    manifest regular file's content.
            for loss in report.losses() {
                if !allowed.contains(&loss.kind) {
                    problems.push(format!(
                        "unexpected loss {:?} on {}: {}",
                        loss.kind, loss.path, loss.detail
                    ));
                }
                if loss.kind.is_decline() {
                    let p = strip_dot(&loss.path);
                    if files
                        .iter()
                        .any(|f| strip_dot(f["path"].as_str().unwrap()) == p)
                    {
                        problems.push(format!("regular file {p} was declined: {}", loss.detail));
                    }
                }
            }

            append_summary(&format!(
                "| {producer} | {} | {} | {} | {} | {} |",
                archive["name"].as_str().unwrap_or("?"),
                reader.members().len(),
                report.members_written,
                report.declined.len(),
                report.metadata_lost.len()
            ));
            eprintln!(
                "{label}: {} members, {} written, {} declined, {} metadata lost{}",
                reader.members().len(),
                report.members_written,
                report.declined.len(),
                report.metadata_lost.len(),
                if report.is_clean() { "" } else { "\n" }
            );
            if !report.is_clean() {
                eprintln!("{}", report.summary());
            }
            if !problems.is_empty() {
                failures.push(format!("{label}:\n  {}", problems.join("\n  ")));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "cross-host consumption problems:\n{}",
        failures.join("\n")
    );
}
