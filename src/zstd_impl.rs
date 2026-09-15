//! The zstd codec used for every data frame and the TOC frame.
//!
//! This is a thin alias over the `zstd` crate's streaming types so the rest
//! of the crate has one import path for the codec.
pub(crate) use zstd::stream::{read::Decoder, write::Encoder};
