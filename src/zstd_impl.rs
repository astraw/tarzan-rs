#[cfg(all(feature = "zstd-sys", feature = "pure-rust"))]
compile_error!("features `zstd-sys` and `pure-rust` are mutually exclusive");

#[cfg(not(any(feature = "zstd-sys", feature = "pure-rust")))]
compile_error!("one of features `zstd-sys` or `pure-rust` must be enabled");

#[cfg(feature = "zstd-sys")]
pub(crate) use zstd::stream::{read::Decoder, write::Encoder};

#[cfg(feature = "pure-rust")]
pub(crate) use pure_impl::{Decoder, Encoder};

#[cfg(feature = "pure-rust")]
mod pure_impl {
    use std::io::{self, Read, Write};
    use zstd_pure_rs::prelude::*;

    pub(crate) struct Encoder<W: Write> {
        writer: W,
        cctx: Box<ZSTD_CCtx>,
        buf: Vec<u8>,
    }

    impl<W: Write> Encoder<W> {
        pub(crate) fn new(writer: W, level: i32) -> io::Result<Self> {
            let mut cctx = ZSTD_createCCtx()
                .ok_or_else(|| io::Error::other("failed to allocate zstd CCtx"))?;
            ZSTD_initCStream(&mut cctx, level);
            let buf = vec![0u8; ZSTD_CStreamOutSize()];
            Ok(Self { writer, cctx, buf })
        }

        pub(crate) fn include_checksum(&mut self, include: bool) -> io::Result<&mut Self> {
            let rc = ZSTD_CCtx_setParameter(
                &mut self.cctx,
                ZSTD_cParameter::ZSTD_c_checksumFlag,
                i32::from(include),
            );
            if ERR_isError(rc) {
                return Err(io::Error::other(format!(
                    "failed to set zstd checksum flag: {}",
                    ZSTD_getErrorName(rc)
                )));
            }
            Ok(self)
        }

        pub(crate) fn finish(mut self) -> io::Result<W> {
            loop {
                let mut pos = 0usize;
                let rc = ZSTD_endStream(&mut self.cctx, &mut self.buf, &mut pos);
                if pos > 0 {
                    self.writer.write_all(&self.buf[..pos])?;
                }
                if ERR_isError(rc) {
                    return Err(io::Error::other(format!(
                        "zstd endStream error: {}",
                        ZSTD_getErrorName(rc)
                    )));
                }
                if rc == 0 {
                    break;
                }
            }
            Ok(self.writer)
        }
    }

    impl<W: Write> Write for Encoder<W> {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            let mut src_pos = 0usize;
            while src_pos < data.len() {
                let mut dst_pos = 0usize;
                let rc = ZSTD_compressStream(
                    &mut self.cctx,
                    &mut self.buf,
                    &mut dst_pos,
                    data,
                    &mut src_pos,
                );
                if ERR_isError(rc) {
                    return Err(io::Error::other(format!(
                        "zstd compressStream error: {}",
                        ZSTD_getErrorName(rc)
                    )));
                }
                if dst_pos > 0 {
                    self.writer.write_all(&self.buf[..dst_pos])?;
                }
            }
            Ok(data.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.writer.flush()
        }
    }

    pub(crate) struct Decoder<R: Read> {
        reader: R,
        dctx: Box<ZSTD_DStream>,
        in_buf: Vec<u8>,
        in_start: usize,
        in_end: usize,
        out_buf: Vec<u8>,
        out_start: usize,
        out_end: usize,
        // When rc > 0 and out_pos > 0, the decompressor has more internal
        // state to drain; pass empty input on the next call to continue
        // draining rather than reading fresh compressed bytes from the inner
        // reader.  This flag must NOT be set when rc > 0 but out_pos == 0,
        // which means the decompressor needs more input — setting it in that
        // case would cause an infinite loop of empty-input calls that produce
        // no output.
        dctx_has_pending: bool,
    }

    impl<R: Read> Decoder<R> {
        pub(crate) fn new(reader: R) -> io::Result<Self> {
            let dctx = ZSTD_createDCtx();
            Ok(Self {
                reader,
                dctx,
                in_buf: vec![0u8; ZSTD_DStreamInSize()],
                in_start: 0,
                in_end: 0,
                out_buf: vec![0u8; ZSTD_DStreamOutSize()],
                out_start: 0,
                out_end: 0,
                dctx_has_pending: false,
            })
        }
    }

    impl<R: Read> Read for Decoder<R> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if buf.is_empty() {
                return Ok(0);
            }
            loop {
                // Serve buffered decoded output first.
                if self.out_start < self.out_end {
                    let n = (self.out_end - self.out_start).min(buf.len());
                    buf[..n].copy_from_slice(&self.out_buf[self.out_start..self.out_start + n]);
                    self.out_start += n;
                    return Ok(n);
                }
                // Determine the input slice for this round:
                // - If dctx_has_pending, pass empty input to drain internal state.
                // - If in_buf has unconsumed bytes from a previous read, use those.
                // - Otherwise read fresh compressed bytes from the inner reader.
                let (in_s, in_e) = if self.dctx_has_pending {
                    (self.in_start, self.in_start)
                } else if self.in_start < self.in_end {
                    (self.in_start, self.in_end)
                } else {
                    let n = self.reader.read(&mut self.in_buf)?;
                    if n == 0 {
                        return Ok(0);
                    }
                    self.in_start = 0;
                    self.in_end = n;
                    (0, n)
                };
                let mut in_pos = 0usize;
                let mut out_pos = 0usize;
                let rc = ZSTD_decompressStream(
                    &mut self.dctx,
                    &mut self.out_buf,
                    &mut out_pos,
                    &self.in_buf[in_s..in_e],
                    &mut in_pos,
                );
                if ERR_isError(rc) {
                    return Err(io::Error::other(format!(
                        "zstd decompressStream error: {}",
                        ZSTD_getErrorName(rc)
                    )));
                }
                self.in_start += in_pos;
                // Only arm the drain flag when this call actually produced output
                // AND rc > 0, meaning there is more internal state to drain.
                // When rc > 0 but out_pos == 0, the decompressor needs more
                // compressed input — not another empty-input call.
                self.dctx_has_pending = rc != 0 && out_pos > 0;
                self.out_start = 0;
                self.out_end = out_pos;
            }
        }
    }
}
