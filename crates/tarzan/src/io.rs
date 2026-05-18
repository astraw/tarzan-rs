use std::io::{self, Write};

pub fn is_nonzero(value: usize) -> bool {
    value != 0
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
