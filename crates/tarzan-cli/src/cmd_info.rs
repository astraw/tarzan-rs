use std::path::Path;

use anyhow::Result;
use tarzan::TarzanReader;

use crate::util::format_size;

pub fn run(archive: &Path) -> Result<()> {
    let reader = TarzanReader::open(archive)?;

    let members = reader.members();
    let member_count = members.len();
    let chunk_count: u64 = members.iter().map(|m| m.chunks.len() as u64).sum();
    let uncompressed: u64 = members
        .iter()
        .flat_map(|m| m.chunks.iter())
        .map(|c| c.uncompressed_size)
        .sum();
    let compressed: u64 = members
        .iter()
        .flat_map(|m| m.chunks.iter())
        .map(|c| c.compressed_size)
        .sum();
    let archive_size = reader.archive_size();
    let toc_offset = reader.toc_offset();
    let toc_frame_size = reader.toc_frame_size();
    let identity_version = reader.identity_version();

    let ratio = if uncompressed > 0 {
        format!("{:.1}%", 100.0 * archive_size as f64 / uncompressed as f64)
    } else {
        "n/a".to_owned()
    };
    let avg_chunk = if chunk_count > 0 {
        format_size(uncompressed / chunk_count)
    } else {
        "n/a".to_owned()
    };

    println!("Format:          tarzan v{identity_version}");
    println!("File:            {}", archive.display());
    println!("Size:            {}", format_size(archive_size));
    println!("Uncompressed:    {}", format_size(uncompressed));
    println!("Ratio:           {ratio} (archive / uncompressed)");
    println!(
        "Data frames:     {} (sum of compressed chunks)",
        format_size(compressed)
    );
    println!("Members:         {member_count}");
    println!("Chunks:          {chunk_count}");
    println!("Avg chunk size:  {avg_chunk} (uncompressed)");
    println!("Identity frame:  TRZN v{identity_version}");
    println!(
        "TOC frame:       {} at offset {}",
        format_size(toc_frame_size),
        toc_offset
    );

    Ok(())
}
