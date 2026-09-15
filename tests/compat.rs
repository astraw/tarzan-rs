//! Archives written by earlier tarzan releases must keep opening.
//!
//! `testdata/compat/` holds one archive per published release, all wrapping
//! the same bsdtar archive of `testdata/fixtures/tiny-tree` (see the README
//! there for how they were produced). The TOC schema only ever gains optional
//! fields, so every v2 archive must deserialise, list, extract, and verify
//! with the current code. The lone v1 archive must be rejected with the
//! message that points users at `zstd -d`.

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::tempdir;

/// One published release and what its `wrap` recorded.
struct Release {
    version: &'static str,
    has_md5: bool,
    has_mtime_ns: bool,
}

const V2_RELEASES: &[Release] = &[
    Release {
        version: "0.2.0",
        has_md5: false,
        has_mtime_ns: false,
    },
    Release {
        version: "0.2.1",
        has_md5: false,
        has_mtime_ns: false,
    },
    Release {
        version: "0.2.2",
        has_md5: false,
        has_mtime_ns: false,
    },
    Release {
        version: "0.3.0",
        has_md5: true,
        has_mtime_ns: false,
    },
    Release {
        version: "0.4.0",
        has_md5: true,
        has_mtime_ns: true,
    },
];

/// Regular files in the fixture tree, as recorded by `tar -C tiny-tree .`.
const REGULAR_FILES: &[&str] = &[
    "./README.txt",
    "./src/main.rs",
    "./data/numbers.csv",
    "./data/blob.bin",
];
const DIRECTORIES: &[&str] = &["./", "./data/", "./src/"];

fn manifest_path(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn fixture_archive(version: &str) -> PathBuf {
    manifest_path(&format!("testdata/compat/tarzan-v{version}.tar.zst"))
}

fn fixture_tree_file(archive_path: &str) -> PathBuf {
    manifest_path("testdata/fixtures/tiny-tree").join(archive_path.trim_start_matches("./"))
}

fn tarzan_bin() -> PathBuf {
    PathBuf::from(std::env::var("CARGO_BIN_EXE_tarzan").expect("missing tarzan test binary"))
}

fn run_tarzan(args: &[&str], archive: &Path) -> std::process::Output {
    Command::new(tarzan_bin())
        .args(args)
        .arg("-f")
        .arg(archive)
        .output()
        .expect("failed to spawn tarzan")
}

#[test]
fn every_release_has_a_fixture_on_disk() {
    for release in V2_RELEASES {
        assert!(
            fixture_archive(release.version).is_file(),
            "missing fixture for {}",
            release.version
        );
    }
    assert!(fixture_archive("0.1.2").is_file());
}

#[test]
fn v2_archives_open_and_list_the_same_members() {
    for release in V2_RELEASES {
        let reader = tarzan::TarzanReader::open(&fixture_archive(release.version))
            .unwrap_or_else(|e| panic!("v{} archive must open: {e:#}", release.version));
        assert_eq!(reader.identity_version(), 2, "v{}", release.version);

        let mut paths: Vec<&str> = reader.members().iter().map(|m| m.path.as_str()).collect();
        paths.sort_unstable();
        let mut expected: Vec<&str> = REGULAR_FILES.iter().chain(DIRECTORIES).copied().collect();
        expected.sort_unstable();
        assert_eq!(paths, expected, "member list for v{}", release.version);

        for member in reader.members() {
            let is_file = REGULAR_FILES.contains(&member.path.as_str());
            if is_file {
                let on_disk = fs::read(fixture_tree_file(&member.path)).unwrap();
                assert_eq!(
                    member.size,
                    on_disk.len() as u64,
                    "size of {} in v{}",
                    member.path,
                    release.version
                );
                assert!(
                    member.content_sha256.is_some(),
                    "v{} recorded content_sha256 for every regular file",
                    release.version
                );
                assert_eq!(
                    member.content_md5.is_some(),
                    release.has_md5,
                    "content_md5 presence for {} in v{}",
                    member.path,
                    release.version
                );
            }
            assert_eq!(
                member.mtime_ns.is_some(),
                release.has_mtime_ns,
                "mtime_ns presence for {} in v{}",
                member.path,
                release.version
            );
        }
    }
}

#[test]
fn v2_archives_extract_byte_exact_content() {
    for release in V2_RELEASES {
        let mut reader = tarzan::TarzanReader::open(&fixture_archive(release.version)).unwrap();
        for path in REGULAR_FILES {
            let mut out = Vec::new();
            reader
                .extract_member(path, &mut out)
                .unwrap_or_else(|e| panic!("extract {path} from v{}: {e:#}", release.version));
            assert_eq!(
                out,
                fs::read(fixture_tree_file(path)).unwrap(),
                "content of {path} from v{}",
                release.version
            );
        }
    }
}

#[test]
fn v2_archives_pass_full_and_quick_verify() {
    for release in V2_RELEASES {
        let archive = fixture_archive(release.version);
        for args in [&["verify"][..], &["verify", "--quick"][..]] {
            let out = run_tarzan(args, &archive);
            assert!(
                out.status.success(),
                "{args:?} on v{} failed:\n{}",
                release.version,
                String::from_utf8_lossy(&out.stderr)
            );
        }
        let mut reader = tarzan::TarzanReader::open(&archive).unwrap();
        reader
            .verify_archive_hash()
            .unwrap_or_else(|e| panic!("footer hash for v{}: {e:#}", release.version));
    }
}

#[test]
fn v2_archives_extract_to_disk_via_cli() {
    for release in V2_RELEASES {
        let temp = tempdir().unwrap();
        let out = Command::new(tarzan_bin())
            .args(["extract", "-f"])
            .arg(fixture_archive(release.version))
            .arg("-C")
            .arg(temp.path())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "extract v{}: {}",
            release.version,
            String::from_utf8_lossy(&out.stderr)
        );
        for path in REGULAR_FILES {
            let rel = path.trim_start_matches("./");
            assert_eq!(
                fs::read(temp.path().join(rel)).unwrap(),
                fs::read(fixture_tree_file(path)).unwrap(),
                "{rel} from v{}",
                release.version
            );
        }
    }
}

#[test]
fn v1_archive_is_rejected_with_guidance_but_remains_valid_zstd() {
    let archive = fixture_archive("0.1.2");

    let msg = match tarzan::TarzanReader::open(&archive) {
        Ok(_) => panic!("v1 archives are not supported and must not open"),
        Err(err) => format!("{err:#}"),
    };
    assert!(
        msg.contains("v1"),
        "error should name the legacy format: {msg}"
    );
    assert!(
        msg.contains("zstd -d"),
        "error should point at the standard-tool fallback: {msg}"
    );

    // The standard-tools escape hatch the message promises must actually work.
    let decoded = zstd::stream::decode_all(Cursor::new(fs::read(&archive).unwrap())).unwrap();
    assert_eq!(
        decoded.len() % 512,
        0,
        "decoded payload must be a tar stream"
    );
    assert!(
        decoded
            .windows(b"README.txt".len())
            .any(|w| w == b"README.txt"),
        "decoded tar must contain the fixture member names"
    );
}
