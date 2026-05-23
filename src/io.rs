use std::hash::Hasher;
use std::io::{self, Read, Write};

use twox_hash::XxHash64;

use crate::format::footer::ARCHIVE_HASH_SEED;

pub fn is_nonzero(value: usize) -> bool {
    value != 0
}

/// Discards exactly `n` bytes from `r`, returning an error if fewer bytes are available.
pub fn skip_exact<R: Read>(r: &mut R, mut n: u64) -> io::Result<()> {
    let mut buf = [0u8; 8192];
    while n > 0 {
        let to_read = n.min(buf.len() as u64) as usize;
        r.read_exact(&mut buf[..to_read])?;
        n -= to_read as u64;
    }
    Ok(())
}

/// Copies exactly `n` bytes from `r` to `w`, returning an error if fewer bytes are available.
pub fn copy_exact<R: Read, W: Write + ?Sized>(r: &mut R, w: &mut W, mut n: u64) -> io::Result<()> {
    let mut buf = [0u8; 65536];
    while n > 0 {
        let to_read = n.min(buf.len() as u64) as usize;
        r.read_exact(&mut buf[..to_read])?;
        w.write_all(&buf[..to_read])?;
        n -= to_read as u64;
    }
    Ok(())
}

/// Wraps a `Write` and counts total bytes written.
pub struct CountingWriter<W> {
    inner: W,
    count: u64,
}

impl<W: Write> CountingWriter<W> {
    pub fn new(inner: W) -> Self {
        Self { inner, count: 0 }
    }

    pub fn bytes_written(&self) -> u64 {
        self.count
    }
}

impl<W: Write> Write for CountingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.count += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Wraps a `Write` and feeds every successfully-written byte into an
/// XXHash64 hasher seeded with [`ARCHIVE_HASH_SEED`].
///
/// `wrap` runs the archive prefix (identity + data frames + TOC) through this
/// writer, then [`finish`](Self::finish) gives back the inner writer and the
/// hash so the footer can be appended outside the hashed region. XXHash64 is
/// ~10 GB/s — effectively free compared to the surrounding zstd compression —
/// and gives `tarzan verify --quick` an O(n) sequential-read end-to-end check
/// without any decompression.
pub struct HashingWriter<W> {
    inner: W,
    hasher: XxHash64,
}

impl<W: Write> HashingWriter<W> {
    pub fn new(inner: W) -> Self {
        Self {
            inner,
            hasher: XxHash64::with_seed(ARCHIVE_HASH_SEED),
        }
    }

    /// Consumes the writer and returns the inner writer plus the final hash.
    pub fn finish(self) -> (W, u64) {
        (self.inner, self.hasher.finish())
    }
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.write(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
