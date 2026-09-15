//! Deterministic tar builders shared by the integration tests.
//!
//! Everything here produces bytes in memory so tests never depend on the
//! host `tar`, its version, or what the host filesystem attaches to files.

#![allow(dead_code)]

use std::io::Cursor;
use std::path::{Path, PathBuf};

/// One entry to place in a tar built by [`build_tar`].
pub enum Entry<'a> {
    File {
        path: &'a str,
        mode: u32,
        content: &'a [u8],
    },
    Dir {
        path: &'a str,
        mode: u32,
    },
    Symlink {
        path: &'a str,
        target: &'a str,
    },
    HardLink {
        path: &'a str,
        target: &'a str,
    },
    CharDevice {
        path: &'a str,
    },
    Fifo {
        path: &'a str,
    },
    /// A PAX `x` header carrying `records`, applying to the next entry.
    Pax {
        records: &'a [(&'a str, &'a [u8])],
    },
    /// A header-only entry with an arbitrary type byte tarzan does not
    /// materialise (e.g. `b'M'`, GNU multi-volume continuation).
    Other {
        path: &'a str,
        type_byte: u8,
    },
    /// A regular file whose name field holds raw bytes (need not be UTF-8).
    /// Limited to 100 bytes, the ustar name field.
    RawNameFile {
        name: &'a [u8],
        content: &'a [u8],
    },
    /// A regular file whose in-header mtime is written in GNU base-256
    /// ("binary") form, as GNU tar and bsdtar do for values that do not fit
    /// the octal field, notably pre-1970 timestamps.
    Base256MtimeFile {
        path: &'a str,
        mtime: i64,
        content: &'a [u8],
    },
}

/// Encodes `value` into a 12-byte GNU base-256 numeric field: leading byte
/// 0xFF for negative (0x80 for positive) followed by the two's-complement
/// big-endian magnitude.
pub fn base256_field(value: i64) -> [u8; 12] {
    let mut field = [if value < 0 { 0xFF } else { 0x80 }; 12];
    let bytes = value.to_be_bytes();
    field[4..].copy_from_slice(&bytes);
    field
}

fn header(path: &str, mode: u32, size: u64, ty: tar::EntryType) -> tar::Header {
    let mut h = tar::Header::new_ustar();
    // Write the name field as bytes rather than through `set_path`: the tar
    // crate interprets the string as a host `Path`, and on Windows a name
    // like `a:b.txt` parses as a drive prefix and is rejected. The corpus
    // deliberately contains such names. Paths over the ustar limit are set
    // later by `append_data`, which emits a GNU long-name record; keep the
    // header valid with a placeholder until then.
    if path.len() <= 100 {
        let block = h.as_mut_bytes();
        block[..100].fill(0);
        block[..path.len()].copy_from_slice(path.as_bytes());
    } else {
        h.set_path("long-name-placeholder").unwrap();
    }
    h.set_mode(mode);
    h.set_uid(0);
    h.set_gid(0);
    h.set_mtime(1_700_000_000);
    h.set_size(size);
    h.set_entry_type(ty);
    h
}

/// Encodes one PAX record: `"<len> <key>=<value>\n"` with a self-consistent
/// length prefix. Values may be arbitrary bytes.
pub fn pax_record(key: &str, value: &[u8]) -> Vec<u8> {
    let body_len = 1 + key.len() + 1 + value.len() + 1; // ' ' key '=' value '\n'
    let mut len_digits = 1usize;
    let total = loop {
        let total = len_digits + body_len;
        if total.to_string().len() == len_digits {
            break total;
        }
        len_digits += 1;
    };
    let mut out = format!("{total} {key}=").into_bytes();
    out.extend_from_slice(value);
    out.push(b'\n');
    out
}

/// Builds a complete tar (with end-of-archive blocks) from `entries`.
pub fn build_tar(entries: &[Entry<'_>]) -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    for e in entries {
        match e {
            Entry::File {
                path,
                mode,
                content,
            } => {
                let mut h = header(path, *mode, content.len() as u64, tar::EntryType::Regular);
                if path.len() <= 100 {
                    h.set_cksum();
                    b.append(&h, Cursor::new(content)).unwrap();
                } else {
                    b.append_data(&mut h, path, Cursor::new(content)).unwrap();
                }
            }
            Entry::Dir { path, mode } => {
                let mut h = header(path, *mode, 0, tar::EntryType::Directory);
                h.set_cksum();
                b.append(&h, Cursor::new(&[][..])).unwrap();
            }
            Entry::Symlink { path, target } => {
                let mut h = header(path, 0o777, 0, tar::EntryType::Symlink);
                h.set_link_name(target).unwrap();
                h.set_cksum();
                b.append(&h, Cursor::new(&[][..])).unwrap();
            }
            Entry::HardLink { path, target } => {
                let mut h = header(path, 0o644, 0, tar::EntryType::Link);
                h.set_link_name(target).unwrap();
                h.set_cksum();
                b.append(&h, Cursor::new(&[][..])).unwrap();
            }
            Entry::CharDevice { path } => {
                let mut h = header(path, 0o666, 0, tar::EntryType::Char);
                h.set_device_major(1).unwrap();
                h.set_device_minor(3).unwrap();
                h.set_cksum();
                b.append(&h, Cursor::new(&[][..])).unwrap();
            }
            Entry::Fifo { path } => {
                let mut h = header(path, 0o644, 0, tar::EntryType::Fifo);
                h.set_cksum();
                b.append(&h, Cursor::new(&[][..])).unwrap();
            }
            Entry::Other { path, type_byte } => {
                let mut h = header(path, 0o644, 0, tar::EntryType::new(*type_byte));
                h.set_cksum();
                b.append(&h, Cursor::new(&[][..])).unwrap();
            }
            Entry::RawNameFile { name, content } => {
                assert!(name.len() <= 100, "raw name must fit the ustar field");
                let mut h = header(
                    "placeholder",
                    0o644,
                    content.len() as u64,
                    tar::EntryType::Regular,
                );
                {
                    let block = h.as_mut_bytes();
                    block[..100].fill(0);
                    block[..name.len()].copy_from_slice(name);
                }
                h.set_cksum();
                b.append(&h, Cursor::new(content)).unwrap();
            }
            Entry::Base256MtimeFile {
                path,
                mtime,
                content,
            } => {
                let mut h = header(path, 0o644, content.len() as u64, tar::EntryType::Regular);
                h.as_mut_bytes()[136..148].copy_from_slice(&base256_field(*mtime));
                h.set_cksum();
                b.append(&h, Cursor::new(content)).unwrap();
            }
            Entry::Pax { records } => {
                let mut data = Vec::new();
                for (k, v) in *records {
                    data.extend_from_slice(&pax_record(k, v));
                }
                let mut h = header(
                    "PaxHeader/entry",
                    0o644,
                    data.len() as u64,
                    tar::EntryType::XHeader,
                );
                h.set_cksum();
                b.append(&h, Cursor::new(data)).unwrap();
            }
        }
    }
    b.into_inner().unwrap()
}

/// Wraps raw tar bytes into an in-memory tarzan archive with default options.
pub fn wrap(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    tarzan::wrap(Cursor::new(raw), &mut out, tarzan::WrapOptions::default())
        .expect("wrap should succeed");
    out
}

/// Wraps `raw` and writes the archive to `dir/name`, returning its path.
pub fn wrap_to_file(raw: &[u8], dir: &Path, name: &str) -> PathBuf {
    let archive = dir.join(name);
    std::fs::write(&archive, wrap(raw)).unwrap();
    archive
}

pub fn tarzan_bin() -> PathBuf {
    PathBuf::from(std::env::var("CARGO_BIN_EXE_tarzan").expect("missing tarzan test binary"))
}
