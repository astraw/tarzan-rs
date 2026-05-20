use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use tarzan::TarzanReader;

use crate::util::format_size;

pub fn run(archive: &Path, json: bool) -> Result<()> {
    let reader = TarzanReader::open(archive)?;

    let members = reader.members();
    let member_count = members.len();
    let uncompressed: u64 = members
        .iter()
        .flat_map(|m| m.chunks.iter())
        .map(|c| c.uncompressed_size)
        .sum();
    // Small members can share a compressed frame, so collapse chunk records to
    // distinct frames (keyed by compressed offset) before counting and summing.
    let mut frames: HashMap<u64, u64> = HashMap::new();
    for chunk in members.iter().flat_map(|m| m.chunks.iter()) {
        frames.entry(chunk.compressed_offset).or_insert(chunk.compressed_size);
    }
    let chunk_count = frames.len() as u64;
    let compressed: u64 = frames.values().sum();
    let archive_size = reader.archive_size();
    let toc_offset = reader.toc_offset();
    let toc_frame_size = reader.toc_frame_size();
    let identity_version = reader.identity_version();

    let ratio_value: Option<f64> =
        (uncompressed > 0).then(|| archive_size as f64 / uncompressed as f64);
    let avg_chunk_bytes: Option<u64> = (chunk_count > 0).then(|| uncompressed / chunk_count);

    if json {
        let obj = serde_json::json!({
            "format_version": identity_version,
            "identity_version": identity_version,
            "file": archive.display().to_string(),
            "size_bytes": archive_size,
            "uncompressed_bytes": uncompressed,
            "data_frame_bytes": compressed,
            "ratio": ratio_value,
            "members": member_count,
            "chunks": chunk_count,
            "avg_chunk_size_bytes": avg_chunk_bytes,
            "toc_offset": toc_offset,
            "toc_frame_bytes": toc_frame_size,
        });
        println!("{}", serde_json::to_string_pretty(&obj)?);
        return Ok(());
    }

    let ratio = match ratio_value {
        Some(r) => format!("{:.1}%", 100.0 * r),
        None => "n/a".to_owned(),
    };
    let avg_chunk = match avg_chunk_bytes {
        Some(b) => format_size(b),
        None => "n/a".to_owned(),
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
