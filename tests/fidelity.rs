//! The extract contract: content is guaranteed, metadata is attempted, and
//! the difference is reported.
//!
//! Every test here builds its tar in memory (see `common`) so the expected
//! degradation depends only on the platform the test runs on, never on what
//! the host attached to some fixture file.

mod common;

use std::fs;
use std::process::Command;

use common::{Entry, build_tar, tarzan_bin, wrap_to_file};
use tarzan::{ExtractOptions, LossKind, StrictFidelityError, TarzanReader};
use tempfile::tempdir;

fn extract(
    archive: &std::path::Path,
    dest: &std::path::Path,
    opts: &ExtractOptions,
) -> anyhow::Result<tarzan::FidelityReport> {
    TarzanReader::open(archive)
        .unwrap()
        .extract_to_dir(dest, opts, |_| {})
}

fn cli_extract(
    archive: &std::path::Path,
    dest: &std::path::Path,
    extra: &[&str],
) -> std::process::Output {
    Command::new(tarzan_bin())
        .args(["extract", "-f"])
        .arg(archive)
        .arg("-C")
        .arg(dest)
        .args(extra)
        .output()
        .expect("spawn tarzan")
}

#[test]
fn clean_archive_yields_clean_report_and_silent_cli() {
    let temp = tempdir().unwrap();
    let raw = build_tar(&[
        Entry::Dir {
            path: "d/",
            mode: 0o755,
        },
        Entry::File {
            path: "d/a.txt",
            mode: 0o644,
            content: b"alpha",
        },
    ]);
    let archive = wrap_to_file(&raw, temp.path(), "a.tar.zst");

    let report = extract(
        &archive,
        &temp.path().join("lib"),
        &ExtractOptions::default(),
    )
    .unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(report.members_written, 2);
    assert_eq!(report.summary(), "");

    let out = cli_extract(&archive, &temp.path().join("cli"), &[]);
    assert!(out.status.success());
    assert!(
        out.stderr.is_empty(),
        "clean extraction must be silent: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// An xattr no platform accepts: over 127 bytes (macOS rejects the length),
/// and outside every Linux namespace.
fn unrestorable_xattr_tar() -> Vec<u8> {
    let name = format!("SCHILY.xattr.{}", "x".repeat(200));
    build_tar(&[
        Entry::Pax {
            records: &[(&name, b"value")],
        },
        Entry::File {
            path: "keep.txt",
            mode: 0o644,
            content: b"content survives",
        },
        Entry::File {
            path: "other.txt",
            mode: 0o644,
            content: b"untouched",
        },
    ])
}

#[cfg(unix)]
#[test]
fn unrestorable_xattr_is_reported_not_fatal() {
    let temp = tempdir().unwrap();
    let archive = wrap_to_file(&unrestorable_xattr_tar(), temp.path(), "a.tar.zst");
    let dest = temp.path().join("out");

    let report = extract(&archive, &dest, &ExtractOptions::default()).unwrap();
    assert_eq!(report.members_written, 2);
    assert!(report.declined.is_empty(), "{report:?}");
    assert_eq!(report.metadata_lost.len(), 1, "{report:?}");
    let loss = &report.metadata_lost[0];
    assert_eq!(loss.kind, LossKind::Xattr);
    assert_eq!(loss.path, "keep.txt");
    // The TOC stores the attribute name without the PAX `SCHILY.xattr.` prefix.
    assert!(loss.detail.starts_with("xxxx"), "{}", loss.detail);
    assert_eq!(
        fs::read(dest.join("keep.txt")).unwrap(),
        b"content survives"
    );
    assert_eq!(fs::read(dest.join("other.txt")).unwrap(), b"untouched");
}

#[cfg(unix)]
#[test]
fn cli_prints_summary_and_exits_zero_without_strict() {
    let temp = tempdir().unwrap();
    let archive = wrap_to_file(&unrestorable_xattr_tar(), temp.path(), "a.tar.zst");
    let out = cli_extract(&archive, &temp.path().join("out"), &[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("extracted 2 members; 0 not written, 1 metadata item not restored:"),
        "{stderr}"
    );
    assert!(stderr.contains("\n  xattrs: 1 (keep.txt: xxxx"), "{stderr}");
}

#[cfg(unix)]
#[test]
fn strict_exits_two_after_extracting_everything() {
    let temp = tempdir().unwrap();
    let archive = wrap_to_file(&unrestorable_xattr_tar(), temp.path(), "a.tar.zst");
    let dest = temp.path().join("out");

    let out = cli_extract(&archive, &dest, &["--strict"]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--strict: treating the above as failure"),
        "{stderr}"
    );
    assert!(stderr.contains("xattrs: 1"), "{stderr}");
    // The verdict changed; the tree did not.
    assert_eq!(
        fs::read(dest.join("keep.txt")).unwrap(),
        b"content survives"
    );
    assert_eq!(fs::read(dest.join("other.txt")).unwrap(), b"untouched");

    // Library: the error carries the same report.
    let dest2 = temp.path().join("out2");
    let err = extract(
        &archive,
        &dest2,
        &ExtractOptions {
            strict: true,
            ..Default::default()
        },
    )
    .expect_err("strict must fail");
    let strict = err
        .downcast_ref::<StrictFidelityError>()
        .expect("error must be StrictFidelityError");
    assert_eq!(strict.0.metadata_lost.len(), 1);
    assert!(dest2.join("keep.txt").is_file(), "strict still extracts");
}

#[test]
fn hard_failures_still_exit_one() {
    let temp = tempdir().unwrap();
    let out = Command::new(tarzan_bin())
        .args(["extract", "-f"])
        .arg(temp.path().join("does-not-exist.tar.zst"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn devices_and_fifos_are_declined_and_leave_nothing_behind() {
    let temp = tempdir().unwrap();
    let raw = build_tar(&[
        Entry::CharDevice { path: "dev/null" },
        Entry::Fifo { path: "pipe" },
        Entry::File {
            path: "sibling.txt",
            mode: 0o644,
            content: b"still here",
        },
    ]);
    let archive = wrap_to_file(&raw, temp.path(), "a.tar.zst");
    let dest = temp.path().join("out");

    let report = extract(&archive, &dest, &ExtractOptions::default()).unwrap();
    assert_eq!(report.members_written, 1);
    let kinds: Vec<LossKind> = report.declined.iter().map(|l| l.kind).collect();
    assert_eq!(kinds, vec![LossKind::Device, LossKind::Fifo], "{report:?}");
    assert!(
        !dest.join("dev/null").exists(),
        "nothing may be written for a device"
    );
    assert!(
        !dest.join("pipe").exists(),
        "nothing may be written for a fifo"
    );
    assert_eq!(fs::read(dest.join("sibling.txt")).unwrap(), b"still here");

    let s = report.summary();
    assert!(
        s.contains("device nodes: 1 (dev/null: not materialised)"),
        "{s}"
    );
    assert!(s.contains("fifos: 1 (pipe: not materialised)"), "{s}");
}

#[test]
fn hard_link_without_its_target_is_declined() {
    let temp = tempdir().unwrap();
    let raw = build_tar(&[
        Entry::File {
            path: "a.txt",
            mode: 0o644,
            content: b"a",
        },
        Entry::HardLink {
            path: "b.txt",
            target: "a.txt",
        },
    ]);
    let archive = wrap_to_file(&raw, temp.path(), "a.tar.zst");
    let dest = temp.path().join("out");

    // Select only the link; its target is filtered out, so it cannot be made.
    let report = extract(
        &archive,
        &dest,
        &ExtractOptions {
            includes: vec!["b.txt".to_owned()],
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(report.members_written, 0);
    assert_eq!(report.declined.len(), 1);
    assert_eq!(report.declined[0].kind, LossKind::HardLink);
    assert!(!dest.join("b.txt").exists());

    // With both selected the link is made and counted.
    let dest2 = temp.path().join("out2");
    let report = extract(&archive, &dest2, &ExtractOptions::default()).unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(report.members_written, 2);
}

/// xattrs must be applied before permission bits: on Linux, setting a
/// `user.*` attribute needs write permission on the file, so a read-only
/// member restored mode-first would lose its xattrs. Timestamps come last
/// so neither step disturbs them.
#[cfg(unix)]
#[test]
fn read_only_file_keeps_xattr_mode_and_mtime() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let temp = tempdir().unwrap();
    let raw = build_tar(&[
        Entry::Pax {
            records: &[
                ("SCHILY.xattr.user.tag", b"v"),
                ("mtime", b"1500000000.250000000"),
            ],
        },
        Entry::File {
            path: "ro.txt",
            mode: 0o444,
            content: b"read only",
        },
    ]);
    let archive = wrap_to_file(&raw, temp.path(), "a.tar.zst");
    let dest = temp.path().join("out");

    let report = extract(&archive, &dest, &ExtractOptions::default()).unwrap();
    let target = dest.join("ro.txt");
    let meta = fs::metadata(&target).unwrap();
    assert_eq!(meta.permissions().mode() & 0o777, 0o444);
    assert_eq!(meta.mtime(), 1_500_000_000);
    assert_eq!(meta.mtime_nsec(), 250_000_000);

    match xattr::get(&target, "user.tag") {
        Ok(Some(v)) => {
            assert_eq!(v, b"v");
            assert!(report.is_clean(), "{report:?}");
        }
        // Some filesystems (tmpfs on older kernels, some CI mounts) refuse
        // user xattrs entirely; then the loss must be reported, not hidden.
        Ok(None) | Err(_) => {
            assert!(
                report
                    .metadata_lost
                    .iter()
                    .any(|l| l.kind == LossKind::Xattr),
                "xattr missing on disk but not reported: {report:?}"
            );
        }
    }
}

#[cfg(not(unix))]
#[test]
fn symlinks_are_declined_on_platforms_without_them() {
    let temp = tempdir().unwrap();
    let raw = build_tar(&[
        Entry::File {
            path: "real.txt",
            mode: 0o644,
            content: b"real",
        },
        Entry::Symlink {
            path: "link.txt",
            target: "real.txt",
        },
    ]);
    let archive = wrap_to_file(&raw, temp.path(), "a.tar.zst");
    let dest = temp.path().join("out");

    let report = extract(&archive, &dest, &ExtractOptions::default()).unwrap();
    assert_eq!(fs::read(dest.join("real.txt")).unwrap(), b"real");
    assert!(!dest.join("link.txt").exists());
    assert_eq!(report.declined.len(), 1);
    assert_eq!(report.declined[0].kind, LossKind::Symlink);
}

#[test]
fn skip_bad_chunks_declines_and_removes_the_partial_file() {
    let temp = tempdir().unwrap();
    let raw = build_tar(&[
        Entry::File {
            path: "good.txt",
            mode: 0o644,
            content: b"good",
        },
        Entry::File {
            path: "bad.bin",
            mode: 0o644,
            content: &[0xAB; 4096],
        },
    ]);
    // Wrap with a small chunk size so each member is in its own frame, then
    // corrupt the middle of the second member's frame.
    let mut wrapped = Vec::new();
    tarzan::wrap(
        std::io::Cursor::new(&raw),
        &mut wrapped,
        tarzan::WrapOptions::default().chunk_size(1024),
    )
    .unwrap();
    let toc_reader = {
        let path = temp.path().join("probe.tar.zst");
        fs::write(&path, &wrapped).unwrap();
        TarzanReader::open(&path).unwrap()
    };
    let bad = toc_reader
        .members()
        .iter()
        .find(|m| m.path == "bad.bin")
        .unwrap();
    let chunk = &bad.chunks[0];
    let mid = (chunk.compressed_offset + chunk.compressed_size / 2) as usize;
    for b in &mut wrapped[mid..mid + 8] {
        *b ^= 0xFF;
    }
    let archive = temp.path().join("corrupt.tar.zst");
    fs::write(&archive, &wrapped).unwrap();
    let dest = temp.path().join("out");

    let report = extract(
        &archive,
        &dest,
        &ExtractOptions {
            skip_bad_chunks: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(fs::read(dest.join("good.txt")).unwrap(), b"good");
    assert!(
        !dest.join("bad.bin").exists(),
        "partial file must be removed"
    );
    assert_eq!(report.declined.len(), 1);
    assert_eq!(report.declined[0].kind, LossKind::BadData);
    assert_eq!(report.declined[0].path, "bad.bin");
}

/// Does the filesystem under `dir` fold two names onto one entry?
fn folds(dir: &std::path::Path, a: &str, b: &str) -> bool {
    fs::write(dir.join(a), b"probe").unwrap();
    let folded = dir.join(b).exists();
    let _ = fs::remove_file(dir.join(a));
    folded
}

/// Two archive names the filesystem may fold onto one entry. On a folding
/// filesystem (APFS, NTFS) the second member must be declined and the first
/// left intact; elsewhere both are extracted. Either way, nothing silently
/// overwrites another member's content.
fn assert_fold_contract(first: &str, second: &str) {
    let temp = tempdir().unwrap();
    let raw = build_tar(&[
        Entry::File {
            path: first,
            mode: 0o644,
            content: b"first",
        },
        Entry::File {
            path: second,
            mode: 0o644,
            content: b"second",
        },
    ]);
    let archive = wrap_to_file(&raw, temp.path(), "a.tar.zst");
    let dest = temp.path().join("out");
    fs::create_dir(&dest).unwrap();
    let folding = folds(&dest, first, second);

    let report = extract(&archive, &dest, &ExtractOptions::default()).unwrap();
    assert_eq!(
        fs::read(dest.join(first)).unwrap(),
        b"first",
        "first member's content must survive"
    );
    if folding {
        assert_eq!(report.members_written, 1);
        assert_eq!(report.declined.len(), 1, "{report:?}");
        assert_eq!(report.declined[0].kind, LossKind::NameCollision);
        assert_eq!(report.declined[0].path, second);
        assert!(
            report.declined[0].detail.contains(first),
            "detail should name the surviving member: {}",
            report.declined[0].detail
        );
    } else {
        assert!(report.is_clean(), "{report:?}");
        assert_eq!(fs::read(dest.join(second)).unwrap(), b"second");
    }
}

#[test]
fn case_folding_filesystems_decline_the_second_name() {
    assert_fold_contract("Readme.txt", "README.txt");
}

#[test]
fn normalization_folding_filesystems_decline_the_second_name() {
    // U+00E9 (NFC) versus e + U+0301 (NFD).
    assert_fold_contract("caf\u{e9}.txt", "cafe\u{301}.txt");
}

#[test]
fn duplicate_archive_paths_let_the_later_entry_win() {
    let temp = tempdir().unwrap();
    let raw = build_tar(&[
        Entry::File {
            path: "same.txt",
            mode: 0o644,
            content: b"old",
        },
        Entry::File {
            path: "same.txt",
            mode: 0o644,
            content: b"new",
        },
    ]);
    let archive = wrap_to_file(&raw, temp.path(), "a.tar.zst");
    let dest = temp.path().join("out");
    let report = extract(&archive, &dest, &ExtractOptions::default()).unwrap();
    assert!(
        report.is_clean(),
        "same path twice is tar's overwrite, not a collision: {report:?}"
    );
    assert_eq!(fs::read(dest.join("same.txt")).unwrap(), b"new");
}

#[test]
fn pre_existing_files_in_the_destination_are_overwritten_as_tar_does() {
    let temp = tempdir().unwrap();
    let raw = build_tar(&[Entry::File {
        path: "x.txt",
        mode: 0o644,
        content: b"from archive",
    }]);
    let archive = wrap_to_file(&raw, temp.path(), "a.tar.zst");
    let dest = temp.path().join("out");
    fs::create_dir(&dest).unwrap();
    fs::write(dest.join("x.txt"), b"stale").unwrap();
    let report = extract(&archive, &dest, &ExtractOptions::default()).unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(fs::read(dest.join("x.txt")).unwrap(), b"from archive");
}

/// Names Windows cannot create: a reserved device name, a colon, a trailing
/// dot, a forbidden character inside a directory component.
const WINDOWS_HOSTILE: &[(&str, &[u8])] = &[
    ("CON.txt", b"reserved"),
    ("a:b.txt", b"colon"),
    ("trailing.", b"dot"),
    ("dir?/inner.txt", b"question"),
];

fn windows_hostile_tar() -> Vec<u8> {
    let mut entries: Vec<Entry<'_>> = WINDOWS_HOSTILE
        .iter()
        .map(|(path, content)| Entry::File {
            path,
            mode: 0o644,
            content,
        })
        .collect();
    entries.push(Entry::File {
        path: "plain.txt",
        mode: 0o644,
        content: b"plain",
    });
    build_tar(&entries)
}

#[cfg(windows)]
#[test]
fn names_windows_cannot_create_are_declined_not_fatal() {
    let temp = tempdir().unwrap();
    let archive = wrap_to_file(&windows_hostile_tar(), temp.path(), "a.tar.zst");
    let dest = temp.path().join("out");
    let report = extract(&archive, &dest, &ExtractOptions::default()).unwrap();
    assert_eq!(fs::read(dest.join("plain.txt")).unwrap(), b"plain");
    assert_eq!(report.members_written, 1);
    let declined: Vec<&str> = report.declined.iter().map(|l| l.path.as_str()).collect();
    let expected: Vec<&str> = WINDOWS_HOSTILE.iter().map(|(p, _)| *p).collect();
    assert_eq!(declined, expected, "{report:?}");
    assert!(
        report
            .declined
            .iter()
            .all(|l| l.kind == LossKind::InvalidName),
        "{report:?}"
    );
}

#[cfg(unix)]
#[test]
fn names_windows_cannot_create_are_ordinary_on_unix() {
    let temp = tempdir().unwrap();
    let archive = wrap_to_file(&windows_hostile_tar(), temp.path(), "a.tar.zst");
    let dest = temp.path().join("out");
    let report = extract(&archive, &dest, &ExtractOptions::default()).unwrap();
    assert!(report.is_clean(), "{report:?}");
    for (path, content) in WINDOWS_HOSTILE {
        assert_eq!(&fs::read(dest.join(path)).unwrap(), content, "{path}");
    }
}
