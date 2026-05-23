# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
