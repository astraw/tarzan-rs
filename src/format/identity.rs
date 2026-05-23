use anyhow::{Result, bail};

use super::{FRAME_TYPE_IDENTITY, encode_skippable_frame};

pub const IDENTITY_MAGIC: [u8; 4] = *b"TRZN";

/// Version byte of the unsupported v1 format. The reader still detects it so it
/// can emit a helpful "decompress and re-wrap" error rather than the generic
/// "unknown version" message. Since 0.1.x, nothing in the codebase produces v1
/// archives anymore.
pub const IDENTITY_VERSION_V1_LEGACY: u8 = 1;

/// Version byte of the current format. Bumped to `0x02` when the TOC offset
/// footer and whole-archive SHA-256 landed.
pub const IDENTITY_VERSION_V2: u8 = 2;

/// Skippable frame magic for tarzan. Any value in `0x184D2A50..=0x184D2A5F` is valid
/// per the zstd spec; `54` avoids `5E` (zstd seekable format) and, in little-endian
/// byte order, puts ASCII `T` at file offset 0. Other producers may legally share
/// this magic — readers must identify tarzan frames via the `TRZN` payload identifier,
/// not the magic number alone.
pub const SKIPPABLE_FRAME_MAGIC: u32 = 0x184D2A54;

/// Encodes the current identity frame: `TRZN` + frame-type byte + version byte.
pub fn identity_frame() -> Vec<u8> {
    encode_skippable_frame(
        &[
            IDENTITY_MAGIC.as_slice(),
            &[FRAME_TYPE_IDENTITY, IDENTITY_VERSION_V2],
        ]
        .concat(),
    )
}

/// Validates an identity-frame payload and returns the version byte.
pub fn decode(payload: &[u8]) -> Result<u8> {
    if payload.len() < 6 {
        bail!(
            "identity payload too short: {} bytes (expected ≥6)",
            payload.len()
        );
    }
    if payload[0..4] != IDENTITY_MAGIC {
        bail!("identity payload does not begin with TRZN");
    }
    if payload[4] != FRAME_TYPE_IDENTITY {
        bail!(
            "unexpected frame type in identity payload: {:#04x}",
            payload[4]
        );
    }
    Ok(payload[5])
}
