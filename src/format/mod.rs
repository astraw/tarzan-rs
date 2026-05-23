pub mod footer;
pub mod identity;
pub mod toc;

pub use identity::SKIPPABLE_FRAME_MAGIC;

/// Frame type byte embedded in every tarzan skippable-frame payload after `TRZN`.
pub const FRAME_TYPE_IDENTITY: u8 = 0x01;
pub const FRAME_TYPE_TOC: u8 = 0x02;
pub const FRAME_TYPE_FOOTER: u8 = 0x03;

/// Wraps `payload` in a zstd skippable frame.
pub(crate) fn encode_skippable_frame(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + payload.len());
    out.extend_from_slice(&identity::SKIPPABLE_FRAME_MAGIC.to_le_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    out
}
