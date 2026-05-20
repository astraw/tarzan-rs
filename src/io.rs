use std::io::{self, Read, Write};

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
