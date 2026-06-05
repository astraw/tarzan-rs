use std::path::Path;

use anyhow::{Result, bail};
use tarzan::{TarzanReader, VerifyStatus};

pub fn run(archive: &Path, target_path: Option<&str>, quick: bool, verbose: bool) -> Result<()> {
    let mut reader = TarzanReader::open(archive)?;

    if quick {
        if target_path.is_some() {
            bail!("--quick verifies the whole archive and cannot be combined with a path filter");
        }
        reader.verify_archive_hash()?;
        if verbose {
            println!("OK  whole-archive hash");
        }
        return Ok(());
    }

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
                    "FAIL  {}: expected {}, got {}",
                    record.path, expected, actual
                );
            }
            VerifyStatus::NoChecksum => {}
        }
    }

    if !any_checksum {
        eprintln!(
            "warning: no SHA-256 checksums found in archive (archive may have been created with --disable-sha256)"
        );
    }

    if any_failure {
        bail!("checksum verification failed");
    }

    Ok(())
}
