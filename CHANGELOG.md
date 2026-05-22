# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1](https://github.com/astraw/tarzan-rs/compare/v0.1.0...v0.1.1) - 2026-05-22

### Other

- release v0.1.0

## [0.1.0](https://github.com/astraw/tarzan-rs/releases/tag/v0.1.0) - 2026-05-22

### Added

- *(reader)* open archives from any seekable source (Opus 4.7)
- *(list)* show -v timestamps in local time with --utc override (Opus 4.7)
- pack small members into shared frames (Opus 4.7)
- stream wrap to bound memory and split large members (Opus 4.7)
- add file magic pattern for tarzan archives (gemma-4-31b)
- merge tarzan and tarzan-cli into a single crate (gemma-4-31b)
- extract hard links, add --no-mtime and info --json (Opus 4.7)
- add list path filter and extract mtime restoration (Opus 4.7)
- *(list)* add owner column, link targets, and --json (Opus 4.7)
- *(info)* add info subcommand (Opus 4.7)
- *(extract)* add extract subcommand (Opus 4.7)
- *(cli)* add wrap -v, verify -v, and TTY guard (Opus 4.7)
- *(cli)* align flags with tar conventions (Opus 4.7)
- implement per-chunk SHA-256 and per-file extraction (Sonnet 4.6)
- per-member chunking and edge-case shape tests (Sonnet 4.6)
- implement TOC frame, tarzan list, and reader (Sonnet 4.6)
- add identity skippable frame write path (GPT-5.3-Codex)
- bootstrap tarzan wrap MVP and tests (GPT-5.3-Codex)

### Other

- add cargo-dist release workflow (Opus 4.7)
- note Windows build caveats in the README (Opus 4.7)
- *(deps)* gate libc to unix targets (Opus 4.7)
- switch release-plz to tag-only, deferring releases to cargo-dist (Opus 4.7)
- add GitHub Actions workflow for fmt, clippy, and tests (Opus 4.7)
- raise declared MSRV to 1.87 (Opus 4.7)
- apply cargo fmt (Opus 4.7)
- *(reader)* take &mut self instead of interior mutability (Opus 4.7)
- cover timestamp display, wrap -v, corruption, and file magic (Opus 4.7)
- fix install instructions, repair file magic, and test README examples (Opus 4.7)
- update AGENTS.md to use conventional commits
- wire up release-plz and crates.io publishing metadata (Opus 4.7)
- note zstd|tar fallback for extract/cat edge cases (Opus 4.7)
- document tar | wrap as the archive-creation workflow (Opus 4.7)
- document some design decisions
- better justify magic number
- initial readme
