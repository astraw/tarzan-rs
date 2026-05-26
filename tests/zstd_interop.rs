/// Cross-backend zstd compatibility: data compressed by the C library (`zstd`
/// crate) must be decompressible by the pure-Rust backend (`zstd-pure-rs`)
/// and vice versa.  Both are unconditional dev-dependencies so this test
/// compiles regardless of which feature (`zstd-sys` / `pure-rust`) the
/// library itself is built with.
use zstd_pure_rs::prelude::*;

use std::io::Write;

const PAYLOAD: &[u8] =
    b"the quick brown fox jumps over the lazy dog -- repeated to give zstd something to compress. ";

fn payload() -> Vec<u8> {
    PAYLOAD.repeat(200)
}

fn c_stream_encode_with_content_size(src: &[u8]) -> Vec<u8> {
    let mut encoder = zstd::stream::Encoder::new(Vec::new(), 3).expect("C zstd stream encoder");
    encoder
        .set_pledged_src_size(Some(src.len() as u64))
        .expect("pledge source size");
    encoder
        .include_contentsize(true)
        .expect("include content size");
    encoder.write_all(src).expect("C zstd stream encode");
    encoder.finish().expect("finish C zstd stream")
}

#[test]
fn c_encode_pure_decode() {
    let src = payload();
    let compressed = zstd::encode_all(src.as_slice(), 3).expect("C zstd encode");

    let mut decoded = vec![0u8; src.len() + 64];
    let n = ZSTD_decompress(&mut decoded, &compressed);
    assert!(
        !ERR_isError(n),
        "pure-rust decode error: {}",
        ZSTD_getErrorName(n)
    );
    assert_eq!(&decoded[..n], src.as_slice());
}

#[test]
fn pure_encode_c_decode() {
    let src = payload();
    let mut compressed = vec![0u8; ZSTD_compressBound(src.len())];
    let n = ZSTD_compress(&mut compressed, &src, 3);
    assert!(
        !ERR_isError(n),
        "pure-rust encode error: {}",
        ZSTD_getErrorName(n)
    );
    compressed.truncate(n);

    let decoded = zstd::decode_all(compressed.as_slice()).expect("C zstd decode");
    assert_eq!(decoded, src);
}

#[test]
fn c_encode_pure_stream_decode() {
    // zstd-pure-rs 0.1.x decodes a complete frame into an internal buffer
    // before draining it through ZSTD_decompressStream. Give it a pledged
    // content size so the test covers stream-drain behavior instead of its
    // fallback allocation heuristic for unknown-size frames.
    let src = PAYLOAD.repeat(2_000);
    let compressed = c_stream_encode_with_content_size(&src);

    let mut dctx = ZSTD_createDCtx();
    let mut in_pos = 0usize;
    let mut decoded = Vec::with_capacity(src.len());

    loop {
        let previous_in_pos = in_pos;
        let mut out = vec![0u8; ZSTD_DStreamOutSize() / 4];
        let mut out_pos = 0usize;
        let rc = if in_pos < compressed.len() {
            ZSTD_decompressStream(&mut dctx, &mut out, &mut out_pos, &compressed, &mut in_pos)
        } else {
            let mut empty_in_pos = 0usize;
            ZSTD_decompressStream(&mut dctx, &mut out, &mut out_pos, &[], &mut empty_in_pos)
        };
        assert!(
            !ERR_isError(rc),
            "pure-rust stream decode error: {}",
            ZSTD_getErrorName(rc)
        );
        assert!(
            out_pos > 0 || in_pos > previous_in_pos || rc == 0,
            "pure-rust stream decode made no progress"
        );
        decoded.extend_from_slice(&out[..out_pos]);
        if rc == 0 {
            break;
        }
    }

    assert_eq!(decoded, src);
}

#[test]
fn pure_stream_encode_c_decode() {
    let src = payload();
    let mut cctx = ZSTD_createCCtx().expect("pure-rust CCtx alloc");
    ZSTD_initCStream(&mut cctx, 3);

    let mut compressed = vec![0u8; ZSTD_compressBound(src.len())];
    let mut src_pos = 0usize;
    let mut dst_pos = 0usize;
    ZSTD_compressStream(&mut cctx, &mut compressed, &mut dst_pos, &src, &mut src_pos);
    loop {
        let rc = ZSTD_endStream(&mut cctx, &mut compressed, &mut dst_pos);
        if rc == 0 || ERR_isError(rc) {
            assert!(
                !ERR_isError(rc),
                "pure-rust endStream error: {}",
                ZSTD_getErrorName(rc)
            );
            break;
        }
    }
    compressed.truncate(dst_pos);

    let decoded = zstd::decode_all(compressed.as_slice()).expect("C zstd decode");
    assert_eq!(decoded, src);
}
