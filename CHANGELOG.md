# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.0](https://github.com/astraw/tarzan-rs/compare/v0.4.0...v0.5.0) - 2026-09-16

### Added

- require the TOC's tarzan_version to match the identity frame (Claude Fable 5.1)
- [**breaking**] report what extract could not restore; add --strict (Claude Fable 5.1)
- [**breaking**] sync wrapped archives by default; add --no-sync (Claude Fable 5.1)
- [**breaking**] bump zstd to 0.14 and drop the pure-rust feature (Claude Fable 5.1)
- make wrap checksums configurable and document policy (GPT-5.3-Codex)

### Changed

- changed! made WrapOptions fields private

### Fixed

- accept negative PAX mtimes; decline names the filesystem rejects (Claude Fable 5.1)
- decline names Windows cannot create instead of failing mid-extract (Claude Fable 5.1)
- decline a member whose name the filesystem folds onto one already written (Claude Fable 5.1)
- warn instead of abort when an xattr cannot be restored (Claude Fable 5.1)
- say XXHash64, not SHA-256, in the verify --quick help (Claude Fable 5.1)
- pin tar-core to a hardened git revision (Claude Fable 5.1)
- parse extended tar metadata with tar-core (GPT-5.4)

### Other

- spell the owner flags for GNU tar in the old-reader probe (Claude Fable 5.1)
- state the compatibility contract and sync crate docs with README (Claude Fable 5.1)
- probe forward compatibility without promising it (Claude Fable 5.1)
- write tar names as bytes so host path rules do not apply (Claude Fable 5.1)
- move artifact and python actions to their Node 24 majors (Claude Fable 5.1)
- produce archives on every OS and consume them on every OS (Claude Fable 5.1)
- hostile-but-valid tar corpus (Claude Fable 5.1)
- pin list/info --json output with golden files (Claude Fable 5.1)
- scan committed fixtures for host-specific facts (Claude Fable 5.1)
- state the extract contract in the README (Claude Fable 5.1)
- force a PAX header on the sub-second mtime fixture (Claude Fable 5.1)
- make the compatibility fixtures host-neutral (Claude Fable 5.1)
- describe the Windows job and drop the "untested" wording (Claude Fable 5.1)
- keep archives from every published release readable (Claude Fable 5.1)
- describe the zstd and tar compatibility contract and fix README drift (Claude Fable 5.1)
- update Cargo.lock for zstd 0.14 (Claude Fable 5.1)
- exercise host bsdtar default output on macOS (Claude Fable 5.1)
- update Cargo.lock for the tar-core git pin (Claude Fable 5.1)
- *(deps)* update parser dependency lockfile (GPT-5.4)
- update Cargo.lock
- update README
- trivial refactoring
- fix three errors in lib.rs and reader.rs docstrings (claude-sonnet-4-6)
- fix three errors in --json field description (claude-sonnet-4-6)
- mention MD5 sums in README JSON TOC section
- add badges to readme
- update dependencies

## [0.4.0](https://github.com/astraw/tarzan-rs/compare/v0.3.0...v0.4.0) - 2026-05-27

### Added

- support wrapping GNU sparse tar entries (Opus 4.7)
- added tarzan wrap --sync (GPT-5.5-Codex)

### Fixed

- [**breaking**] close TOC correctness gaps in wrap/TOC/readers while keeping schema at v2 (Opus 4.7 and GPT-5.3-Codex)
- gnu tar sparse files (Opus 4.7)
- various save-path issues found during adversarial wrap audit (GPT-5.5-Codex)
- found and fixed one real corruption-detection bug (GPT-5.5-Codex)

### Other

- automatically catch semver bumps
- added gnu tar sparse regression test (GPT-5.5-Codex)
- update Cargo.lock
- fix 'cargo semver-checks check-release' call

## [0.3.0](https://github.com/astraw/tarzan-rs/compare/v0.2.2...v0.3.0) - 2026-05-26

### Added

- add pure-rust feature for C-free zstd via zstd-pure-rs (claude-sonnet-4-6 and GPT-5.5-Codex)
- show content_sha256 and content_md5 presence in tarzan info (claude-sonnet-4-6)
- [**breaking**] document content_md5 in integrity-layers prose (claude-sonnet-4-6)
- store content_md5 in TOC for S3 ETag interoperability (claude-sonnet-4-6)

### Fixed

- correct feat-to-patch bump config, use release-plz native option not cliff.toml (claude-sonnet-4-6)

### Other

- disclose AI-assisted development in README and lib.rs (claude-sonnet-4-6)
- [**breaking**] add semver-compatibility check (claude-sonnet-4-6)
- configure git-cliff so feat: bumps patch not minor (pre-1.0 policy) (claude-sonnet-4-6)

## [0.2.2](https://github.com/astraw/tarzan-rs/compare/v0.2.1...v0.2.2) - 2026-05-24

### Fixed

- honour PAX size= overrides when wrapping (claude-opus-4-7)

### Other

- add explicit pre-commit checklist to AGENTS.md (claude-opus-4-7)

## [0.2.1](https://github.com/astraw/tarzan-rs/compare/v0.2.0...v0.2.1) - 2026-05-24

### Fixed

- cap wrap window buffer (claude-opus-4-7)

### Other

- write TOC frame directly to save memory (claude-opus-4-7)
- simplify v1 legacy format error message
- release v0.2.0

## [0.2.0](https://github.com/astraw/tarzan-rs/compare/v0.1.2...v0.2.0) - 2026-05-24

### Added

- [**breaking**] tarzan v2 format — TOC offset footer, per-member SHA-256, XXHash64 (claude-opus-4-7)

### Other

- *(deps)* drop tar's default xattr feature; regen THIRD-PARTY-LICENSES (claude-opus-4-7)
- gate local-time list rendering as unix-only (claude-opus-4-7)
- run on every branch, not just main (claude-opus-4-7)
- run tests on Linux, macOS, and Windows; pin testdata to LF (claude-opus-4-7)

## [0.1.2](https://github.com/astraw/tarzan-rs/compare/v0.1.1...v0.1.2) - 2026-05-23

### Fixed

- *(reader)* grow TOC scan window adaptively for large archives (claude-opus-4-7)

### Other

- clarify release process and PAT requirement in README (claude-sonnet-4-6)

## [0.1.1](https://github.com/astraw/tarzan-rs/compare/v0.1.0...v0.1.1) - 2026-05-23

### Fixed

- handle broken pipe cleanly when stdout is closed early (claude-sonnet-4-6)

### Other

- split README and lib.rs module docstring (claude-sonnet-4-6)
- add xxd archive check and release process to README (claude-sonnet-4-6)
- suppress macOS AppleDouble metadata files in tar fixtures (claude-sonnet-4-6)
- fix file_magic_identifies_tarzan_archive on macOS (claude-sonnet-4-6)
- verify THIRD-PARTY-LICENSES keeps the libzstd entry (Opus 4.7)
- bundle third-party license notices into release archives (Opus 4.7)
- dual-license under MIT OR Apache-2.0 (Opus 4.7)
- add CONTRIBUTING.md (Opus 4.7)
- bump actions/checkout to v6 in release-plz workflow (Opus 4.7)
