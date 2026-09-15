//! The reading layer (`list --json`, `info --json`) is a pure function of the
//! archive bytes and must produce identical output on every platform.
//!
//! Expected output for each compat fixture is committed under
//! `testdata/compat/golden/`. Regenerate after an intentional change with
//!
//! ```sh
//! UPDATE_GOLDEN=1 cargo test --test golden_toc
//! ```
//!
//! `info --json` is compared without its `file` field, which is the path the
//! test happened to pass.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

const FIXTURES: &[&str] = &["0.2.0", "0.2.1", "0.2.2", "0.3.0", "0.4.0"];

fn compat_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/compat")
}

fn tarzan_bin() -> PathBuf {
    PathBuf::from(std::env::var("CARGO_BIN_EXE_tarzan").expect("missing tarzan test binary"))
}

fn run_json(args: &[&str], archive: &Path) -> Value {
    let out = Command::new(tarzan_bin())
        .args(args)
        .arg("-f")
        .arg(archive)
        .output()
        .expect("failed to spawn tarzan");
    assert!(
        out.status.success(),
        "{args:?} failed on {}: {}",
        archive.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("output must be JSON")
}

fn canonical(value: &Value) -> String {
    // Pretty-print through serde_json so the golden files are readable and
    // the comparison is insensitive to incidental whitespace and key order
    // (serde_json sorts object keys); every key and value still has to match.
    let mut text = serde_json::to_string_pretty(value).unwrap();
    text.push('\n');
    text
}

fn check_or_update(golden: &Path, actual: &str, what: &str) {
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        fs::create_dir_all(golden.parent().unwrap()).unwrap();
        fs::write(golden, actual).unwrap();
        return;
    }
    let expected = fs::read_to_string(golden).unwrap_or_else(|e| {
        panic!(
            "missing golden {} for {what}: {e}\nrun: UPDATE_GOLDEN=1 cargo test --test golden_toc",
            golden.display()
        )
    });
    assert!(
        expected == actual,
        "{what} differs from golden {}\n--- expected\n{expected}\n--- actual\n{actual}\n\
         If the change is intentional: UPDATE_GOLDEN=1 cargo test --test golden_toc",
        golden.display()
    );
}

#[test]
fn list_json_matches_golden_on_every_platform() {
    for version in FIXTURES {
        let archive = compat_dir().join(format!("tarzan-v{version}.tar.zst"));
        let actual = canonical(&run_json(&["list", "--json"], &archive));
        let golden = compat_dir().join(format!("golden/tarzan-v{version}.list.json"));
        check_or_update(&golden, &actual, &format!("list --json of v{version}"));
    }
}

#[test]
fn info_json_matches_golden_on_every_platform() {
    for version in FIXTURES {
        let archive = compat_dir().join(format!("tarzan-v{version}.tar.zst"));
        let mut info = run_json(&["info", "--json"], &archive);
        let removed = info
            .as_object_mut()
            .expect("info --json is an object")
            .remove("file");
        assert!(removed.is_some(), "info --json should carry a `file` field");
        let actual = canonical(&info);
        let golden = compat_dir().join(format!("golden/tarzan-v{version}.info.json"));
        check_or_update(&golden, &actual, &format!("info --json of v{version}"));
    }
}
