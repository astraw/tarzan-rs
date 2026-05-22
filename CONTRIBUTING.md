# Contributing to tarzan

Thanks for your interest in contributing! tarzan is a single Rust crate that
provides both the `tarzan` command-line tool and an embeddable library.

## Getting started

```sh
git clone https://github.com/astraw/tarzan-rs
cd tarzan-rs
cargo build
cargo test
```

The minimum supported Rust version is **1.87**. Don't use newer standard-library
or language features without bumping `rust-version` in `Cargo.toml`.

## Before you open a pull request

CI runs three checks, and a PR won't merge until they pass. Run them locally
first — they're the same commands the `CI` workflow uses:

```sh
cargo fmt --all --check                   # or `cargo fmt` to apply
cargo clippy --all-targets -- -D warnings  # warnings are treated as errors
cargo test
```

The integration tests under `tests/` shell out to the system `tar`, and some
also use `zstd` and `file(1)`. Make sure those are on your `PATH`, or a few
tests will fail or skip.

## Commit messages

Commits must follow the [Conventional Commits](https://www.conventionalcommits.org)
specification — release automation derives version bumps and the changelog
from them. Use a type prefix and an imperative summary:

```
feat: add --chunk-size flag to wrap
fix: handle empty archives in info
docs: clarify the TOC schema
```

Common types: `feat`, `fix`, `docs`, `refactor`, `test`, `chore`, `ci`.

## Code conventions

- **Formatting** — rustfmt defaults; run `cargo fmt`. Don't add a
  `rustfmt.toml` without discussing it first.
- **Warnings** — the build denies warnings (`-D warnings`); fix them all.
- **Logging** — use `tracing`, not `log`.
- **CLI** — use `clap` in derive mode.
- **String formatting** — capture arguments inline: `format!("{value}")`,
  not `format!("{}", value)`.
- **Lints** — prefer `#[expect(lint, reason = "…")]` over `#[allow(…)]`.
- **Dead code** — delete it; don't comment it out.

These are also recorded in [`AGENTS.md`](AGENTS.md) for AI-assisted tooling.

## Tests

New logic should come with tests: unit tests in a `#[cfg(test)]` module in the
same file, or integration tests under `tests/`. Archive-format changes should
keep the round-trip and standard-tool-interoperability tests passing — a tarzan
archive must always remain decompressible by plain `zstd` and `tar`.

## What to work on

The README's [Contributing](README.md#contributing) section lists areas of
particular interest. For anything substantial, opening an issue first to
discuss the approach is appreciated.

## License

tarzan is dual-licensed under [Apache-2.0](LICENSE-APACHE) or
[MIT](LICENSE-MIT), at the user's option. Unless you explicitly state
otherwise, any contribution you intentionally submit for inclusion in the work,
as defined in the Apache-2.0 license, shall be dual licensed as above, without
any additional terms or conditions.
