use std::path::Path;

use anyhow::{Result, bail};
use tarzan::{TarzanReader, VerifyStatus};

pub fn run(archive: &Path, target_path: Option<&str>, verbose: bool) -> Result<()> {
    let reader = TarzanReader::open(archive)?;

    let records = match target_path {
        Some(path) => reader.verify_member(path)?,
        None => reader.verify_all()?,
    };

    let mut any_checksum = false;
    let mut any_failure = false;

    for record in &records {
        match &record.status {
            VerifyStatus::Ok => {
                any_checksum = true;
                if verbose {
                    println!("OK  {}", record.path);
                }
            }
            VerifyStatus::Mismatch { expected, actual } => {
                any_checksum = true;
                any_failure = true;
                eprintln!(
                    "FAIL  {} (chunk {}): expected {}, got {}",
                    record.path, record.chunk_index, expected, actual
                );
            }
            VerifyStatus::NoChecksum => {}
        }
    }

    if !any_checksum {
        eprintln!(
            "warning: no checksums found in archive (archive may have been created without SHA-256 support)"
        );
    }

    if any_failure {
        bail!("checksum verification failed");
    }

    Ok(())
}
