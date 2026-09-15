//! Committed archive fixtures must not carry facts about the machine that
//! produced them.
//!
//! A fixture is either hermetic (nothing host-specific in it) or deliberately
//! non-portable with an allowlist entry here naming the test that relies on
//! that fact. What it must never be is accidentally tied to the author's
//! laptop: a `com.apple.provenance` xattr on every member once made the
//! compat fixtures fail to extract on the Linux and macOS CI runners.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Host-specific facts a fixture member can carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Fact {
    /// Any extended attribute. Namespaces are host-specific and cannot be
    /// restored elsewhere.
    Xattrs,
    /// PAX `uname`/`gname`. Records the author's account name.
    OwnerName,
    /// Numeric owner other than 0. Records the author's uid/gid.
    NonRootOwner,
    /// Absolute path. Would also be refused by `extract`.
    AbsolutePath,
}

/// Fixtures that intentionally carry a fact, with the test that needs it.
/// Keyed by path relative to `testdata/`.
const INTENTIONAL: &[(&str, &[Fact])] = &[];

/// Fixtures the reader rejects by design (legacy format). Checked for the
/// rejection instead of being scanned.
const LEGACY: &[&str] = &["compat/tarzan-v0.1.2.tar.zst"];

fn testdata() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata")
}

fn archives_under(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            archives_under(&path, out);
        } else if path.extension().is_some_and(|e| e == "zst") {
            out.push(path);
        }
    }
}

fn facts_of(member: &tarzan::format::toc::TocMember) -> BTreeSet<Fact> {
    let mut facts = BTreeSet::new();
    if member.xattrs.as_ref().is_some_and(|x| !x.is_empty()) {
        facts.insert(Fact::Xattrs);
    }
    if member.uname.is_some() || member.gname.is_some() {
        facts.insert(Fact::OwnerName);
    }
    if member.uid != 0 || member.gid != 0 {
        facts.insert(Fact::NonRootOwner);
    }
    if member.path.starts_with('/') {
        facts.insert(Fact::AbsolutePath);
    }
    facts
}

#[test]
fn gitattributes_keeps_testdata_binary() {
    let attrs = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(".gitattributes"))
        .expect(".gitattributes must exist");
    assert!(
        attrs.lines().any(|l| l.trim() == "testdata/** -text"),
        "testdata/** must stay -text or fixtures are corrupted by autocrlf checkouts"
    );
}

#[test]
fn committed_fixtures_carry_no_unexpected_host_facts() {
    let root = testdata();
    let mut archives = Vec::new();
    archives_under(&root, &mut archives);
    assert!(!archives.is_empty(), "no fixtures found under testdata/");

    let mut problems = Vec::new();
    for archive in &archives {
        let rel = archive
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");

        if LEGACY.contains(&rel.as_str()) {
            let err = match tarzan::TarzanReader::open(archive) {
                Ok(_) => panic!("{rel} is listed as legacy but opened"),
                Err(e) => format!("{e:#}"),
            };
            assert!(
                err.contains("v1"),
                "{rel}: expected legacy rejection, got {err}"
            );
            continue;
        }

        let allowed: BTreeSet<Fact> = INTENTIONAL
            .iter()
            .find(|(p, _)| *p == rel)
            .map(|(_, f)| f.iter().copied().collect())
            .unwrap_or_default();

        let reader = tarzan::TarzanReader::open(archive)
            .unwrap_or_else(|e| panic!("{rel} must open: {e:#}"));
        for member in reader.members() {
            let unexpected: Vec<Fact> = facts_of(member).difference(&allowed).copied().collect();
            if !unexpected.is_empty() {
                problems.push(format!("  {rel}: {} -> {unexpected:?}", member.path));
            }
        }
    }

    assert!(
        problems.is_empty(),
        "fixtures carry host-specific facts:\n{}\n\
         Regenerate the source tar with owner and xattrs normalised, e.g.\n\
         bsdtar: --no-xattrs --uid 0 --gid 0 --uname '' --gname ''\n\
         GNU tar: --no-xattrs --owner=0 --group=0 --numeric-owner\n\
         or add an INTENTIONAL entry naming the test that needs the fact.",
        problems.join("\n")
    );
}
