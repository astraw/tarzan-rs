use std::io::{Read, Write};

use anyhow::{Result, bail};

use crate::io;

#[derive(Debug, Clone)]
pub struct WrapOptions {
    pub chunk_size: usize,
    pub level: i32,
}

impl Default for WrapOptions {
    fn default() -> Self {
        Self {
            chunk_size: 4 * 1024 * 1024,
            level: 3,
        }
    }
}

impl WrapOptions {
    pub fn chunk_size(mut self, chunk_size: usize) -> Self {
        self.chunk_size = chunk_size;
        self
    }

    pub fn level(mut self, level: i32) -> Self {
        self.level = level;
        self
    }
}

pub fn wrap<R: Read, W: Write>(mut input: R, output: W, opts: WrapOptions) -> Result<()> {
    if !io::is_nonzero(opts.chunk_size) {
        bail!("chunk size must be greater than zero");
    }

    let mut encoder = zstd::stream::write::Encoder::new(output, opts.level)?;
    std::io::copy(&mut input, &mut encoder)?;
    let _output = encoder.finish()?;
    Ok(())
}
