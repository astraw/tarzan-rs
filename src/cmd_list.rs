use std::path::Path;

use anyhow::Result;
use tarzan::TarzanReader;
use tarzan::filter::PathFilter;
use tarzan::format::toc::{EntryType, TocMember};

use crate::util::format_size;

pub fn run(archive: &Path, verbose: bool, json: bool, utc: bool, paths: &[String]) -> Result<()> {
    let reader = TarzanReader::open(archive)?;
    let filter = PathFilter::new(paths)?;

    let lossy_count = reader
        .members()
        .iter()
        .filter(|m| m.path_bytes.is_some() || m.link_target_bytes.is_some())
        .count();

    let members: Vec<&TocMember> = reader
        .members()
        .iter()
        .filter(|m| filter.matches(&m.path))
        .collect();

    if lossy_count > 0 {
        eprintln!(
            "tarzan: warning: {} contains {lossy_count} entries with non-UTF-8 path/link bytes; displayed names are lossy",
            archive.display()
        );
    }

    if json {
        let out = serde_json::to_string_pretty(&members)?;
        println!("{out}");
        return Ok(());
    }

    for member in members {
        if verbose {
            let type_char = type_char_for(&member.entry_type);
            let mode = format_mode(type_char, member.mode);
            let owner = format!("{}/{}", member.uid, member.gid);
            let size = format_size(member.size);
            let mtime = format_mtime(member.mtime, utc);
            let path = format_path_with_link(member);
            println!("{mode} {owner}  {size:>10}  {mtime}  {path}");
        } else {
            println!("{}", member.path);
        }
    }
    Ok(())
}

fn type_char_for(t: &EntryType) -> char {
    match t {
        EntryType::Dir => 'd',
        EntryType::Symlink => 'l',
        EntryType::HardLink => 'h',
        EntryType::CharDevice => 'c',
        EntryType::BlockDevice => 'b',
        EntryType::Fifo => 'p',
        EntryType::File | EntryType::Other => '-',
    }
}

fn format_path_with_link(member: &TocMember) -> String {
    match (&member.entry_type, &member.link_target) {
        (EntryType::Symlink | EntryType::HardLink, Some(target)) => {
            format!("{} -> {}", member.path, target)
        }
        _ => member.path.clone(),
    }
}

fn format_mode(type_char: char, mode: u32) -> String {
    let bits = [
        (0o400, 'r'),
        (0o200, 'w'),
        (0o100, 'x'),
        (0o040, 'r'),
        (0o020, 'w'),
        (0o010, 'x'),
        (0o004, 'r'),
        (0o002, 'w'),
        (0o001, 'x'),
    ];
    let mut s = String::with_capacity(10);
    s.push(type_char);
    for (bit, ch) in bits {
        s.push(if mode & bit != 0 { ch } else { '-' });
    }
    s
}

/// Formats a Unix timestamp as `YYYY-MM-DD HH:MM`. Renders in local time
/// (like `tar -tvf`) unless `utc` is set (like `tar --utc -tvf`).
fn format_mtime(mtime: i64, utc: bool) -> String {
    let shifted = if utc {
        mtime
    } else {
        mtime.saturating_add(local_offset_seconds(mtime))
    };

    // Format the (already offset-adjusted) timestamp as broken-down time
    // without pulling in a date library; valid for dates 1970-2106.
    if shifted < 0 {
        return "????-??-?? ??:??".to_owned();
    }
    let t = shifted as u64;
    let secs_per_min = 60u64;
    let secs_per_hour = 3600u64;
    let secs_per_day = 86400u64;

    let minute = (t / secs_per_min) % 60;
    let hour = (t / secs_per_hour) % 24;
    let mut days = t / secs_per_day;

    // Count years from 1970.
    let mut year = 1970u32;
    loop {
        let days_in_year = if is_leap(year) { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }
    let month_days: [u64; 12] = [
        31,
        if is_leap(year) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 1u32;
    for &md in &month_days {
        if days < md {
            break;
        }
        days -= md;
        month += 1;
    }
    let day = days + 1;

    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}")
}

fn is_leap(year: u32) -> bool {
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
}

/// Seconds east of UTC for the local timezone at instant `t`, consulting the
/// system tz database (so historical DST is honored), matching `tar`'s
/// `localtime`-based display. Returns 0 if the offset cannot be determined.
#[cfg(unix)]
fn local_offset_seconds(t: i64) -> i64 {
    // SAFETY: `tm` is zero-initialized and passed by mutable pointer for
    // `localtime_r` to fill; on success the struct is fully written.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        let tt = t as libc::time_t;
        if libc::localtime_r(&tt, &mut tm).is_null() {
            0
        } else {
            tm.tm_gmtoff as i64
        }
    }
}

#[cfg(not(unix))]
fn local_offset_seconds(_t: i64) -> i64 {
    0
}
