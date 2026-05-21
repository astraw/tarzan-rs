use std::io;
use std::path::Path;

use anyhow::Result;
use tarzan::TarzanReader;

pub fn run(archive: &Path, target_path: &str) -> Result<()> {
    let mut reader = TarzanReader::open(archive)?;
    let mut stdout = io::stdout().lock();
    reader.extract_member(target_path, &mut stdout)?;
    Ok(())
}
