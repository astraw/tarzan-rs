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
#[derive(Debug, Clone)]
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
    /// Restore each member's recorded mtime. When false, extracted
    /// entries keep whatever timestamp the filesystem assigns at
    /// creation. Defaults to true.
    pub restore_mtime: bool,
    /// If a regular-file member fails to extract because of a corrupted
    /// data chunk (zstd decode error, unexpected EOF mid-frame, …), log
    /// a warning and continue with the remaining members rather than
    /// aborting the whole extraction. Defaults to false.
    pub skip_bad_chunks: bool,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        Self {
            strip_components: 0,
            excludes: Vec::new(),
            includes: Vec::new(),
            restore_mtime: true,
            skip_bad_chunks: false,
        }
    }
}

/// Filesystem actions deferred to a second pass after the main walk:
/// directory mtimes (children must be in place first) and hard links
/// (their target file must be extracted first).
#[derive(Default)]
struct Deferred {
    /// (directory path, mtime to apply)
    dir_times: Vec<(PathBuf, FileTime)>,
    /// (member path for diagnostics, link source, link target)
    hard_links: Vec<(String, PathBuf, PathBuf)>,
}

impl TarzanReader {
    /// Extracts archive members onto the filesystem under `dest`.
    ///
    /// Creates `dest` (and any missing parent directories) as needed.
    /// Refuses to extract members whose path is absolute or contains a
    /// `..` component, to keep the result inside `dest`.
    ///
    /// Hard links are reconstructed once their target file is on disk.
    /// Character/block devices and FIFOs are skipped with a warning.
    ///
    /// `on_extracted` is invoked after each member is successfully
    /// written, with the member's archive path. Useful for verbose
    /// progress output.
    pub fn extract_to_dir<F>(
        &mut self,
        dest: &Path,
        opts: &ExtractOptions,
        mut on_extracted: F,
    ) -> Result<()>
    where
        F: FnMut(&str),
    {
        let includes = PathFilter::new(&opts.includes).context("invalid include/filter pattern")?;
        let excludes = compile_patterns(&opts.excludes).context("invalid exclude pattern")?;

        fs::create_dir_all(dest)
            .with_context(|| format!("creating destination {}", dest.display()))?;

        let mut deferred = Deferred::default();

        // Clone the member list so the loop can call `&mut self` methods
        // (extraction seeks the source) while iterating.
        let members = self.members().to_vec();
        for member in &members {
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
            self.extract_one(member, &target, dest, opts, &mut deferred)?;
            on_extracted(&member.path);
        }

        // Hard links: every regular file is on disk now, so their targets
        // resolve. Created before directory mtimes are stamped, since
        // adding a link bumps the containing directory's mtime.
        for (member_path, source, target) in deferred.hard_links {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            if !source.exists() {
                warn!(
                    path = %member_path,
                    source = %source.display(),
                    "hard-link target was not extracted; skipping"
                );
                continue;
            }
            // Replace any existing entry so hard_link does not fail with EEXIST.
            let _ = fs::remove_file(&target);
            fs::hard_link(&source, &target).with_context(|| {
                format!(
                    "creating hard link {} -> {}",
                    target.display(),
                    source.display()
                )
            })?;
        }

        // Directory mtimes last: writing children (files, subdirs, hard
        // links) bumps the parent's mtime back to "now".
        for (path, mtime) in deferred.dir_times {
            filetime::set_file_mtime(&path, mtime)
                .with_context(|| format!("setting mtime on directory {}", path.display()))?;
        }

        Ok(())
    }

    fn extract_one(
        &mut self,
        member: &TocMember,
        target: &Path,
        dest: &Path,
        opts: &ExtractOptions,
        deferred: &mut Deferred,
    ) -> Result<()> {
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        let mtime = FileTime::from_unix_time(member.mtime, 0);
        match member.entry_type {
            EntryType::Dir => {
                fs::create_dir_all(target)
                    .with_context(|| format!("creating dir {}", target.display()))?;
                set_unix_mode(target, member.mode)?;
                if opts.restore_mtime {
                    deferred.dir_times.push((target.to_path_buf(), mtime));
                }
            }
            EntryType::File => {
                let file = File::create(target)
                    .with_context(|| format!("creating file {}", target.display()))?;
                let mut writer = BufWriter::new(file);
                match self.extract_member(&member.path, &mut writer) {
                    Ok(()) => {
                        writer.flush()?;
                        set_unix_mode(target, member.mode)?;
                        if opts.restore_mtime {
                            filetime::set_file_mtime(target, mtime).with_context(|| {
                                format!("setting mtime on {}", target.display())
                            })?;
                        }
                    }
                    Err(err) if opts.skip_bad_chunks => {
                        // Drop the writer first so the partial file is closed
                        // before we remove it.
                        drop(writer);
                        let _ = fs::remove_file(target);
                        warn!(
                            path = %member.path,
                            error = format!("{err:#}"),
                            "skipping member with unreadable data (--skip-bad-chunks)"
                        );
                    }
                    Err(err) => return Err(err),
                }
            }
            EntryType::Symlink => {
                let link_target = member
                    .link_target
                    .as_deref()
                    .ok_or_else(|| anyhow!("symlink {} has no link_target", member.path))?;
                create_symlink(link_target, target)?;
                if opts.restore_mtime {
                    // Use mtime for both atime and mtime; the TOC doesn't
                    // record atime separately, and most filesystems don't
                    // accurately preserve it anyway.
                    filetime::set_symlink_file_times(target, mtime, mtime).with_context(|| {
                        format!("setting mtime on symlink {}", target.display())
                    })?;
                }
            }
            EntryType::HardLink => {
                // The link's target is another member, by archive path.
                // Defer creation until that file has been written; no
                // mtime fixup — a hard link shares the target's inode,
                // which already carries the right timestamp.
                let link_target = member
                    .link_target
                    .as_deref()
                    .ok_or_else(|| anyhow!("hard link {} has no link_target", member.path))?;
                match normalize_member_path(link_target, opts.strip_components)? {
                    Some(src_rel) if !src_rel.as_os_str().is_empty() => {
                        deferred.hard_links.push((
                            member.path.clone(),
                            dest.join(src_rel),
                            target.to_path_buf(),
                        ));
                    }
                    _ => warn!(
                        path = %member.path,
                        "hard-link target stripped away; skipping"
                    ),
                }
            }
            EntryType::CharDevice | EntryType::BlockDevice | EntryType::Fifo | EntryType::Other => {
                warn!(path = %member.path, "skipping unsupported entry type");
            }
        }
        Ok(())
    }
}

fn compile_patterns(raw: &[String]) -> Result<Vec<Pattern>> {
    raw.iter()
        .map(|s| {
            Pattern::new(normalize_for_match(s)).map_err(|e| anyhow!("invalid pattern `{s}`: {e}"))
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
    bail!(
        "symlinks not supported on this platform ({})",
        target.display()
    )
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
