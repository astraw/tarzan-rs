use std::path::Path;

use anyhow::Result;
use tarzan::{ExtractOptions, TarzanReader};

pub fn run(archive: &Path, dest: &Path, opts: ExtractOptions, verbose: bool) -> Result<()> {
    let mut reader = TarzanReader::open(archive)?;
    reader.extract_to_dir(dest, &opts, |path| {
        if verbose {
            eprintln!("{path}");
        }
    })?;
    Ok(())
}
