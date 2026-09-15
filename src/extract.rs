use std::collections::HashMap;
use std::fmt;
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
    /// Treat any entry in the [`FidelityReport`] as a failure: extraction
    /// still runs to completion, then returns a [`StrictFidelityError`]
    /// carrying the report. Defaults to false.
    pub strict: bool,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        Self {
            strip_components: 0,
            excludes: Vec::new(),
            includes: Vec::new(),
            restore_mtime: true,
            skip_bad_chunks: false,
            strict: false,
        }
    }
}

/// Why a member, or one piece of its metadata, was not restored.
///
/// The variants fall into two groups. *Declines* mean nothing was written at
/// the member's destination path: [`Symlink`](Self::Symlink) on a platform
/// that cannot create one, [`HardLink`](Self::HardLink) whose target is
/// missing, [`Device`](Self::Device), [`Fifo`](Self::Fifo),
/// [`Unsupported`](Self::Unsupported) entry types, [`BadData`](Self::BadData)
/// under `skip_bad_chunks`, [`NameCollision`](Self::NameCollision), and
/// [`InvalidName`](Self::InvalidName). *Metadata losses* mean the member was
/// written but one attribute could not be applied: [`Xattr`](Self::Xattr),
/// [`Mode`](Self::Mode), [`Mtime`](Self::Mtime).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum LossKind {
    /// An extended attribute could not be set.
    Xattr,
    /// Unix permission bits could not be applied.
    Mode,
    /// A timestamp could not be applied.
    Mtime,
    /// A symlink member could not be created.
    Symlink,
    /// A hard-link member could not be created.
    HardLink,
    /// A character or block device member was skipped.
    Device,
    /// A FIFO member was skipped.
    Fifo,
    /// An entry type tarzan does not materialise (sparse, vendor-specific)
    /// was skipped.
    Unsupported,
    /// A regular file's data could not be read and the member was skipped
    /// (`skip_bad_chunks`).
    BadData,
    /// The filesystem folded this member's name onto one already written by
    /// this extraction (case-insensitive or normalisation-insensitive
    /// filesystems); the member was skipped so the earlier one survives.
    NameCollision,
    /// The member's name cannot exist on this platform.
    InvalidName,
}

impl LossKind {
    /// Plural noun used in the summary block.
    pub fn label(self) -> &'static str {
        match self {
            LossKind::Xattr => "xattrs",
            LossKind::Mode => "permission bits",
            LossKind::Mtime => "timestamps",
            LossKind::Symlink => "symlinks",
            LossKind::HardLink => "hard links",
            LossKind::Device => "device nodes",
            LossKind::Fifo => "fifos",
            LossKind::Unsupported => "unsupported entries",
            LossKind::BadData => "members with unreadable data",
            LossKind::NameCollision => "name collisions",
            LossKind::InvalidName => "invalid names",
        }
    }

    /// True for kinds that mean the member was not written at all.
    pub fn is_decline(self) -> bool {
        !matches!(self, LossKind::Xattr | LossKind::Mode | LossKind::Mtime)
    }
}

/// One thing that did not make it from the archive onto the filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loss {
    /// Archive path of the affected member.
    pub path: String,
    /// What was lost.
    pub kind: LossKind,
    /// Free-form detail: the attribute name, the OS error, the raw type byte.
    pub detail: String,
}

/// What [`TarzanReader::extract_to_dir`] restored and what it could not.
///
/// Extraction guarantees content and attempts metadata; this is the record
/// of the difference. An empty report ([`is_clean`](Self::is_clean)) means
/// every selected member was written with every recorded attribute.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FidelityReport {
    /// Members whose destination entry was created.
    pub members_written: u64,
    /// Members for which nothing was written at the destination path.
    pub declined: Vec<Loss>,
    /// Attributes that could not be applied to members that were written.
    pub metadata_lost: Vec<Loss>,
}

/// How many affected paths the compact summary lists per kind before
/// collapsing the rest into "and N more".
const SUMMARY_EXAMPLES: usize = 5;

impl FidelityReport {
    /// True when nothing was declined and no metadata was lost.
    pub fn is_clean(&self) -> bool {
        self.declined.is_empty() && self.metadata_lost.is_empty()
    }

    /// Every loss, declines first.
    pub fn losses(&self) -> impl Iterator<Item = &Loss> {
        self.declined.iter().chain(self.metadata_lost.iter())
    }

    /// Multi-line summary grouped by kind, listing at most a few affected
    /// paths per kind. Empty string when the report is clean.
    pub fn summary(&self) -> String {
        self.render(Some(SUMMARY_EXAMPLES))
    }

    /// Like [`summary`](Self::summary) but lists every affected path.
    pub fn summary_full(&self) -> String {
        self.render(None)
    }

    fn render(&self, cap: Option<usize>) -> String {
        if self.is_clean() {
            return String::new();
        }
        let mut out = format!(
            "extracted {} member{}; {} not written, {} metadata item{} not restored:",
            self.members_written,
            plural(self.members_written),
            self.declined.len(),
            self.metadata_lost.len(),
            plural(self.metadata_lost.len() as u64),
        );
        let mut kinds: Vec<LossKind> = self.losses().map(|l| l.kind).collect();
        kinds.sort_unstable();
        kinds.dedup();
        for kind in kinds {
            let items: Vec<&Loss> = self.losses().filter(|l| l.kind == kind).collect();
            let shown = cap.map_or(items.len(), |c| c.min(items.len()));
            let mut examples: Vec<String> = items[..shown]
                .iter()
                .map(|l| {
                    if l.detail.is_empty() {
                        l.path.clone()
                    } else {
                        format!("{}: {}", l.path, l.detail)
                    }
                })
                .collect();
            if shown < items.len() {
                examples.push(format!("and {} more", items.len() - shown));
            }
            out.push_str(&format!(
                "\n  {}: {} ({})",
                kind.label(),
                items.len(),
                examples.join("; ")
            ));
        }
        out
    }

    fn decline(&mut self, member_path: &str, kind: LossKind, detail: impl fmt::Display) {
        let detail = detail.to_string();
        warn!(path = %member_path, kind = kind.label(), %detail, "member not written");
        self.declined.push(Loss {
            path: member_path.to_owned(),
            kind,
            detail,
        });
    }

    fn lose(&mut self, member_path: &str, kind: LossKind, detail: impl fmt::Display) {
        let detail = detail.to_string();
        warn!(path = %member_path, kind = kind.label(), %detail, "metadata not restored");
        self.metadata_lost.push(Loss {
            path: member_path.to_owned(),
            kind,
            detail,
        });
    }
}

fn plural(n: u64) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Returned by [`TarzanReader::extract_to_dir`] when
/// [`ExtractOptions::strict`] is set and the report is not clean. The tree
/// has still been extracted as far as possible; the error only changes the
/// verdict.
#[derive(Debug, Clone)]
pub struct StrictFidelityError(pub FidelityReport);

impl fmt::Display for StrictFidelityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}\n--strict: treating the above as failure",
            self.0.summary()
        )
    }
}

impl std::error::Error for StrictFidelityError {}

/// Filesystem actions deferred to a second pass after the main walk:
/// directory mtimes (children must be in place first) and hard links
/// (their target file must be extracted first).
#[derive(Default)]
struct Deferred {
    /// (member path for diagnostics, directory path, atime, mtime)
    dir_times: Vec<(String, PathBuf, FileTime, FileTime)>,
    /// (member path for diagnostics, link source, link target)
    hard_links: Vec<(String, PathBuf, PathBuf)>,
}

/// Files and symlinks created by this extraction, used to notice when the
/// filesystem folds two distinct archive names onto one entry (case- or
/// normalisation-insensitive filesystems). Directories are excluded: two
/// archive directories folding together simply merge. Hard links are
/// excluded: sharing an entry is their purpose.
///
/// The same archive path appearing twice is not a collision; tar semantics
/// are that the later entry wins, and we keep that.
#[derive(Default)]
struct WrittenNames {
    #[cfg(unix)]
    by_inode: HashMap<(u64, u64), String>,
    #[cfg(not(unix))]
    by_folded_path: HashMap<String, String>,
}

impl WrittenNames {
    /// If creating `target` (archive path `member_path`) would overwrite an
    /// entry this run created under a *different* archive path, returns that
    /// path.
    fn collides(&self, target: &Path, rel: &Path, member_path: &str) -> Option<&str> {
        let previous = self.lookup(target, rel)?;
        (previous != member_path).then_some(previous)
    }

    #[cfg(unix)]
    fn lookup(&self, target: &Path, _rel: &Path) -> Option<&str> {
        use std::os::unix::fs::MetadataExt;
        let meta = fs::symlink_metadata(target).ok()?;
        self.by_inode
            .get(&(meta.dev(), meta.ino()))
            .map(String::as_str)
    }

    #[cfg(unix)]
    fn record(&mut self, target: &Path, _rel: &Path, member_path: &str) {
        use std::os::unix::fs::MetadataExt;
        if let Ok(meta) = fs::symlink_metadata(target) {
            self.by_inode
                .insert((meta.dev(), meta.ino()), member_path.to_owned());
        }
    }

    /// Without a stable file identity API on this platform, approximate the
    /// filesystem's folding as case-insensitivity, which is what NTFS does.
    #[cfg(not(unix))]
    fn lookup(&self, _target: &Path, rel: &Path) -> Option<&str> {
        self.by_folded_path.get(&fold_path(rel)).map(String::as_str)
    }

    #[cfg(not(unix))]
    fn record(&mut self, _target: &Path, rel: &Path, member_path: &str) {
        self.by_folded_path
            .insert(fold_path(rel), member_path.to_owned());
    }
}

#[cfg(not(unix))]
fn fold_path(rel: &Path) -> String {
    rel.to_string_lossy().to_lowercase()
}

/// Returns why `rel` cannot be created on Windows, or `None` if it can.
///
/// Checked up front so the member is declined with a clear reason instead
/// of failing mid-extraction with an opaque OS error, and so a child of an
/// invalid directory name is declined for the same reason as the directory.
#[cfg(windows)]
fn platform_name_problem(member: &TocMember, rel: &Path) -> Option<String> {
    if member.path_bytes.is_some() {
        return Some("name is not valid UTF-8".to_owned());
    }
    for component in rel.components() {
        let s = component.as_os_str().to_string_lossy();
        if let Some(c) = s
            .chars()
            .find(|c| matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*') || (*c as u32) < 32)
        {
            return Some(format!("component {s:?} contains {c:?}"));
        }
        if s.ends_with('.') || s.ends_with(' ') {
            return Some(format!("component {s:?} ends with a dot or space"));
        }
        let stem = s.split('.').next().unwrap_or("").to_ascii_uppercase();
        let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (stem.len() == 4
                && (stem.starts_with("COM") || stem.starts_with("LPT"))
                && matches!(stem.as_bytes()[3], b'1'..=b'9'));
        if reserved {
            return Some(format!("component {s:?} is a reserved device name"));
        }
    }
    None
}

/// Unix filesystems accept any component without `/` or NUL, and tar cannot
/// encode either, so every name is valid here.
#[cfg(not(windows))]
fn platform_name_problem(_member: &TocMember, _rel: &Path) -> Option<String> {
    None
}

impl TarzanReader {
    /// Extracts archive members onto the filesystem under `dest`.
    ///
    /// Creates `dest` (and any missing parent directories) as needed.
    /// Refuses to extract members whose path is absolute or contains a
    /// `..` component, to keep the result inside `dest`; that refusal is
    /// an error, not a decline, because it is a safety boundary.
    ///
    /// # Contract
    ///
    /// Content is guaranteed, metadata is attempted, and the difference is
    /// reported. A regular file either lands byte-exact or (with
    /// `skip_bad_chunks`) is removed and declined; nothing is ever left at a
    /// declined member's path. Everything else that can fail for reasons
    /// outside the archive — an xattr namespace the host lacks, permission
    /// bits on a filesystem without them, a symlink on a platform that
    /// cannot create one — is recorded in the returned [`FidelityReport`]
    /// and extraction continues. Hard I/O failures (cannot create a
    /// directory or file, cannot write data) still abort with an error.
    ///
    /// # Order of application
    ///
    /// Per member: content, then extended attributes, then permission
    /// bits, then timestamps. xattrs precede mode because a read-only mode
    /// would forbid setting them. Hard links are created in a second pass
    /// once every regular file exists; directory timestamps are applied in
    /// a final pass, since creating children bumps a directory's mtime.
    ///
    /// `on_extracted` is invoked after each member is written, with the
    /// member's archive path. Useful for verbose progress output.
    pub fn extract_to_dir<F>(
        &mut self,
        dest: &Path,
        opts: &ExtractOptions,
        mut on_extracted: F,
    ) -> Result<FidelityReport>
    where
        F: FnMut(&str),
    {
        let includes = PathFilter::new(&opts.includes).context("invalid include/filter pattern")?;
        let excludes = compile_patterns(&opts.excludes).context("invalid exclude pattern")?;

        fs::create_dir_all(dest)
            .with_context(|| format!("creating destination {}", dest.display()))?;

        let mut deferred = Deferred::default();
        let mut report = FidelityReport::default();
        let mut written = WrittenNames::default();

        // Clone the member list so the loop can call `&mut self` methods
        // (extraction seeks the source) while iterating.
        let members = self.members().to_vec();
        for (index, member) in members.iter().enumerate() {
            if !includes.matches(&member.path) {
                continue;
            }
            if member_excluded(&member.path, &excludes) {
                continue;
            }
            let rel = match member_relative_path(member, opts.strip_components)? {
                Some(p) if !p.as_os_str().is_empty() => p,
                _ => continue,
            };
            let target = dest.join(&rel);
            if let Some(problem) = platform_name_problem(member, &rel) {
                report.decline(&member.path, LossKind::InvalidName, problem);
                continue;
            }
            if let Some(previous) = written.collides(&target, &rel, &member.path) {
                report.decline(
                    &member.path,
                    LossKind::NameCollision,
                    format!("this filesystem folds it onto {previous}, which was already written"),
                );
                continue;
            }
            if self.extract_one(
                index,
                member,
                &target,
                dest,
                opts,
                &mut deferred,
                &mut report,
            )? {
                report.members_written += 1;
                on_extracted(&member.path);
                if matches!(member.entry_type, EntryType::File | EntryType::Symlink) {
                    written.record(&target, &rel, &member.path);
                }
            }
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
                report.decline(
                    &member_path,
                    LossKind::HardLink,
                    format!("target {} was not extracted", source.display()),
                );
                continue;
            }
            // Replace any existing entry so hard_link does not fail with EEXIST.
            let _ = fs::remove_file(&target);
            match fs::hard_link(&source, &target) {
                Ok(()) => {
                    report.members_written += 1;
                    on_extracted(&member_path);
                }
                Err(error) => report.decline(
                    &member_path,
                    LossKind::HardLink,
                    format!("linking to {}: {error}", source.display()),
                ),
            }
        }

        // Directory mtimes last: writing children (files, subdirs, hard
        // links) bumps the parent's mtime back to "now".
        for (member_path, path, atime, mtime) in deferred.dir_times {
            if let Err(error) = filetime::set_file_times(&path, atime, mtime) {
                report.lose(&member_path, LossKind::Mtime, error);
            }
        }

        if opts.strict && !report.is_clean() {
            return Err(StrictFidelityError(report).into());
        }
        Ok(report)
    }

    /// Writes one member. Returns `Ok(true)` if an entry was created at
    /// `target`, `Ok(false)` if the member was declined (and recorded in
    /// `report`) or queued for the hard-link pass.
    #[expect(
        clippy::too_many_arguments,
        reason = "one call site; a struct would only rename the arguments"
    )]
    fn extract_one(
        &mut self,
        index: usize,
        member: &TocMember,
        target: &Path,
        dest: &Path,
        opts: &ExtractOptions,
        deferred: &mut Deferred,
        report: &mut FidelityReport,
    ) -> Result<bool> {
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        let mtime = member_mtime(member);
        let atime = member_atime(member, mtime);
        match member.entry_type {
            EntryType::Dir => {
                fs::create_dir_all(target)
                    .with_context(|| format!("creating dir {}", target.display()))?;
                apply_member_xattrs(target, member, report);
                apply_mode(target, member, report);
                if opts.restore_mtime {
                    deferred.dir_times.push((
                        member.path.clone(),
                        target.to_path_buf(),
                        atime,
                        mtime,
                    ));
                }
                Ok(true)
            }
            EntryType::File => {
                let file = File::create(target)
                    .with_context(|| format!("creating file {}", target.display()))?;
                let mut writer = BufWriter::new(file);
                // By index, not path: an archive may name the same path
                // twice, and tar semantics are that the later entry wins.
                match self.extract_member_at(index, &mut writer) {
                    Ok(()) => {
                        writer.flush()?;
                        apply_member_xattrs(target, member, report);
                        apply_mode(target, member, report);
                        if opts.restore_mtime
                            && let Err(error) = filetime::set_file_times(target, atime, mtime)
                        {
                            report.lose(&member.path, LossKind::Mtime, error);
                        }
                        Ok(true)
                    }
                    Err(err) if opts.skip_bad_chunks => {
                        // Drop the writer first so the partial file is closed
                        // before we remove it.
                        drop(writer);
                        let _ = fs::remove_file(target);
                        report.decline(&member.path, LossKind::BadData, format!("{err:#}"));
                        Ok(false)
                    }
                    Err(err) => Err(err),
                }
            }
            EntryType::Symlink => {
                // Replace any existing entry so symlink does not fail with EEXIST.
                let _ = fs::remove_file(target);
                match create_member_symlink(member, target) {
                    Ok(()) => {
                        if opts.restore_mtime
                            && let Err(error) =
                                filetime::set_symlink_file_times(target, atime, mtime)
                        {
                            report.lose(&member.path, LossKind::Mtime, error);
                        }
                        Ok(true)
                    }
                    Err(error) => {
                        report.decline(&member.path, LossKind::Symlink, format!("{error:#}"));
                        Ok(false)
                    }
                }
            }
            EntryType::HardLink => {
                // The link's target is another member, by archive path.
                // Defer creation until that file has been written; no
                // mtime fixup — a hard link shares the target's inode,
                // which already carries the right timestamp. Counted as
                // written when the second pass succeeds.
                match member_link_target_relative_path(member, opts.strip_components)? {
                    Some(src_rel) if !src_rel.as_os_str().is_empty() => {
                        deferred.hard_links.push((
                            member.path.clone(),
                            dest.join(src_rel),
                            target.to_path_buf(),
                        ));
                    }
                    _ => report.decline(
                        &member.path,
                        LossKind::HardLink,
                        "target path stripped away by --strip-components",
                    ),
                }
                Ok(false)
            }
            EntryType::CharDevice | EntryType::BlockDevice => {
                report.decline(&member.path, LossKind::Device, "not materialised");
                Ok(false)
            }
            EntryType::Fifo => {
                report.decline(&member.path, LossKind::Fifo, "not materialised");
                Ok(false)
            }
            EntryType::Other => {
                let detail = match member.raw_type_byte {
                    Some(raw) => format!("tar type '{}' (0x{raw:02x})", raw as char),
                    None => "unknown tar type".to_owned(),
                };
                report.decline(&member.path, LossKind::Unsupported, detail);
                Ok(false)
            }
        }
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

fn member_relative_path(member: &TocMember, strip: usize) -> Result<Option<PathBuf>> {
    #[cfg(unix)]
    if let Some(raw) = &member.path_bytes {
        return normalize_member_path_bytes(raw, strip);
    }
    normalize_member_path(&member.path, strip)
}

fn member_link_target_relative_path(member: &TocMember, strip: usize) -> Result<Option<PathBuf>> {
    #[cfg(unix)]
    if let Some(raw) = &member.link_target_bytes {
        return normalize_member_path_bytes(raw, strip);
    }
    let link_target = member
        .link_target
        .as_deref()
        .ok_or_else(|| anyhow!("hard link {} has no link_target", member.path))?;
    normalize_member_path(link_target, strip)
}

fn member_mtime(member: &TocMember) -> FileTime {
    FileTime::from_unix_time(member.mtime, member.mtime_ns.unwrap_or(0))
}

fn member_atime(member: &TocMember, fallback: FileTime) -> FileTime {
    match member.atime {
        Some(sec) => FileTime::from_unix_time(sec, member.atime_ns.unwrap_or(0)),
        None => fallback,
    }
}

/// Restores the member's recorded xattrs onto `target`, best effort.
///
/// Attributes are host-specific metadata: a `com.apple.*` name from a macOS
/// archive is not a valid namespace on Linux, macOS refuses to set some
/// system-managed attributes, and many filesystems do not support xattrs at
/// all. Each failure is recorded as a [`LossKind::Xattr`] loss and
/// extraction continues, matching what GNU tar and bsdtar do.
#[cfg(unix)]
fn apply_member_xattrs(target: &Path, member: &TocMember, report: &mut FidelityReport) {
    if let Some(xattrs) = &member.xattrs {
        for (name, value) in xattrs {
            if let Err(error) = xattr::set(target, name, value) {
                report.lose(&member.path, LossKind::Xattr, format!("{name}: {error}"));
            }
        }
    }
}

/// Windows has no xattrs. Recorded attributes are dropped without a
/// per-member report entry, since every member would carry the same note;
/// the README documents the limitation.
#[cfg(not(unix))]
fn apply_member_xattrs(_target: &Path, _member: &TocMember, _report: &mut FidelityReport) {}

/// Applies the member's Unix permission bits, recording a
/// [`LossKind::Mode`] loss on failure.
#[cfg(unix)]
fn apply_mode(target: &Path, member: &TocMember, report: &mut FidelityReport) {
    use std::os::unix::fs::PermissionsExt;
    // Mask to the standard 12 bits; ignore high bits that may encode entry type.
    let perms = fs::Permissions::from_mode(member.mode & 0o7777);
    if let Err(error) = fs::set_permissions(target, perms) {
        report.lose(&member.path, LossKind::Mode, error);
    }
}

/// Unix permission bits have no equivalent on this platform; not reported,
/// for the same reason as xattrs.
#[cfg(not(unix))]
fn apply_mode(_target: &Path, _member: &TocMember, _report: &mut FidelityReport) {}

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
fn normalize_member_path_bytes(raw: &[u8], strip: usize) -> Result<Option<PathBuf>> {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    if raw.starts_with(b"/") {
        bail!("absolute path in archive (refusing to extract)");
    }
    let mut parts: Vec<&[u8]> = Vec::new();
    for part in raw.split(|b| *b == b'/') {
        match part {
            b"" | b"." => continue,
            b".." => bail!("path contains `..` (refusing to extract)"),
            s => parts.push(s),
        }
    }
    if parts.len() <= strip {
        return Ok(None);
    }

    let mut path = PathBuf::new();
    for part in &parts[strip..] {
        path.push(OsStr::from_bytes(part));
    }
    Ok(Some(path))
}

#[cfg(unix)]
fn create_member_symlink(member: &TocMember, target: &Path) -> Result<()> {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    if let Some(raw) = &member.link_target_bytes {
        std::os::unix::fs::symlink(OsStr::from_bytes(raw), target)
            .with_context(|| format!("creating symlink {}", target.display()))?;
        return Ok(());
    }
    let link_target = member
        .link_target
        .as_deref()
        .ok_or_else(|| anyhow!("symlink {} has no link_target", member.path))?;
    std::os::unix::fs::symlink(link_target, target)
        .with_context(|| format!("creating symlink {}", target.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn create_member_symlink(_member: &TocMember, _target: &Path) -> Result<()> {
    bail!("symlinks are not supported on this platform")
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

    fn loss(path: &str, kind: LossKind, detail: &str) -> Loss {
        Loss {
            path: path.to_owned(),
            kind,
            detail: detail.to_owned(),
        }
    }

    #[test]
    fn clean_report_renders_empty() {
        let report = FidelityReport {
            members_written: 3,
            ..Default::default()
        };
        assert!(report.is_clean());
        assert_eq!(report.summary(), "");
    }

    #[test]
    fn summary_groups_by_kind_and_caps_examples() {
        let mut report = FidelityReport {
            members_written: 10,
            ..Default::default()
        };
        for i in 0..7 {
            report
                .metadata_lost
                .push(loss(&format!("./f{i}"), LossKind::Xattr, "user.k: EPERM"));
        }
        report
            .declined
            .push(loss("./dev/null", LossKind::Device, "not materialised"));

        let s = report.summary();
        assert!(
            s.starts_with("extracted 10 members; 1 not written, 7 metadata items not restored:"),
            "{s}"
        );
        assert!(
            s.contains("\n  device nodes: 1 (./dev/null: not materialised)"),
            "{s}"
        );
        assert!(s.contains("\n  xattrs: 7 ("), "{s}");
        assert!(s.contains("./f4: user.k: EPERM; and 2 more)"), "{s}");
        assert!(
            !s.contains("./f5"),
            "capped summary must not list ./f5: {s}"
        );

        let full = report.summary_full();
        assert!(full.contains("./f6"), "{full}");
        assert!(!full.contains("and 2 more"), "{full}");
    }

    #[test]
    fn strict_error_carries_summary() {
        let mut report = FidelityReport::default();
        report
            .declined
            .push(loss("./link", LossKind::Symlink, "not supported"));
        let err = StrictFidelityError(report);
        let text = err.to_string();
        assert!(
            text.contains("symlinks: 1 (./link: not supported)"),
            "{text}"
        );
        assert!(
            text.ends_with("--strict: treating the above as failure"),
            "{text}"
        );
    }
}
