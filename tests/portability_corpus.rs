//! Hostile-but-valid tars: one row per way an archive made elsewhere can
//! carry something this host cannot represent.
//!
//! Every row asserts the same contract. `wrap` accepts the tar and the bytes
//! round-trip. `extract` returns `Ok`, every portable member lands byte-exact,
//! and whatever could not be restored appears in the [`FidelityReport`]
//! rather than aborting the run or leaving wrong content behind. The
//! expected degradation differs by platform, so tests branch on `cfg!`
//! for the *expectation*, never for whether they run.
//!
//! Sparse entries are exercised in `edge_case_shapes.rs`; devices, FIFOs,
//! xattrs, hard links, name collisions and Windows-invalid names in
//! `fidelity.rs`. This file holds the rest of the taxonomy.

mod common;

use std::fs;
use std::io::Cursor;

use common::{Entry, build_tar, wrap, wrap_to_file};
use tarzan::{ExtractOptions, FidelityReport, LossKind, TarzanReader};
use tempfile::{TempDir, tempdir};

/// Wraps, checks the byte round-trip, extracts, and hands back the report
/// and destination for row-specific assertions.
fn run_row(raw: &[u8]) -> (TempDir, std::path::PathBuf, FidelityReport) {
    let temp = tempdir().unwrap();
    let wrapped = wrap(raw);
    assert_eq!(
        zstd::stream::decode_all(Cursor::new(&wrapped)).unwrap(),
        raw,
        "wrap must preserve the tar byte-for-byte"
    );
    let archive = wrap_to_file(raw, temp.path(), "row.tar.zst");
    let dest = temp.path().join("out");
    let report = TarzanReader::open(&archive)
        .unwrap()
        .extract_to_dir(&dest, &ExtractOptions::default(), |_| {})
        .expect("extract must not abort on portability problems");
    (temp, dest, report)
}

const SIBLING: Entry<'static> = Entry::File {
    path: "sibling.txt",
    mode: 0o644,
    content: b"portable",
};

fn assert_sibling(dest: &std::path::Path) {
    assert_eq!(fs::read(dest.join("sibling.txt")).unwrap(), b"portable");
}

// ── entry types tarzan never materialises ──────────────────────────────────

#[test]
fn unknown_type_byte_is_declined_with_the_byte_named() {
    let raw = build_tar(&[
        Entry::Other {
            path: "volume-continuation",
            type_byte: b'M',
        },
        SIBLING,
    ]);
    let (_t, dest, report) = run_row(&raw);
    assert_sibling(&dest);
    assert!(!dest.join("volume-continuation").exists());
    assert_eq!(report.declined.len(), 1, "{report:?}");
    assert_eq!(report.declined[0].kind, LossKind::Unsupported);
    assert!(
        report.declined[0].detail.contains("'M'"),
        "detail should name the raw type: {}",
        report.declined[0].detail
    );
}

// ── PAX records for facilities tarzan does not model ───────────────────────

#[test]
fn acl_records_are_ignored_without_complaint() {
    // star/GNU ACL encoding. tarzan indexes only SCHILY.xattr.* and
    // LIBARCHIVE.xattr.*; other vendor keys stay in the raw tar for a native
    // extractor and are neither restored nor reported.
    let raw = build_tar(&[
        Entry::Pax {
            records: &[
                ("SCHILY.acl.access", b"user::rw-,group::r--,other::r--"),
                ("SCHILY.acl.default", b"user::rwx,group::r-x,other::r-x"),
            ],
        },
        Entry::File {
            path: "with-acl.txt",
            mode: 0o644,
            content: b"acl",
        },
        SIBLING,
    ]);
    let (_t, dest, report) = run_row(&raw);
    assert_sibling(&dest);
    assert_eq!(fs::read(dest.join("with-acl.txt")).unwrap(), b"acl");
    assert!(report.is_clean(), "{report:?}");
}

#[test]
fn huge_uid_from_pax_is_indexed_and_extraction_succeeds() {
    let raw = build_tar(&[
        Entry::Pax {
            records: &[("uid", b"4294967296"), ("gid", b"4294967297")],
        },
        Entry::File {
            path: "owned.txt",
            mode: 0o644,
            content: b"who owns me",
        },
    ]);
    let temp = tempdir().unwrap();
    let archive = wrap_to_file(&raw, temp.path(), "a.tar.zst");
    let reader = TarzanReader::open(&archive).unwrap();
    let m = &reader.members()[0];
    assert_eq!(m.uid, 4_294_967_296);
    assert_eq!(m.gid, 4_294_967_297);

    let (_t, dest, report) = run_row(&raw);
    assert_eq!(fs::read(dest.join("owned.txt")).unwrap(), b"who owns me");
    // Ownership is never restored, so nothing to report.
    assert!(report.is_clean(), "{report:?}");
}

// ── timestamps outside comfortable ranges ──────────────────────────────────

/// Extracts a file whose PAX mtime is `record` and checks that either the
/// timestamp was restored exactly or its loss was reported. Content must
/// land regardless.
fn assert_mtime_row(record: &str, expected_secs: i64) {
    let raw = build_tar(&[
        Entry::Pax {
            records: &[("mtime", record.as_bytes())],
        },
        Entry::File {
            path: "stamped.txt",
            mode: 0o644,
            content: b"when",
        },
        SIBLING,
    ]);
    let (_t, dest, report) = run_row(&raw);
    assert_sibling(&dest);
    assert_eq!(fs::read(dest.join("stamped.txt")).unwrap(), b"when");

    let restored = filetime::FileTime::from_last_modification_time(
        &fs::metadata(dest.join("stamped.txt")).unwrap(),
    )
    .unix_seconds();
    let reported = report
        .metadata_lost
        .iter()
        .any(|l| l.kind == LossKind::Mtime && l.path == "stamped.txt");
    assert!(
        restored == expected_secs || reported,
        "mtime {record}: restored {restored}, expected {expected_secs}, and no Mtime loss reported: {report:?}"
    );
}

#[test]
fn mtime_before_1970_restores_or_reports() {
    assert_mtime_row("-86400", -86_400);
}

/// Characterisation, not aspiration: GNU tar and bsdtar write a pre-1970
/// mtime into the ustar header as a negative GNU base-256 field, and the
/// pinned tar-core release rejects negative base-256 numerics, so such
/// archives cannot be wrapped today. The error must at least be clear. When
/// tar-core accepts them this test should flip to a round-trip assertion
/// and the README known-gaps entry should go.
#[test]
fn negative_base256_header_mtime_is_rejected_with_a_clear_error() {
    let raw = build_tar(&[
        Entry::Base256MtimeFile {
            path: "moon-landing.txt",
            mtime: -14_182_916, // 1969-07-20T20:17:40Z
            content: b"one small step",
        },
        SIBLING,
    ]);
    let mut out = Vec::new();
    let err = tarzan::wrap(Cursor::new(&raw), &mut out, tarzan::WrapOptions::default())
        .expect_err("pinned tar-core rejects negative base-256 header fields");
    let text = format!("{err:#}");
    assert!(
        text.contains("tar header"),
        "error should point at header parsing: {text}"
    );
}

#[test]
fn mtime_past_2038_restores_or_reports() {
    assert_mtime_row("2147483648", 2_147_483_648);
}

#[test]
fn mtime_past_2106_restores_or_reports() {
    assert_mtime_row("4294967296", 4_294_967_296);
}

// ── names ──────────────────────────────────────────────────────────────────

#[test]
fn non_utf8_name_extracts_where_the_filesystem_allows_and_is_declined_elsewhere() {
    let raw = build_tar(&[
        Entry::RawNameFile {
            name: b"latin1-caf\xe9.txt",
            content: b"bytes",
        },
        SIBLING,
    ]);
    let (_t, dest, report) = run_row(&raw);
    assert_sibling(&dest);
    // APFS and NTFS require UTF-8/UTF-16 names; ext4, XFS, tmpfs take bytes.
    if cfg!(all(unix, not(target_os = "macos"))) {
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            use std::ffi::OsStr;
            use std::os::unix::ffi::OsStrExt;
            let on_disk = dest.join(OsStr::from_bytes(b"latin1-caf\xe9.txt"));
            assert_eq!(
                fs::read(&on_disk).unwrap(),
                b"bytes",
                "raw name bytes must be used"
            );
        }
        assert!(report.is_clean(), "{report:?}");
    } else {
        assert_eq!(report.declined.len(), 1, "{report:?}");
        assert_eq!(report.declined[0].kind, LossKind::InvalidName);
    }
}

#[test]
fn very_long_path_extracts_or_is_reported_never_aborts() {
    let component = "c".repeat(60);
    let path = format!("{component}/{component}/{component}/{component}/{component}/leaf.txt");
    assert!(path.len() > 260, "must exceed the classic Windows MAX_PATH");
    let raw = build_tar(&[
        // Over 100 bytes, so the builder emits a GNU long-name record.
        Entry::File {
            path: &path,
            mode: 0o644,
            content: b"deep",
        },
        SIBLING,
    ]);
    let (_t, dest, report) = run_row(&raw);
    assert_sibling(&dest);
    match fs::read(dest.join(&path)) {
        Ok(bytes) => {
            assert_eq!(bytes, b"deep");
            assert!(report.is_clean(), "{report:?}");
        }
        Err(_) => {
            assert!(
                report.declined.iter().any(|l| l.path == path),
                "leaf missing but not declined: {report:?}"
            );
        }
    }
}

#[test]
fn deep_nesting_is_ordinary() {
    let path = (0..40).map(|_| "d").collect::<Vec<_>>().join("/") + "/leaf.txt";
    let raw = build_tar(&[
        Entry::File {
            path: &path,
            mode: 0o644,
            content: b"deep",
        },
        SIBLING,
    ]);
    let (_t, dest, report) = run_row(&raw);
    assert_sibling(&dest);
    assert_eq!(fs::read(dest.join(&path)).unwrap(), b"deep");
    assert!(report.is_clean(), "{report:?}");
}

#[cfg(unix)]
#[test]
fn absolute_symlink_target_is_preserved_verbatim() {
    // The destination-escape rule applies to member *paths*, not to what a
    // symlink points at: tar creates these too, and rewriting them would
    // alter content the user asked for.
    let raw = build_tar(&[
        Entry::Symlink {
            path: "passwd-link",
            target: "/etc/passwd",
        },
        SIBLING,
    ]);
    let (_t, dest, report) = run_row(&raw);
    assert_sibling(&dest);
    assert_eq!(
        fs::read_link(dest.join("passwd-link")).unwrap(),
        std::path::Path::new("/etc/passwd")
    );
    assert!(report.is_clean(), "{report:?}");
}
