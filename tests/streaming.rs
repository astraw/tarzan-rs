use std::collections::HashSet;
use std::io::{self, Cursor, Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tempfile::tempdir;

/// Builds an in-memory tar holding several small regular files.
fn multi_file_tar(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, data) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_entry_type(tar::EntryType::Regular);
        builder
            .append_data(&mut header, name, *data)
            .expect("append file to tar");
    }
    builder.into_inner().expect("finish tar")
}

/// Distinct compressed frame offsets referenced by an archive's members.
fn distinct_frames(reader: &tarzan::TarzanReader) -> HashSet<u64> {
    reader
        .members()
        .iter()
        .flat_map(|m| m.chunks.iter())
        .map(|c| c.compressed_offset)
        .collect()
}

/// Builds an in-memory tar holding a single regular file of `size` bytes,
/// returning `(tar_bytes, file_data)`.
fn big_file_tar(name: &str, size: usize) -> (Vec<u8>, Vec<u8>) {
    let data: Vec<u8> = (0..size).map(|i| ((i * 31 + 7) % 256) as u8).collect();
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(size as u64);
    header.set_mode(0o644);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_entry_type(tar::EntryType::Regular);
    builder
        .append_data(&mut header, name, data.as_slice())
        .expect("append file to tar");
    let tar = builder.into_inner().expect("finish tar");
    (tar, data)
}

#[test]
fn large_member_is_split_into_multiple_chunks() {
    let (tar, data) = big_file_tar("big.bin", 256 * 1024);
    let opts = tarzan::WrapOptions::default().chunk_size(16 * 1024);

    let temp = tempdir().expect("tempdir");
    let archive_path = temp.path().join("archive.tar.zst");
    let out = std::fs::File::create(&archive_path).expect("create archive");
    tarzan::wrap(Cursor::new(&tar), out, opts).expect("wrap should succeed");

    let mut reader = tarzan::TarzanReader::open(&archive_path).expect("open archive");
    let member = reader
        .members()
        .iter()
        .find(|m| m.path == "big.bin")
        .expect("big.bin must be present");
    assert!(
        member.chunks.len() > 1,
        "a member larger than chunk_size should span multiple chunks, got {}",
        member.chunks.len()
    );

    // Extraction must reassemble the data across all of the member's chunks.
    let mut extracted = Vec::new();
    reader
        .extract_member("big.bin", &mut extracted)
        .expect("extract should succeed");
    assert_eq!(extracted, data, "extracted data must match the original");

    // Every chunk's recorded checksum must verify.
    for record in reader.verify_all().expect("verify should succeed") {
        assert!(
            matches!(record.status, tarzan::VerifyStatus::Ok),
            "chunk {} of {} failed verification",
            record.chunk_index,
            record.path
        );
    }
}

#[test]
fn split_archive_still_decodes_bit_for_bit() {
    let (tar, _) = big_file_tar("big.bin", 200 * 1024);
    let opts = tarzan::WrapOptions::default().chunk_size(8 * 1024);

    let mut wrapped = Vec::new();
    tarzan::wrap(Cursor::new(&tar), &mut wrapped, opts).expect("wrap should succeed");

    let decoded = zstd::stream::decode_all(Cursor::new(&wrapped)).expect("zstd decode");
    assert_eq!(
        decoded, tar,
        "concatenated chunks must reproduce the tar stream exactly"
    );
}

#[test]
fn small_members_are_packed_into_a_shared_frame() {
    let files: Vec<(String, Vec<u8>)> = (0..50)
        .map(|i| {
            (
                format!("file{i}.txt"),
                format!("contents of file {i}\n").into_bytes(),
            )
        })
        .collect();
    let refs: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(n, d)| (n.as_str(), d.as_slice()))
        .collect();
    let tar = multi_file_tar(&refs);

    let temp = tempdir().expect("tempdir");
    let archive_path = temp.path().join("archive.tar.zst");
    let out = std::fs::File::create(&archive_path).expect("create archive");
    // The default chunk size dwarfs the whole archive: all members fit in one frame.
    tarzan::wrap(Cursor::new(&tar), out, tarzan::WrapOptions::default()).expect("wrap");

    let mut reader = tarzan::TarzanReader::open(&archive_path).expect("open archive");
    assert_eq!(
        distinct_frames(&reader).len(),
        1,
        "all small members should be packed into a single shared frame"
    );

    // Every member must still extract correctly out of the shared frame.
    for (name, data) in &files {
        let mut extracted = Vec::new();
        reader
            .extract_member(name, &mut extracted)
            .expect("extract should succeed");
        assert_eq!(&extracted, data, "extracted data for {name} must match");
    }

    for record in reader.verify_all().expect("verify should succeed") {
        assert!(
            matches!(record.status, tarzan::VerifyStatus::Ok),
            "chunk of {} failed verification",
            record.path
        );
    }
}

#[test]
fn grouping_splits_into_several_frames_at_chunk_size() {
    let files: Vec<(String, Vec<u8>)> = (0..40).map(|i| (format!("f{i}"), vec![b'x'; 1000])).collect();
    let refs: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(n, d)| (n.as_str(), d.as_slice()))
        .collect();
    let tar = multi_file_tar(&refs);

    let temp = tempdir().expect("tempdir");
    let archive_path = temp.path().join("archive.tar.zst");
    let out = std::fs::File::create(&archive_path).expect("create archive");
    // Each member region is ~1.5 KiB; an 8 KiB chunk size packs a handful per frame.
    let opts = tarzan::WrapOptions::default().chunk_size(8 * 1024);
    tarzan::wrap(Cursor::new(&tar), out, opts).expect("wrap");

    let mut reader = tarzan::TarzanReader::open(&archive_path).expect("open archive");
    let frames = distinct_frames(&reader).len();
    assert!(
        frames > 1 && frames < files.len(),
        "expected several grouped frames, got {frames} for {} members",
        files.len()
    );

    for (name, data) in &files {
        let mut extracted = Vec::new();
        reader
            .extract_member(name, &mut extracted)
            .expect("extract should succeed");
        assert_eq!(&extracted, data, "extracted data for {name} must match");
    }
}

#[test]
fn reader_opens_from_a_non_file_source() {
    // Wrap entirely in memory, then read it back through a non-file,
    // seekable source — the path an HTTP-range-backed reader would take.
    let (tar, data) = big_file_tar("big.bin", 100 * 1024);
    let mut wrapped = Vec::new();
    let opts = tarzan::WrapOptions::default().chunk_size(16 * 1024);
    tarzan::wrap(Cursor::new(&tar), &mut wrapped, opts).expect("wrap should succeed");

    let mut reader = tarzan::TarzanReader::from_seekable(Cursor::new(wrapped))
        .expect("from_seekable should open an in-memory archive");
    assert!(reader.members().iter().any(|m| m.path == "big.bin"));

    let mut extracted = Vec::new();
    reader
        .extract_member("big.bin", &mut extracted)
        .expect("extract should succeed");
    assert_eq!(extracted, data, "extracted data must match the original");

    for record in reader.verify_all().expect("verify should succeed") {
        assert!(matches!(record.status, tarzan::VerifyStatus::Ok));
    }
}

/// A reader that records the running total of bytes it has served.
struct CountingReader {
    data: Vec<u8>,
    pos: usize,
    counter: Arc<AtomicU64>,
}

impl Read for CountingReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut src = &self.data[self.pos..];
        let n = src.read(buf)?;
        self.pos += n;
        self.counter.fetch_add(n as u64, Ordering::SeqCst);
        Ok(n)
    }
}

/// A writer that snapshots how much input had been read at the moment the
/// first compressed data (beyond the small identity frame) was written.
struct ProbeWriter {
    counter: Arc<AtomicU64>,
    written: u64,
    input_read_at_first_data: Option<u64>,
}

impl Write for ProbeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.written += buf.len() as u64;
        // The 14-byte identity frame is written before any input is read, so
        // only snapshot once output has clearly moved on to compressed data.
        if self.input_read_at_first_data.is_none() && self.written > 64 {
            self.input_read_at_first_data = Some(self.counter.load(Ordering::SeqCst));
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn wrap_streams_without_buffering_whole_input() {
    let (tar, _) = big_file_tar("big.bin", 2 * 1024 * 1024);
    let total = tar.len() as u64;
    let counter = Arc::new(AtomicU64::new(0));

    let reader = CountingReader {
        data: tar,
        pos: 0,
        counter: Arc::clone(&counter),
    };
    let mut writer = ProbeWriter {
        counter: Arc::clone(&counter),
        written: 0,
        input_read_at_first_data: None,
    };

    let opts = tarzan::WrapOptions::default().chunk_size(16 * 1024);
    tarzan::wrap(reader, &mut writer, opts).expect("wrap should succeed");

    let read_so_far = writer
        .input_read_at_first_data
        .expect("wrap should have emitted compressed data");
    assert!(
        read_so_far < total / 4,
        "wrap read {read_so_far} of {total} bytes before emitting any data — not streaming"
    );
}
