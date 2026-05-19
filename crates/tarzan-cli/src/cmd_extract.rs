use std::path::Path;

use anyhow::Result;
use tarzan::{ExtractOptions, TarzanReader};

pub fn run(
    archive: &Path,
    dest: &Path,
    strip_components: usize,
    excludes: Vec<String>,
    includes: Vec<String>,
    verbose: bool,
) -> Result<()> {
    let reader = TarzanReader::open(archive)?;
    let opts = ExtractOptions {
        strip_components,
        excludes,
        includes,
    };
    reader.extract_to_dir(dest, &opts, |path| {
        if verbose {
            eprintln!("{path}");
        }
    })?;
    Ok(())
}
