use std::path::Path;

use anyhow::Result;
use tarzan::{ExtractOptions, StrictFidelityError, TarzanReader};

pub fn run(archive: &Path, dest: &Path, opts: ExtractOptions, verbose: bool) -> Result<()> {
    let mut reader = TarzanReader::open(archive)?;
    let report = reader.extract_to_dir(dest, &opts, |path| {
        if verbose {
            eprintln!("{path}");
        }
    });
    match report {
        Ok(report) => {
            // Silent when everything was restored, like `verify`. Otherwise
            // one block on stderr; `-v` lists every affected path instead of
            // the first few per kind.
            if !report.is_clean() {
                let text = if verbose {
                    report.summary_full()
                } else {
                    report.summary()
                };
                eprintln!("{text}");
            }
            Ok(())
        }
        Err(err) if err.is::<StrictFidelityError>() && verbose => {
            // Same report, but the full listing the user asked for.
            let report = err
                .downcast::<StrictFidelityError>()
                .expect("checked above");
            eprintln!("{}", report.0.summary_full());
            Err(StrictFidelityError(report.0).into())
        }
        Err(err) => Err(err),
    }
}
