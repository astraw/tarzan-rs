//! End-to-end coverage against the *host* bsdtar on macOS, run with its
//! default flags.
//!
//! Every other test that shells out to `tar` sets `COPYFILE_DISABLE=1`, which
//! switches off the macOS-specific output: AppleDouble `._*` companion
//! members, binary `SCHILY.xattr.*` / base64 `LIBARCHIVE.xattr.*` PAX
//! records, and PAX `mtime` records carrying sub-second precision. The
//! hand-built PAX fixtures in `edge_case_shapes.rs` model those shapes, but
//! nothing else exercises what the real tool actually emits today. This file
//! does, so a change in either bsdtar or tarzan shows up in CI.
#![cfg(target_os = "macos")]

use std::collections::BTreeMap;
use std::fs;
use std::io::Cursor;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;
use tempfile::{TempDir, tempdir};

const USER_NOTE: &[u8] = b"plain xattr";
// Newlines and NULs force bsdtar to use length-delimited binary PAX values.
const RESOURCE_FORK: &[u8] = b"resource fork bytes\n\x00\x01\xff";
const STAMPED_SECS: i64 = 1_700_000_000;
const STAMPED_NANOS: u32 = 123_456_789;
const UNICODE_NAME: &str = "\u{c4}rger_na\u{ef}ve_\u{65e5}\u{672c}\u{8a9e}.txt";
const LONG_COMPONENT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn tarzan_bin() -> PathBuf {
    PathBuf::from(std::env::var("CARGO_BIN_EXE_tarzan").expect("missing tarzan test binary"))
}

fn run(cmd: &mut Command) -> Vec<u8> {
    let output = cmd.output().expect("failed to spawn command");
    assert!(
        output.status.success(),
        "{cmd:?} failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

struct Fixture {
    _temp: TempDir,
    src: PathBuf,
    tar: PathBuf,
    archive: PathBuf,
    /// Relative path of the long-named file (three components, each > 100 bytes total).
    long_path: String,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempdir().expect("tempdir");
        let src = temp.path().join("src");
        fs::create_dir(&src).unwrap();

        fs::write(src.join("plain.txt"), b"hello\n").unwrap();
        xattr::set(src.join("plain.txt"), "user.note", USER_NOTE).unwrap();

        fs::write(src.join("forked.bin"), b"data with a fork").unwrap();
        xattr::set(
            src.join("forked.bin"),
            "com.apple.ResourceFork",
            RESOURCE_FORK,
        )
        .unwrap();

        // bsdtar's default "restricted pax" format only emits a PAX header,
        // and with it the fractional-second mtime, when some other property
        // of the entry needs one. Give the stamped file an xattr so the
        // header is guaranteed regardless of what the host adds on its own
        // (developer Macs stamp every new file with com.apple.provenance;
        // CI runners do not).
        fs::write(src.join("stamped.txt"), b"timestamped").unwrap();
        xattr::set(
            src.join("stamped.txt"),
            "user.stamp",
            b"forces a PAX header",
        )
        .unwrap();
        filetime::set_file_mtime(
            src.join("stamped.txt"),
            filetime::FileTime::from_unix_time(STAMPED_SECS, STAMPED_NANOS),
        )
        .unwrap();

        fs::create_dir_all(src.join("dir with spaces/nested")).unwrap();
        fs::write(src.join("dir with spaces/nested/file.txt"), b"nested\n").unwrap();
        fs::create_dir(src.join("emptydir")).unwrap();
        fs::write(src.join("empty.txt"), b"").unwrap();

        // Spans several chunks once wrapped with a small --chunk-size.
        let big: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        fs::write(src.join("big.bin"), &big).unwrap();

        fs::hard_link(src.join("plain.txt"), src.join("hardlink.txt")).unwrap();
        std::os::unix::fs::symlink("plain.txt", src.join("symlink.txt")).unwrap();

        let long_path = format!("{LONG_COMPONENT}/{LONG_COMPONENT}/deep_{LONG_COMPONENT}.txt");
        fs::create_dir_all(src.join(&long_path).parent().unwrap()).unwrap();
        fs::write(src.join(&long_path), b"deep\n").unwrap();
        std::os::unix::fs::symlink(&long_path, src.join("longlink")).unwrap();

        fs::write(src.join(UNICODE_NAME), b"umlaut\n").unwrap();

        let tar = temp.path().join("bsdtar-default.tar");
        // Deliberately *no* COPYFILE_DISABLE: we want bsdtar's real default output.
        run(Command::new("tar")
            .env_remove("COPYFILE_DISABLE")
            .arg("-cf")
            .arg(&tar)
            .arg("-C")
            .arg(&src)
            .arg("."));

        let archive = temp.path().join("bsdtar-default.tar.zst");
        run(Command::new(tarzan_bin())
            .arg("wrap")
            .arg(&tar)
            .args(["--chunk-size", "64K", "-f"])
            .arg(&archive));

        Self {
            _temp: temp,
            src,
            tar,
            archive,
            long_path,
        }
    }

    fn tarzan(&self, args: &[&str]) -> Vec<u8> {
        run(Command::new(tarzan_bin())
            .args(args)
            .arg("-f")
            .arg(&self.archive))
    }

    fn list(&self) -> Vec<String> {
        String::from_utf8(self.tarzan(&["list"]))
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn toc_json(&self) -> BTreeMap<String, Value> {
        let members: Vec<Value> =
            serde_json::from_slice(&self.tarzan(&["list", "--json"])).unwrap();
        members
            .into_iter()
            .map(|m| (m["path"].as_str().unwrap().to_owned(), m))
            .collect()
    }

    fn extract(&self, extra: &[&str]) -> PathBuf {
        let dest = tempfile::Builder::new()
            .prefix("out")
            .tempdir_in(self._temp.path())
            .unwrap()
            .keep();
        run(Command::new(tarzan_bin())
            .args(["extract", "-f"])
            .arg(&self.archive)
            .arg("-C")
            .arg(&dest)
            .args(extra));
        dest
    }
}

fn is_apple_double(path: &str) -> bool {
    Path::new(path)
        .file_name()
        .is_some_and(|name| name.to_string_lossy().starts_with("._"))
}

fn xattr_bytes(path: &Path, name: &str) -> Option<Vec<u8>> {
    xattr::get(path, name).unwrap()
}

#[test]
fn default_bsdtar_output_wraps_verifies_and_roundtrips_verbatim() {
    let fx = Fixture::new();

    let wrapped = fs::read(&fx.archive).unwrap();
    let decoded = zstd::stream::decode_all(Cursor::new(&wrapped)).unwrap();
    assert_eq!(
        decoded,
        fs::read(&fx.tar).unwrap(),
        "zstd -d of the archive must reproduce bsdtar's bytes exactly"
    );

    fx.tarzan(&["verify"]);
    fx.tarzan(&["verify", "--quick"]);
}

#[test]
fn default_bsdtar_output_indexes_every_member_including_appledouble() {
    let fx = Fixture::new();
    let listed = fx.list();

    // bsdtar hides `._*` companions from `tar -t`; tarzan indexes them as
    // ordinary members. Document that here so a behaviour change is loud.
    let apple_doubles: Vec<_> = listed.iter().filter(|p| is_apple_double(p)).collect();
    assert!(
        apple_doubles.iter().any(|p| p.as_str() == "./._plain.txt"),
        "expected an AppleDouble companion for plain.txt, got {apple_doubles:?}"
    );

    for expected in [
        "./plain.txt",
        "./forked.bin",
        "./stamped.txt",
        "./dir with spaces/nested/file.txt",
        "./emptydir/",
        "./empty.txt",
        "./big.bin",
        "./hardlink.txt",
        "./symlink.txt",
        "./longlink",
    ] {
        assert!(
            listed.iter().any(|p| p == expected),
            "missing {expected} in {listed:?}"
        );
    }
    let long = format!("./{}", fx.long_path);
    assert!(
        listed.contains(&long),
        "long PAX path must survive: {listed:?}"
    );
    let unicode = format!("./{UNICODE_NAME}");
    assert!(
        listed.contains(&unicode),
        "name bytes must be preserved as written (bsdtar itself renders NFD): {listed:?}"
    );

    // Every ASCII name bsdtar reports must be present with the same spelling.
    // (Non-ASCII names are skipped: bsdtar renders them in NFD, tarzan keeps
    // the archive's bytes, which is asserted separately above.)
    let bsd_list = String::from_utf8(run(Command::new("tar").arg("-tf").arg(&fx.tar))).unwrap();
    for name in bsd_list.lines().filter(|n| n.is_ascii()) {
        assert!(
            listed.iter().any(|p| p == name),
            "bsdtar lists {name}, tarzan does not"
        );
    }
}

#[test]
fn default_bsdtar_pax_records_populate_toc_metadata() {
    let fx = Fixture::new();
    let toc = fx.toc_json();

    let stamped = &toc["./stamped.txt"];
    assert_eq!(stamped["mtime"], Value::from(STAMPED_SECS));
    assert_eq!(
        stamped["mtime_ns"],
        Value::from(STAMPED_NANOS),
        "sub-second mtime comes only from the PAX mtime record"
    );

    let plain = &toc["./plain.txt"];
    let xattrs = plain["xattrs"]
        .as_object()
        .expect("plain.txt should carry xattrs");
    let note: Vec<u8> = serde_json::from_value(xattrs["user.note"].clone()).unwrap();
    assert_eq!(note, USER_NOTE);

    let forked = &toc["./forked.bin"];
    let xattrs = forked["xattrs"]
        .as_object()
        .expect("forked.bin should carry xattrs");
    let fork: Vec<u8> = serde_json::from_value(xattrs["com.apple.ResourceFork"].clone()).unwrap();
    assert_eq!(
        fork, RESOURCE_FORK,
        "binary SCHILY.xattr value with newline and NUL must round-trip"
    );

    assert_eq!(toc["./symlink.txt"]["type"], "symlink");
    assert_eq!(toc["./symlink.txt"]["link_target"], "plain.txt");
    assert_eq!(
        toc["./longlink"]["link_target"],
        Value::from(fx.long_path.as_str())
    );

    // bsdtar picks which of the two hard-linked names is the data member by
    // traversal order; whichever it is, the other must point at it.
    let (plain_t, hard_t) = (&toc["./plain.txt"]["type"], &toc["./hardlink.txt"]["type"]);
    match (plain_t.as_str().unwrap(), hard_t.as_str().unwrap()) {
        ("file", "hard_link") => assert_eq!(toc["./hardlink.txt"]["link_target"], "./plain.txt"),
        ("hard_link", "file") => assert_eq!(toc["./plain.txt"]["link_target"], "./hardlink.txt"),
        other => panic!("unexpected hard-link pair types {other:?}"),
    }

    let big = &toc["./big.bin"];
    assert!(
        big["chunks"].as_array().unwrap().len() > 1,
        "300 KB member must span several 64K chunks"
    );
}

#[test]
fn default_bsdtar_output_extracts_with_metadata_restored() {
    let fx = Fixture::new();
    let dest = fx.extract(&[]);

    for rel in [
        "plain.txt",
        "forked.bin",
        "stamped.txt",
        "dir with spaces/nested/file.txt",
        "empty.txt",
        "big.bin",
        UNICODE_NAME,
    ] {
        assert_eq!(
            fs::read(dest.join(rel)).unwrap(),
            fs::read(fx.src.join(rel)).unwrap(),
            "content mismatch for {rel}"
        );
    }
    assert_eq!(
        fs::read(dest.join(&fx.long_path)).unwrap(),
        b"deep\n",
        "long PAX path must extract"
    );
    assert!(dest.join("emptydir").is_dir());

    let meta = fs::metadata(dest.join("stamped.txt")).unwrap();
    assert_eq!(meta.mtime(), STAMPED_SECS);
    assert_eq!(
        meta.mtime_nsec(),
        i64::from(STAMPED_NANOS),
        "sub-second mtime must be restored"
    );

    assert_eq!(
        xattr_bytes(&dest.join("plain.txt"), "user.note").as_deref(),
        Some(USER_NOTE)
    );
    assert_eq!(
        xattr_bytes(&dest.join("forked.bin"), "com.apple.ResourceFork").as_deref(),
        Some(RESOURCE_FORK)
    );

    assert_eq!(
        fs::metadata(dest.join("plain.txt")).unwrap().ino(),
        fs::metadata(dest.join("hardlink.txt")).unwrap().ino(),
        "hard link must be reconstructed"
    );
    assert_eq!(
        fs::read_link(dest.join("symlink.txt")).unwrap(),
        Path::new("plain.txt")
    );
    assert_eq!(
        fs::read_link(dest.join("longlink")).unwrap(),
        Path::new(&fx.long_path),
        "long PAX linkpath must be restored"
    );

    // Current contract: AppleDouble companions are written out as files.
    assert!(
        dest.join("._plain.txt").is_file(),
        "AppleDouble companion is extracted as an ordinary member"
    );
}

#[test]
fn appledouble_companions_can_be_excluded_on_extract() {
    let fx = Fixture::new();
    let dest = fx.extract(&["--exclude", "._*", "--exclude", "*/._*"]);

    let mut stack = vec![dest.clone()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().into_owned();
            assert!(
                !name.starts_with("._"),
                "{} should have been excluded",
                entry.path().display()
            );
            if entry.file_type().unwrap().is_dir() {
                stack.push(entry.path());
            }
        }
    }
    assert_eq!(fs::read(dest.join("plain.txt")).unwrap(), b"hello\n");
    assert_eq!(
        xattr_bytes(&dest.join("plain.txt"), "user.note").as_deref(),
        Some(USER_NOTE),
        "xattrs come from PAX records, not from the excluded companion"
    );
}

#[test]
fn cat_streams_regular_members_from_default_bsdtar_output() {
    let fx = Fixture::new();
    assert_eq!(fx.tarzan(&["cat", "./stamped.txt"]), b"timestamped");
    assert_eq!(
        fx.tarzan(&["cat", &format!("./{}", fx.long_path)]),
        b"deep\n"
    );
    assert_eq!(
        fx.tarzan(&["cat", "./big.bin"]),
        fs::read(fx.src.join("big.bin")).unwrap(),
        "multi-chunk member must stream intact"
    );
}
