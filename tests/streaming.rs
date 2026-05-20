use std::io::{self, Cursor, Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tempfile::tempdir;

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

    let reader = tarzan::TarzanReader::open(&archive_path).expect("open archive");
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
