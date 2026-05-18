pub const IDENTITY_MAGIC: [u8; 4] = *b"TRZN";
pub const IDENTITY_VERSION_V1: u8 = 1;

/// Skippable frame magic for tarzan. Any value in `0x184D2A50..=0x184D2A5F` is valid
/// per the zstd spec; `54` avoids `5E` (zstd seekable format) and, in little-endian
/// byte order, puts ASCII `T` at file offset 0. Other producers may legally share
/// this magic — readers must identify tarzan frames via the `TRZN` payload identifier,
/// not the magic number alone.
pub const SKIPPABLE_FRAME_MAGIC: u32 = 0x184D2A54;

pub fn identity_frame_v1() -> Vec<u8> {
    let payload = [IDENTITY_MAGIC.as_slice(), &[IDENTITY_VERSION_V1]].concat();
    let mut out = Vec::with_capacity(8 + payload.len());
    out.extend_from_slice(&SKIPPABLE_FRAME_MAGIC.to_le_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&payload);
    out
}
