# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.0](https://github.com/astraw/tarzan-rs/compare/v0.2.2...v0.3.0) - 2026-05-25

### Added

- store content_md5 in TOC for S3 ETag interoperability (claude-sonnet-4-6)

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
