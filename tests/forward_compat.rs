//! Forward compatibility: what *this* reader does with an archive from a
//! hypothetical future writer.
//!
//! tarzan does not promise forward compatibility. These tests exist so that
//! the day a change breaks an old reader, it breaks knowingly: each test
//! states what an existing reader tolerates today (additive JSON fields at
//! every level) and where it stops (a `type` value it has never seen, a
//! `tarzan_version` that disagrees with the identity frame, an identity
//! version it does not understand). A future change that alters one of these
//! outcomes must update the test and, with it, the compatibility notes.
//!
//! The archives are real: a current archive is taken apart at the footer, its
//! TOC JSON edited as a generic document, and the TOC frame and footer
//! rebuilt with the crate's own encoders.

mod common;

use std::hash::Hasher;
use std::io::Cursor;

use common::{Entry, build_tar, wrap};
use serde_json::{Value, json};
use tarzan::TarzanReader;
use tarzan::format::footer::{
    ARCHIVE_HASH_SEED, FOOTER_FRAME_SIZE, Footer, decode_footer_payload, encode_footer_frame,
};
use tarzan::format::{
    FRAME_TYPE_TOC, SKIPPABLE_FRAME_MAGIC, identity::IDENTITY_MAGIC, toc::TOC_VERSION_V1,
};

fn sample_archive() -> Vec<u8> {
    wrap(&build_tar(&[
        Entry::Dir {
            path: "d/",
            mode: 0o755,
        },
        Entry::File {
            path: "d/a.txt",
            mode: 0o644,
            content: b"alpha",
        },
        Entry::File {
            path: "b.txt",
            mode: 0o644,
            content: b"beta",
        },
    ]))
}

/// Returns (prefix before the TOC frame, TOC JSON document).
fn split(archive: &[u8]) -> (Vec<u8>, Value) {
    let footer_start = archive.len() - FOOTER_FRAME_SIZE as usize;
    let footer = decode_footer_payload(&archive[footer_start + 8..]).unwrap();
    let toc_start = footer.toc_offset as usize;
    let toc_frame = &archive[toc_start..footer_start];
    let payload = &toc_frame[8..];
    assert_eq!(&payload[..4], &IDENTITY_MAGIC);
    assert_eq!(payload[4], FRAME_TYPE_TOC);
    let json_bytes = zstd::stream::decode_all(Cursor::new(&payload[6..])).unwrap();
    let toc: Value = serde_json::from_slice(&json_bytes).unwrap();
    (archive[..toc_start].to_vec(), toc)
}

/// Rebuilds a complete archive from `prefix` and an edited TOC document,
/// with a fresh footer whose hash covers the new bytes.
fn rebuild(prefix: Vec<u8>, toc: &Value, toc_version_byte: u8) -> Vec<u8> {
    let compressed =
        zstd::stream::encode_all(Cursor::new(serde_json::to_vec(toc).unwrap()), 3).unwrap();
    let mut payload = IDENTITY_MAGIC.to_vec();
    payload.extend_from_slice(&[FRAME_TYPE_TOC, toc_version_byte]);
    payload.extend_from_slice(&compressed);

    let mut out = prefix;
    let toc_offset = out.len() as u64;
    out.extend_from_slice(&SKIPPABLE_FRAME_MAGIC.to_le_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&payload);
    let toc_frame_size = out.len() as u64 - toc_offset;

    let mut hasher = twox_hash::XxHash64::with_seed(ARCHIVE_HASH_SEED);
    hasher.write(&out);
    let footer = Footer {
        toc_offset,
        toc_frame_size,
        archive_xxhash64: hasher.finish(),
    };
    out.extend_from_slice(&encode_footer_frame(&footer));
    out
}

fn open(archive: Vec<u8>) -> anyhow::Result<TarzanReader> {
    TarzanReader::from_seekable(Cursor::new(archive))
}

fn assert_fully_usable(archive: Vec<u8>) {
    let mut reader = open(archive).expect("archive must open");
    let paths: Vec<&str> = reader.members().iter().map(|m| m.path.as_str()).collect();
    assert_eq!(paths, ["d/", "d/a.txt", "b.txt"]);
    let mut out = Vec::new();
    reader.extract_member("d/a.txt", &mut out).unwrap();
    assert_eq!(out, b"alpha");
    reader
        .verify_archive_hash()
        .expect("rebuilt footer hash must verify");
    for record in reader.verify_all().unwrap() {
        assert!(
            !matches!(record.status, tarzan::VerifyStatus::Mismatch { .. }),
            "{record:?}"
        );
    }
}

#[test]
fn rebuilding_without_changes_is_a_faithful_round_trip() {
    let (prefix, toc) = split(&sample_archive());
    assert_fully_usable(rebuild(prefix, &toc, TOC_VERSION_V1));
}

/// Additive fields are the only kind of schema growth v2 has ever had, and
/// the only kind an existing reader survives. This is the behaviour that
/// makes "we only add optional fields" a meaningful discipline.
#[test]
fn unknown_fields_at_every_level_are_ignored() {
    let (prefix, mut toc) = split(&sample_archive());
    toc["future_top_level_field"] = json!({"nested": [1, 2, 3]});
    for member in toc["members"].as_array_mut().unwrap() {
        member["future_member_field"] = json!("value");
        member["future_flags"] = json!(["a", "b"]);
        if let Some(chunks) = member["chunks"].as_array_mut() {
            for chunk in chunks {
                chunk["future_chunk_field"] = json!(42);
            }
        }
    }
    assert_fully_usable(rebuild(prefix, &toc, TOC_VERSION_V1));
}

/// Known break point: `type` is a closed set. A future writer that emits a
/// new value makes the *whole* TOC undecodable for every existing reader,
/// not just that member. If this ever changes (for example by mapping
/// unknown types to `other`), flip the assertion and note it in the
/// compatibility docs.
#[test]
fn unknown_member_type_string_breaks_existing_readers_today() {
    let (prefix, mut toc) = split(&sample_archive());
    toc["members"][2]["type"] = json!("future_type");
    let err =
        open(rebuild(prefix, &toc, TOC_VERSION_V1)).expect_err("closed enum must fail to decode");
    let text = format!("{err:#}");
    assert!(text.contains("TOC"), "{text}");
}

/// The JSON `tarzan_version` mirrors the identity frame's version byte and
/// readers require the two to agree; a disagreement can only be a buggy
/// writer or corruption.
#[test]
fn tarzan_version_disagreeing_with_identity_frame_is_rejected() {
    let (prefix, mut toc) = split(&sample_archive());
    toc["tarzan_version"] = json!(3);
    let err = open(rebuild(prefix, &toc, TOC_VERSION_V1)).expect_err("mismatch must be rejected");
    let text = format!("{err:#}");
    assert!(text.contains("tarzan_version 3"), "{text}");
    assert!(text.contains("identity frame"), "{text}");
}

#[test]
fn unknown_toc_payload_version_byte_is_rejected() {
    let (prefix, toc) = split(&sample_archive());
    let err =
        open(rebuild(prefix, &toc, TOC_VERSION_V1 + 1)).expect_err("unknown TOC payload version");
    assert!(
        format!("{err:#}").contains("unsupported TOC version"),
        "{err:#}"
    );
}

#[test]
fn unknown_identity_version_is_rejected_with_the_version_named() {
    let mut archive = sample_archive();
    archive[13] = 3;
    // The footer hash covers byte 13, so recompute it too; otherwise the test
    // could pass for the wrong reason. Rebuild via split/rebuild to do so.
    let (prefix, toc) = split(&archive);
    let err = open(rebuild(prefix, &toc, TOC_VERSION_V1)).expect_err("identity v3 is unknown");
    let text = format!("{err:#}");
    assert!(
        text.contains("unsupported tarzan format version 3"),
        "{text}"
    );
    assert!(text.contains("v2"), "{text}");
}
