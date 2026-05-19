use std::path::Path;

use anyhow::Result;
use tarzan::format::toc::EntryType;
use tarzan::TarzanReader;

pub fn run(archive: &Path, verbose: bool) -> Result<()> {
    let reader = TarzanReader::open(archive)?;
    for member in reader.members() {
        if verbose {
            let type_char = match member.entry_type {
                EntryType::Dir => 'd',
                EntryType::Symlink => 'l',
                EntryType::HardLink => 'h',
                EntryType::CharDevice => 'c',
                EntryType::BlockDevice => 'b',
                EntryType::Fifo => 'p',
                _ => '-',
            };
            let mode = format_mode(type_char, member.mode);
            let size = format_size(member.size);
            let mtime = format_mtime(member.mtime);
            println!("{mode}  {size:>10}  {mtime}  {}", member.path);
        } else {
            println!("{}", member.path);
        }
    }
    Ok(())
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

fn format_size(size: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    if size >= GB {
        format!("{:.1} GB", size as f64 / GB as f64)
    } else if size >= MB {
        format!("{:.1} MB", size as f64 / MB as f64)
    } else if size >= KB {
        format!("{:.1} KB", size as f64 / KB as f64)
    } else {
        format!("{size} B")
    }
}

fn format_mtime(mtime: i64) -> String {
    // Format Unix timestamp as YYYY-MM-DD HH:MM without pulling in a date library.
    // Uses a simple algorithm valid for dates 1970-2106.
    if mtime < 0 {
        return "????-??-?? ??:??".to_owned();
    }
    let t = mtime as u64;
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
