use anyhow::{Result, bail};

use super::{FRAME_TYPE_FOOTER, encode_skippable_frame, identity::IDENTITY_MAGIC};

/// Footer version embedded in the footer payload. Independent of the identity
/// frame's version so the footer schema can evolve without re-versioning the
/// whole format (and vice versa).
pub const FOOTER_VERSION_V1: u8 = 1;

/// Seed mixed into the whole-archive XXHash64. Non-zero so a stream of all
/// zeros doesn't hash to zero, and chosen as ASCII `"TRZN2HSH"` so a
/// constants dump is human-recognisable.
pub const ARCHIVE_HASH_SEED: u64 = u64::from_le_bytes(*b"TRZN2HSH");

/// Total on-disk size of the footer in bytes — 8-byte skippable-frame header
/// plus a 30-byte payload. Fixed so the reader can always read the last N
/// bytes of the file to find it.
pub const FOOTER_FRAME_SIZE: u64 = 38;

const FOOTER_PAYLOAD_SIZE: usize = 30;

/// Parsed contents of the footer frame.
#[derive(Debug, Clone, Copy)]
pub struct Footer {
    pub toc_offset: u64,
    pub toc_frame_size: u64,
    /// XXHash64 of bytes 0..(file_size - 38), seeded with [`ARCHIVE_HASH_SEED`].
    /// Cheap end-to-end integrity check; not a cryptographic hash.
    pub archive_xxhash64: u64,
}

/// Encodes a footer skippable frame ready to append to an archive.
///
/// Layout (38 bytes total):
/// - 4 bytes: skippable-frame magic
/// - 4 bytes: payload length (`30`, little-endian u32)
/// - 4 bytes: `"TRZN"`
/// - 1 byte:  `FRAME_TYPE_FOOTER` (`0x03`)
/// - 1 byte:  `FOOTER_VERSION_V1` (`0x01`)
/// - 8 bytes: TOC frame offset from start of file (little-endian u64)
/// - 8 bytes: TOC frame total size including its 8-byte header (little-endian u64)
/// - 8 bytes: XXHash64 of bytes 0..(file_size - 38), seeded with
///   [`ARCHIVE_HASH_SEED`] (little-endian u64).
pub fn encode_footer_frame(footer: &Footer) -> Vec<u8> {
    let mut payload = Vec::with_capacity(FOOTER_PAYLOAD_SIZE);
    payload.extend_from_slice(IDENTITY_MAGIC.as_slice());
    payload.push(FRAME_TYPE_FOOTER);
    payload.push(FOOTER_VERSION_V1);
    payload.extend_from_slice(&footer.toc_offset.to_le_bytes());
    payload.extend_from_slice(&footer.toc_frame_size.to_le_bytes());
    payload.extend_from_slice(&footer.archive_xxhash64.to_le_bytes());
    debug_assert_eq!(payload.len(), FOOTER_PAYLOAD_SIZE);
    encode_skippable_frame(&payload)
}

/// Decodes a footer payload (the 30 bytes after the 8-byte skippable-frame header).
pub fn decode_footer_payload(payload: &[u8]) -> Result<Footer> {
    if payload.len() != FOOTER_PAYLOAD_SIZE {
        bail!(
            "footer payload has wrong size: {} bytes (expected {FOOTER_PAYLOAD_SIZE})",
            payload.len()
        );
    }
    if payload[0..4] != IDENTITY_MAGIC {
        bail!("footer payload does not begin with TRZN");
    }
    if payload[4] != FRAME_TYPE_FOOTER {
        bail!(
            "unexpected frame type in footer payload: {:#04x}",
            payload[4]
        );
    }
    let version = payload[5];
    if version != FOOTER_VERSION_V1 {
        bail!("unsupported footer version: {version}");
    }
    let toc_offset = u64::from_le_bytes(payload[6..14].try_into().unwrap());
    let toc_frame_size = u64::from_le_bytes(payload[14..22].try_into().unwrap());
    let archive_xxhash64 = u64::from_le_bytes(payload[22..30].try_into().unwrap());
    Ok(Footer {
        toc_offset,
        toc_frame_size,
        archive_xxhash64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::SKIPPABLE_FRAME_MAGIC;

    #[test]
    fn footer_frame_size_matches_layout() {
        let f = Footer {
            toc_offset: 0,
            toc_frame_size: 0,
            archive_xxhash64: 0,
        };
        let bytes = encode_footer_frame(&f);
        assert_eq!(bytes.len() as u64, FOOTER_FRAME_SIZE);
        assert_eq!(&bytes[0..4], SKIPPABLE_FRAME_MAGIC.to_le_bytes().as_slice());
        assert_eq!(
            &bytes[4..8],
            (FOOTER_PAYLOAD_SIZE as u32).to_le_bytes().as_slice()
        );
    }

    #[test]
    fn encode_then_decode_roundtrips() {
        let f = Footer {
            toc_offset: 0x1234_5678_9abc_def0,
            toc_frame_size: 0x0fed_cba9_8765_4321,
            archive_xxhash64: 0xdead_beef_cafe_f00d,
        };
        let bytes = encode_footer_frame(&f);
        let decoded = decode_footer_payload(&bytes[8..]).expect("decode");
        assert_eq!(decoded.toc_offset, f.toc_offset);
        assert_eq!(decoded.toc_frame_size, f.toc_frame_size);
        assert_eq!(decoded.archive_xxhash64, f.archive_xxhash64);
    }

    #[test]
    fn decode_rejects_wrong_frame_type() {
        let mut payload = vec![0u8; FOOTER_PAYLOAD_SIZE];
        payload[0..4].copy_from_slice(IDENTITY_MAGIC.as_slice());
        payload[4] = 0x99; // not FRAME_TYPE_FOOTER
        payload[5] = FOOTER_VERSION_V1;
        let err = decode_footer_payload(&payload).expect_err("should reject");
        assert!(format!("{err:#}").contains("unexpected frame type"));
    }

    #[test]
    fn decode_rejects_wrong_size() {
        let payload = vec![0u8; FOOTER_PAYLOAD_SIZE - 1];
        let err = decode_footer_payload(&payload).expect_err("should reject");
        assert!(format!("{err:#}").contains("wrong size"));
    }
}
