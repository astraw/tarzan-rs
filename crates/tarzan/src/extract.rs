use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use filetime::FileTime;
use glob::Pattern;
use tracing::warn;

use crate::filter::PathFilter;
use crate::format::toc::{EntryType, TocMember};
use crate::reader::TarzanReader;

/// Options controlling [`TarzanReader::extract_to_dir`].
#[derive(Debug, Default, Clone)]
pub struct ExtractOptions {
    /// Number of leading path components to drop from each member, like
    /// `tar --strip-components=N`. Members with too few components after
    /// the strip are skipped.
    pub strip_components: usize,
    /// Shell-glob patterns; matching members are skipped.
    pub excludes: Vec<String>,
    /// If non-empty, only members matching at least one pattern by exact
    /// path, directory-prefix, or shell-glob are extracted.
    pub includes: Vec<String>,
}

impl TarzanReader {
    /// Extracts archive members onto the filesystem under `dest`.
    ///
    /// Creates `dest` (and any missing parent directories) as needed.
    /// Refuses to extract members whose path is absolute or contains a
    /// `..` component, to keep the result inside `dest`.
    ///
    /// Hard links, character/block devices, and FIFOs are currently
    /// skipped with a warning.
    ///
    /// `on_extracted` is invoked after each member is successfully
    /// written, with the member's archive path. Useful for verbose
    /// progress output.
    pub fn extract_to_dir<F>(
        &self,
        dest: &Path,
        opts: &ExtractOptions,
        mut on_extracted: F,
    ) -> Result<()>
    where
        F: FnMut(&str),
    {
        let includes =
            PathFilter::new(&opts.includes).context("invalid include/filter pattern")?;
        let excludes = compile_patterns(&opts.excludes).context("invalid exclude pattern")?;

        fs::create_dir_all(dest)
            .with_context(|| format!("creating destination {}", dest.display()))?;

        // Directory mtimes have to be applied after all children are
        // written, because creating a child file or subdir bumps the
        // parent's mtime back to "now". Collect them here, apply at end.
        let mut deferred_dir_times: Vec<(PathBuf, FileTime)> = Vec::new();

        for member in self.members() {
            if !includes.matches(&member.path) {
                continue;
            }
            if member_excluded(&member.path, &excludes) {
                continue;
            }
            let rel = match normalize_member_path(&member.path, opts.strip_components)? {
                Some(p) if !p.as_os_str().is_empty() => p,
                _ => continue,
            };
            let target = dest.join(&rel);
            self.extract_one(member, &target, &mut deferred_dir_times)?;
            on_extracted(&member.path);
        }

        for (path, mtime) in deferred_dir_times {
            filetime::set_file_mtime(&path, mtime).with_context(|| {
                format!("setting mtime on directory {}", path.display())
            })?;
        }

        Ok(())
    }

    fn extract_one(
        &self,
        member: &TocMember,
        target: &Path,
        deferred_dir_times: &mut Vec<(PathBuf, FileTime)>,
    ) -> Result<()> {
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let mtime = FileTime::from_unix_time(member.mtime, 0);
        match member.entry_type {
            EntryType::Dir => {
                fs::create_dir_all(target)
                    .with_context(|| format!("creating dir {}", target.display()))?;
                set_unix_mode(target, member.mode)?;
                deferred_dir_times.push((target.to_path_buf(), mtime));
            }
            EntryType::File => {
                let file = File::create(target)
                    .with_context(|| format!("creating file {}", target.display()))?;
                let mut writer = BufWriter::new(file);
                self.extract_member(&member.path, &mut writer)?;
                writer.flush()?;
                set_unix_mode(target, member.mode)?;
                filetime::set_file_mtime(target, mtime)
                    .with_context(|| format!("setting mtime on {}", target.display()))?;
            }
            EntryType::Symlink => {
                let link_target = member
                    .link_target
                    .as_deref()
                    .ok_or_else(|| anyhow!("symlink {} has no link_target", member.path))?;
                create_symlink(link_target, target)?;
                // Use mtime for both atime and mtime; the TOC doesn't
                // record atime separately, and most filesystems don't
                // accurately preserve it anyway.
                filetime::set_symlink_file_times(target, mtime, mtime).with_context(|| {
                    format!("setting mtime on symlink {}", target.display())
                })?;
            }
            EntryType::HardLink => {
                warn!(path = %member.path, "skipping hard-link extraction (not yet supported)");
            }
            EntryType::CharDevice
            | EntryType::BlockDevice
            | EntryType::Fifo
            | EntryType::Other => {
                warn!(path = %member.path, "skipping unsupported entry type");
            }
        }
        Ok(())
    }
}

fn compile_patterns(raw: &[String]) -> Result<Vec<Pattern>> {
    raw.iter()
        .map(|s| {
            Pattern::new(normalize_for_match(s))
                .map_err(|e| anyhow!("invalid pattern `{s}`: {e}"))
        })
        .collect()
}

fn normalize_for_match(s: &str) -> &str {
    s.trim_start_matches("./").trim_end_matches('/')
}

fn member_excluded(path: &str, compiled: &[Pattern]) -> bool {
    let p = normalize_for_match(path);
    compiled.iter().any(|g| g.matches(p))
}

fn normalize_member_path(p: &str, strip: usize) -> Result<Option<PathBuf>> {
    if p.starts_with('/') {
        bail!("absolute path in archive (refusing to extract): {p}");
    }
    let mut parts: Vec<&str> = Vec::new();
    for part in p.split('/') {
        match part {
            "" | "." => continue,
            ".." => bail!("path contains `..` (refusing to extract): {p}"),
            s => parts.push(s),
        }
    }
    if parts.len() <= strip {
        return Ok(None);
    }
    Ok(Some(parts[strip..].iter().copied().collect()))
}

#[cfg(unix)]
fn set_unix_mode(target: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    // Mask to the standard 12 bits; ignore high bits that may encode entry type.
    let perms = fs::Permissions::from_mode(mode & 0o7777);
    fs::set_permissions(target, perms)
        .with_context(|| format!("setting mode on {}", target.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_unix_mode(_target: &Path, _mode: u32) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn create_symlink(link_target: &str, target: &Path) -> Result<()> {
    std::os::unix::fs::symlink(link_target, target)
        .with_context(|| format!("creating symlink {}", target.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn create_symlink(_link_target: &str, target: &Path) -> Result<()> {
    bail!("symlinks not supported on this platform ({})", target.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_rejects_absolute_path() {
        let err = normalize_member_path("/etc/passwd", 0).unwrap_err();
        assert!(err.to_string().contains("absolute"), "{err}");
    }

    #[test]
    fn normalize_rejects_dotdot_components() {
        let err = normalize_member_path("../escaped.txt", 0).unwrap_err();
        assert!(err.to_string().contains(".."), "{err}");

        let err = normalize_member_path("foo/../../bar", 0).unwrap_err();
        assert!(err.to_string().contains(".."), "{err}");
    }

    #[test]
    fn normalize_strips_dot_and_empty_components() {
        let p = normalize_member_path("./foo/./bar", 0).unwrap().unwrap();
        assert_eq!(p, PathBuf::from("foo/bar"));
    }

    #[test]
    fn normalize_applies_strip_components() {
        let p = normalize_member_path("./a/b/c.txt", 1).unwrap().unwrap();
        assert_eq!(p, PathBuf::from("b/c.txt"));

        let p = normalize_member_path("./a/b/c.txt", 2).unwrap().unwrap();
        assert_eq!(p, PathBuf::from("c.txt"));
    }

    #[test]
    fn normalize_skips_when_strip_consumes_all() {
        assert!(normalize_member_path("./a", 1).unwrap().is_none());
        assert!(normalize_member_path("./a/b", 2).unwrap().is_none());
        assert!(normalize_member_path("./a/b", 5).unwrap().is_none());
    }

    #[test]
    fn excludes_match_glob() {
        let raw = vec!["*.csv".to_owned()];
        let compiled = compile_patterns(&raw).unwrap();
        assert!(member_excluded("data/numbers.csv", &compiled));
        assert!(!member_excluded("data/blob.bin", &compiled));
    }
}
